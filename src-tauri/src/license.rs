//! Murmela Pro license keys, sold through Lemon Squeezy.
//!
//! A key is activated once against the Lemon Squeezy License API, which needs no API
//! secret. After that the activation lives only in the local database and is never
//! re-checked, so Pro keeps working offline and without our servers. The only other
//! request is the optional "Deactivate this Mac", which frees one of the key's slots.
//! Both are logged in Settings → Network activity, without the key.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::network_activity::{self, Destination, Outcome};
use crate::storage::open_connection_by_path;

/// Lemon Squeezy store that sells Murmela. Keys from any other store are refused.
const STORE_ID: u64 = 497381;
/// Products whose keys unlock Pro: "Murmela Pro – Founders Lifetime License".
const PRODUCT_IDS: &[u64] = &[1439649];

const API_BASE: &str = "https://api.lemonsqueezy.com/v1/licenses";
const SUPPORT_EMAIL: &str = "hello@murmela.com";
/// Prefix that the frontend recognises to show a "Pro feature" hint instead of an error.
pub const PRO_REQUIRED: &str = "PRO_REQUIRED";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseStatus {
    pub pro: bool,
    /// Last characters of the key, enough to recognise it in a receipt.
    pub key_hint: Option<String>,
    pub licensed_to: Option<String>,
    pub activated_at: Option<String>,
}

impl LicenseStatus {
    fn free() -> Self {
        Self {
            pro: false,
            key_hint: None,
            licensed_to: None,
            activated_at: None,
        }
    }
}

#[derive(Debug, Clone)]
struct Activation {
    license_key: String,
    instance_id: String,
    customer_name: Option<String>,
    customer_email: Option<String>,
    activated_at: String,
}

impl Activation {
    fn status(&self) -> LicenseStatus {
        LicenseStatus {
            pro: true,
            key_hint: Some(key_hint(&self.license_key)),
            licensed_to: self
                .customer_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .or_else(|| self.customer_email.clone()),
            activated_at: Some(self.activated_at.clone()),
        }
    }
}

pub fn ensure_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS license_activation (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            license_key TEXT NOT NULL,
            instance_id TEXT NOT NULL,
            instance_name TEXT NOT NULL,
            customer_name TEXT,
            customer_email TEXT,
            product_id INTEGER,
            activated_at TEXT NOT NULL
        );",
    )?;
    Ok(())
}

fn open(db_path: &Path) -> Result<Connection> {
    let connection = open_connection_by_path(db_path)?;
    ensure_schema(&connection)?;
    Ok(connection)
}

fn load(db_path: &Path) -> Result<Option<Activation>> {
    Ok(open(db_path)?
        .query_row(
            "SELECT license_key, instance_id, customer_name, customer_email, activated_at
             FROM license_activation WHERE id = 1",
            [],
            |row| {
                Ok(Activation {
                    license_key: row.get(0)?,
                    instance_id: row.get(1)?,
                    customer_name: row.get(2)?,
                    customer_email: row.get(3)?,
                    activated_at: row.get(4)?,
                })
            },
        )
        .optional()?)
}

pub fn status(db_path: &Path) -> Result<LicenseStatus> {
    Ok(load(db_path)?
        .map(|activation| activation.status())
        .unwrap_or_else(LicenseStatus::free))
}

/// True when this Mac has an activated Pro license. A database error counts as Free.
pub fn is_pro(db_path: &Path) -> bool {
    matches!(load(db_path), Ok(Some(_)))
}

/// Fails with a `PRO_REQUIRED:` error unless Pro is active. `feature` reads mid-sentence, e.g. "speaker labels".
pub fn require_pro(db_path: &Path, feature: &str) -> Result<()> {
    if is_pro(db_path) {
        return Ok(());
    }
    bail!("{PRO_REQUIRED}: Murmela Pro unlocks {feature}. Add your license key in Settings → License.")
}

/// The key from an activation link such as `murmela://activate?key=…`, sent to buyers by email.
pub fn key_from_link(link: &str) -> Option<String> {
    let url = tauri::Url::parse(link).ok()?;
    if url.scheme() != "murmela" || url.host_str() != Some("activate") {
        return None;
    }
    let key = url
        .query_pairs()
        .find(|(name, _)| name == "key")
        .map(|(_, value)| value.trim().to_string())?;
    let plausible = (8..=100).contains(&key.len())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    plausible.then_some(key)
}

/// Activation links that arrived before the window could handle them, e.g. the link that launched the app.
#[derive(Default)]
pub struct PendingLinks(std::sync::Mutex<Option<String>>);

impl PendingLinks {
    pub fn offer(&self, key: String) {
        if let Ok(mut pending) = self.0.lock() {
            *pending = Some(key);
        }
    }

    pub fn take(&self) -> Option<String> {
        self.0.lock().ok()?.take()
    }
}

pub fn activate(db_path: &Path, license_key: &str) -> Result<LicenseStatus> {
    activate_with(&HttpLicenseApi::new(db_path)?, db_path, license_key)
}

pub fn deactivate(db_path: &Path) -> Result<LicenseStatus> {
    deactivate_with(&HttpLicenseApi::new(db_path)?, db_path)
}

fn activate_with(api: &dyn LicenseApi, db_path: &Path, license_key: &str) -> Result<LicenseStatus> {
    let license_key = license_key.trim();
    if license_key.is_empty() {
        bail!("Paste the license key from your receipt email.");
    }
    if let Some(existing) = load(db_path)? {
        if existing.license_key.eq_ignore_ascii_case(license_key) {
            return Ok(existing.status());
        }
        bail!("This Mac already has a license. Deactivate it first to use a different key.");
    }

    let instance_name = format!("Mac {}", &Uuid::new_v4().simple().to_string()[..6]);
    let (code, body) = api
        .post(
            "activate",
            &[("license_key", license_key), ("instance_name", &instance_name)],
        )
        .map_err(|_| {
            anyhow!("Couldn't reach the license server. Activation needs the internet once; after that Murmela Pro works offline.")
        })?;
    let response: ApiResponse = serde_json::from_str(&body)
        .with_context(|| format!("Unexpected answer from the license server (HTTP {code})."))?;

    if response.activated != Some(true) {
        bail!(activation_error(code, &response));
    }
    let instance_id = response
        .instance
        .as_ref()
        .map(|instance| instance.id.clone())
        .ok_or_else(|| anyhow!("The license server didn't return an activation. Please try again."))?;
    let meta = response.meta.unwrap_or_default();
    if meta.store_id != Some(STORE_ID) || !meta.product_id.is_some_and(|id| PRODUCT_IDS.contains(&id)) {
        // Free the slot we just used on someone else's product before refusing it.
        let _ = api.post(
            "deactivate",
            &[("license_key", license_key), ("instance_id", &instance_id)],
        );
        bail!("This license key isn't for Murmela Pro. Use the key from your Murmela receipt.");
    }

    let activation = Activation {
        license_key: license_key.to_string(),
        instance_id,
        customer_name: meta.customer_name,
        customer_email: meta.customer_email,
        activated_at: Utc::now().to_rfc3339(),
    };
    open(db_path)?.execute(
        "INSERT OR REPLACE INTO license_activation
            (id, license_key, instance_id, instance_name, customer_name, customer_email, product_id, activated_at)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            activation.license_key,
            activation.instance_id,
            instance_name,
            activation.customer_name,
            activation.customer_email,
            meta.product_id.map(|id| id as i64),
            activation.activated_at,
        ],
    )?;
    Ok(activation.status())
}

fn deactivate_with(api: &dyn LicenseApi, db_path: &Path) -> Result<LicenseStatus> {
    let Some(activation) = load(db_path)? else {
        return Ok(LicenseStatus::free());
    };
    let (code, body) = api
        .post(
            "deactivate",
            &[
                ("license_key", activation.license_key.as_str()),
                ("instance_id", activation.instance_id.as_str()),
            ],
        )
        .map_err(|_| {
            anyhow!("Couldn't reach the license server, so this Mac's slot wasn't freed. Try again when you're online.")
        })?;
    let response: ApiResponse = serde_json::from_str(&body).unwrap_or_default();
    // 404: the activation was already removed, e.g. reset in the Lemon Squeezy dashboard.
    if response.deactivated != Some(true) && code != 404 {
        bail!(
            "The license server couldn't deactivate this Mac{}. Write to {SUPPORT_EMAIL} and we'll free the slot.",
            response.error.map(|error| format!(" ({error})")).unwrap_or_default()
        );
    }
    open(db_path)?.execute("DELETE FROM license_activation WHERE id = 1", [])?;
    Ok(LicenseStatus::free())
}

fn activation_error(code: u16, response: &ApiResponse) -> String {
    let error = response.error.clone().unwrap_or_default();
    let lower = error.to_lowercase();
    let status = response
        .license_key
        .as_ref()
        .and_then(|key| key.status.clone())
        .unwrap_or_default();
    if lower.contains("activation limit") {
        format!("This key is already active on its maximum number of Macs. Deactivate it on a Mac you no longer use (Settings → License), or write to {SUPPORT_EMAIL} and we'll reset it.")
    } else if status == "disabled" || lower.contains("disabled") {
        format!("This license key has been disabled, for example after a refund. If that's a mistake, write to {SUPPORT_EMAIL}.")
    } else if status == "expired" || lower.contains("expired") {
        format!("This license key has expired. Write to {SUPPORT_EMAIL} if you think that's wrong.")
    } else if code == 404 || lower.contains("not found") || lower.contains("invalid") {
        "We couldn't find this license key. Copy the whole key from your receipt email and try again.".to_string()
    } else if error.is_empty() {
        format!("The license server refused the key (HTTP {code}). Please try again or write to {SUPPORT_EMAIL}.")
    } else {
        format!("The license server said: {error}")
    }
}

fn key_hint(license_key: &str) -> String {
    let tail: String = license_key
        .chars()
        .rev()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("••••{}", tail.to_uppercase())
}

#[derive(Debug, Default, Deserialize)]
struct ApiResponse {
    activated: Option<bool>,
    deactivated: Option<bool>,
    error: Option<String>,
    license_key: Option<ApiLicenseKey>,
    instance: Option<ApiInstance>,
    meta: Option<ApiMeta>,
}

#[derive(Debug, Default, Deserialize)]
struct ApiLicenseKey {
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiInstance {
    id: String,
}

#[derive(Debug, Default, Deserialize)]
struct ApiMeta {
    store_id: Option<u64>,
    product_id: Option<u64>,
    customer_name: Option<String>,
    customer_email: Option<String>,
}

/// POSTs a form to the License API and returns the HTTP status and body, also for 4xx answers.
trait LicenseApi {
    fn post(&self, endpoint: &str, form: &[(&str, &str)]) -> Result<(u16, String)>;
}

struct HttpLicenseApi {
    client: reqwest::blocking::Client,
    /// Where each request is logged for Settings → Network activity.
    db_path: PathBuf,
}

impl HttpLicenseApi {
    fn new(db_path: &Path) -> Result<Self> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(20))
                .user_agent("Murmela")
                .build()?,
            db_path: db_path.to_path_buf(),
        })
    }
}

impl LicenseApi for HttpLicenseApi {
    fn post(&self, endpoint: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
        let purpose = match endpoint {
            "activate" => "Activate Murmela Pro",
            "deactivate" => "Deactivate Murmela Pro on this Mac",
            other => other,
        };
        let mut connection =
            network_activity::Connection::begin(&self.db_path, Destination::LemonSqueezy, purpose);
        let request = self
            .client
            .post(format!("{API_BASE}/{endpoint}"))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(form_encode(form));
        let response = connection.send(&self.client, request)?;
        let code = response.status().as_u16();
        let body = response.text()?;
        connection.add_bytes(body.len() as u64);
        connection.finish(Outcome::Completed);
        Ok((code, body))
    }
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    fn encode(value: &str) -> String {
        value
            .bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (byte as char).to_string()
                }
                b' ' => "+".to_string(),
                _ => format!("%{byte:02X}"),
            })
            .collect()
    }
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    struct FakeApi {
        answers: RefCell<Vec<Result<(u16, String)>>>,
        calls: RefCell<Vec<(String, String)>>,
    }

    impl FakeApi {
        fn new(answers: Vec<Result<(u16, String)>>) -> Self {
            Self {
                answers: RefCell::new(answers),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl LicenseApi for FakeApi {
        fn post(&self, endpoint: &str, form: &[(&str, &str)]) -> Result<(u16, String)> {
            self.calls
                .borrow_mut()
                .push((endpoint.to_string(), form_encode(form)));
            self.answers.borrow_mut().remove(0)
        }
    }

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("murmela-license-{}.sqlite", Uuid::new_v4()))
    }

    fn activated(store_id: u64, product_id: u64) -> Result<(u16, String)> {
        Ok((
            200,
            serde_json::json!({
                "activated": true, "error": null,
                "license_key": { "status": "active", "activation_limit": 3, "activation_usage": 1 },
                "instance": { "id": "47596ad9-a811-4ebf-ac8a-03fc7b6d2a17", "name": "Mac abc123" },
                "meta": { "store_id": store_id, "product_id": product_id,
                          "customer_name": "Ada Lovelace", "customer_email": "ada@example.com" }
            })
            .to_string(),
        ))
    }

    #[test]
    fn activation_unlocks_pro_and_survives_without_network() {
        let db = temp_db();
        assert!(!is_pro(&db));
        let api = FakeApi::new(vec![activated(STORE_ID, PRODUCT_IDS[0])]);
        let status = activate_with(&api, &db, "  38B1460A-5104-4067-A91D-77B872934D51 \n").unwrap();
        assert!(status.pro);
        assert_eq!(status.key_hint.as_deref(), Some("••••4D51"));
        assert_eq!(status.licensed_to.as_deref(), Some("Ada Lovelace"));
        assert!(is_pro(&db));
        assert!(require_pro(&db, "speaker labels").is_ok());
        let (endpoint, form) = &api.calls.borrow()[0];
        assert_eq!(endpoint, "activate");
        assert!(form.starts_with("license_key=38B1460A-5104-4067-A91D-77B872934D51&instance_name=Mac+"));
        // Asking again with the same key answers locally, without another request.
        let offline = FakeApi::new(vec![]);
        assert!(activate_with(&offline, &db, "38b1460a-5104-4067-a91d-77b872934d51").unwrap().pro);
        assert!(offline.calls.borrow().is_empty());
    }

    #[test]
    fn keys_for_other_products_are_refused_and_their_slot_freed() {
        let db = temp_db();
        let api = FakeApi::new(vec![
            activated(STORE_ID + 1, 999_999),
            Ok((200, r#"{"deactivated":true,"error":null}"#.to_string())),
        ]);
        let error = activate_with(&api, &db, "some-other-key").unwrap_err().to_string();
        assert!(error.contains("isn't for Murmela Pro"), "{error}");
        assert_eq!(api.calls.borrow()[1].0, "deactivate");
        assert!(!is_pro(&db));
    }

    #[test]
    fn api_errors_become_plain_messages() {
        let db = temp_db();
        let limit = r#"{"activated":false,"error":"This license key has reached the activation limit.","license_key":{"status":"active"},"meta":{}}"#;
        let api = FakeApi::new(vec![Ok((400, limit.to_string()))]);
        let error = activate_with(&api, &db, "key").unwrap_err().to_string();
        assert!(error.contains("maximum number of Macs"), "{error}");

        let api = FakeApi::new(vec![Ok((404, r#"{"activated":false,"error":"license_key not found."}"#.to_string()))]);
        let error = activate_with(&api, &db, "typo").unwrap_err().to_string();
        assert!(error.contains("couldn't find this license key"), "{error}");

        let api = FakeApi::new(vec![Err(anyhow!("offline"))]);
        let error = activate_with(&api, &db, "key").unwrap_err().to_string();
        assert!(error.contains("needs the internet once"), "{error}");
        assert!(!is_pro(&db));
        assert_eq!(
            require_pro(&db, "speaker labels").unwrap_err().to_string(),
            "PRO_REQUIRED: Murmela Pro unlocks speaker labels. Add your license key in Settings → License."
        );
    }

    #[test]
    fn deactivation_frees_the_slot_and_returns_to_free() {
        let db = temp_db();
        let api = FakeApi::new(vec![activated(STORE_ID, PRODUCT_IDS[0])]);
        activate_with(&api, &db, "38b1460a-5104-4067-a91d-77b872934d51").unwrap();

        let offline = FakeApi::new(vec![Err(anyhow!("offline"))]);
        assert!(deactivate_with(&offline, &db).is_err());
        assert!(is_pro(&db), "an offline deactivation must not drop the license");

        let api = FakeApi::new(vec![Ok((200, r#"{"deactivated":true,"error":null}"#.to_string()))]);
        assert!(!deactivate_with(&api, &db).unwrap().pro);
        assert!(api.calls.borrow()[0]
            .1
            .contains("instance_id=47596ad9-a811-4ebf-ac8a-03fc7b6d2a17"));
        assert!(!is_pro(&db));
    }

    #[test]
    fn deactivation_succeeds_when_the_activation_was_already_reset() {
        let db = temp_db();
        let api = FakeApi::new(vec![activated(STORE_ID, PRODUCT_IDS[0])]);
        activate_with(&api, &db, "key-1").unwrap();
        let api = FakeApi::new(vec![Ok((404, r#"{"deactivated":false,"error":"instance not found"}"#.to_string()))]);
        assert!(!deactivate_with(&api, &db).unwrap().pro);
    }

    #[test]
    fn activation_links_yield_only_plausible_keys() {
        let key = "38B1460A-5104-4067-A91D-77B872934D51";
        assert_eq!(key_from_link(&format!("murmela://activate?key={key}")).as_deref(), Some(key));
        assert_eq!(key_from_link(&format!("murmela://activate/?key={key}&utm=mail")).as_deref(), Some(key));
        assert_eq!(key_from_link(&format!("https://activate?key={key}")), None);
        assert_eq!(key_from_link(&format!("murmela://settings?key={key}")), None);
        assert_eq!(key_from_link("murmela://activate?key=%3Cscript%3E-xxxxxxxx"), None);
        assert_eq!(key_from_link("murmela://activate?key=short"), None);
        assert_eq!(key_from_link("murmela://activate"), None);

        let pending = PendingLinks::default();
        pending.offer(key.to_string());
        assert_eq!(pending.take().as_deref(), Some(key));
        assert_eq!(pending.take(), None);
    }

    #[test]
    fn form_values_are_url_encoded() {
        assert_eq!(form_encode(&[("a b", "x&y=z/ü")]), "a+b=x%26y%3Dz%2F%C3%BC");
    }
}
