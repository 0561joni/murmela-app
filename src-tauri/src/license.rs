//! Murmela license keys, sold through Lemon Squeezy, in two kinds:
//!
//! - **Pro** unlocks the Pro features with one year of updates. Pro keeps working
//!   forever in every Murmela released before that year ended. A newer release runs as
//!   Free until the key is renewed (a new Pro key) or replaced by a Lifetime key.
//! - **Lifetime** unlocks the Pro features in every release. Keys bought before Pro
//!   existed are Lifetime keys.
//!
//! A key is activated once against the Lemon Squeezy License API, which needs no API
//! secret. After that the activation lives only in the local database and is never
//! re-checked, so Pro keeps working offline and without our servers. Whether a key
//! covers this version compares two dates that need no network: the day this build was
//! released (built in, see build.rs) and the last day of the key's update year. The only
//! other request is the optional "Deactivate this Mac", which frees one of the key's
//! slots. Both are logged in Settings → Network activity, without the key.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Months, NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::network_activity::{self, Destination, Outcome};
use crate::storage::open_connection_by_path;

/// Lemon Squeezy store that sells Murmela. Keys from any other store are refused.
const STORE_ID: u64 = 497381;
/// Products whose keys are Lifetime keys: "Murmela Pro – Founders Lifetime License".
// TODO(pricing): add the $99 Lifetime product here if it becomes a product of its own.
const LIFETIME_PRODUCT_IDS: &[u64] = &[1439649];
/// Products and variants whose keys are Pro keys with one year of updates. Pro is
/// checked first, so a Pro variant of a Lifetime product is still Pro.
/// "Murmela Pro" ($19, one year of updates).
const PRO_PRODUCT_IDS: &[u64] = &[1445554];
const PRO_VARIANT_IDS: &[u64] = &[2256677];
/// How long a Pro key unlocks new releases, counted from its purchase.
const PRO_UPDATE_MONTHS: u32 = 12;
/// The day this build was released, YYYY-MM-DD (set by build.rs).
const RELEASE_DATE: &str = env!("MURMELA_RELEASE_DATE");

const API_BASE: &str = "https://api.lemonsqueezy.com/v1/licenses";
const SUPPORT_EMAIL: &str = "hello@murmela.com";
/// Prefix that the frontend recognises to show a "Pro feature" hint instead of an error.
pub const PRO_REQUIRED: &str = "PRO_REQUIRED";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// One year of updates.
    Pro,
    /// Updates for good.
    Lifetime,
}

impl Tier {
    fn as_str(self) -> &'static str {
        match self {
            Tier::Pro => "pro",
            Tier::Lifetime => "lifetime",
        }
    }

    /// Rows without a tier come from before Pro existed, when every key was a Lifetime key.
    fn from_column(value: Option<&str>) -> Self {
        match value {
            Some("pro") => Tier::Pro,
            _ => Tier::Lifetime,
        }
    }

    fn for_product(product_id: Option<u64>, variant_id: Option<u64>) -> Option<Self> {
        if variant_id.is_some_and(|id| PRO_VARIANT_IDS.contains(&id))
            || product_id.is_some_and(|id| PRO_PRODUCT_IDS.contains(&id))
        {
            Some(Tier::Pro)
        } else if product_id.is_some_and(|id| LIFETIME_PRODUCT_IDS.contains(&id)) {
            Some(Tier::Lifetime)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseStatus {
    /// The Pro features are unlocked in this version of Murmela.
    pub pro: bool,
    /// The key on this Mac, if any. A Pro key can be here while `pro` is false: this
    /// version was released after the key's update year ended.
    pub tier: Option<Tier>,
    /// Last day whose releases a Pro key unlocks (YYYY-MM-DD). None for Lifetime.
    pub updates_until: Option<String>,
    /// The day this version of Murmela was released (YYYY-MM-DD).
    pub release_date: String,
    /// Last characters of the key, enough to recognise it in a receipt.
    pub key_hint: Option<String>,
    pub licensed_to: Option<String>,
    pub activated_at: Option<String>,
}

impl LicenseStatus {
    fn free(release: NaiveDate) -> Self {
        Self {
            pro: false,
            tier: None,
            updates_until: None,
            release_date: release.to_string(),
            key_hint: None,
            licensed_to: None,
            activated_at: None,
        }
    }
}

/// The day this build was released. A malformed date (impossible from build.rs) unlocks
/// only Lifetime keys.
fn release_date() -> NaiveDate {
    NaiveDate::parse_from_str(RELEASE_DATE, "%Y-%m-%d").unwrap_or(NaiveDate::MAX)
}

#[derive(Debug, Clone)]
struct Activation {
    license_key: String,
    instance_id: String,
    customer_name: Option<String>,
    customer_email: Option<String>,
    activated_at: String,
    tier: Tier,
    updates_until: Option<NaiveDate>,
}

impl Activation {
    /// True when the key unlocks Pro in a release from this day.
    fn covers(&self, release: NaiveDate) -> bool {
        match (self.tier, self.updates_until) {
            (Tier::Lifetime, _) | (Tier::Pro, None) => true,
            (Tier::Pro, Some(until)) => release <= until,
        }
    }

    fn status(&self, release: NaiveDate) -> LicenseStatus {
        LicenseStatus {
            pro: self.covers(release),
            tier: Some(self.tier),
            updates_until: match self.tier {
                Tier::Pro => self.updates_until.map(|until| until.to_string()),
                Tier::Lifetime => None,
            },
            release_date: release.to_string(),
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
    // Added with the Pro tier. Older rows keep NULL here and stay Lifetime.
    for (column, kind) in [("variant_id", "INTEGER"), ("tier", "TEXT"), ("updates_until", "TEXT")] {
        let exists: bool = connection.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('license_activation') WHERE name = ?1",
            [column],
            |row| row.get(0),
        )?;
        if !exists {
            connection.execute_batch(&format!(
                "ALTER TABLE license_activation ADD COLUMN {column} {kind};"
            ))?;
        }
    }
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
            "SELECT license_key, instance_id, customer_name, customer_email, activated_at, tier, updates_until
             FROM license_activation WHERE id = 1",
            [],
            |row| {
                let tier: Option<String> = row.get(5)?;
                let updates_until: Option<String> = row.get(6)?;
                Ok(Activation {
                    license_key: row.get(0)?,
                    instance_id: row.get(1)?,
                    customer_name: row.get(2)?,
                    customer_email: row.get(3)?,
                    activated_at: row.get(4)?,
                    tier: Tier::from_column(tier.as_deref()),
                    updates_until: updates_until
                        .and_then(|day| NaiveDate::parse_from_str(&day, "%Y-%m-%d").ok()),
                })
            },
        )
        .optional()?)
}

pub fn status(db_path: &Path) -> Result<LicenseStatus> {
    status_for(db_path, release_date())
}

fn status_for(db_path: &Path, release: NaiveDate) -> Result<LicenseStatus> {
    Ok(load(db_path)?
        .map(|activation| activation.status(release))
        .unwrap_or_else(|| LicenseStatus::free(release)))
}

/// True when this Mac has a key that unlocks Pro in this version. A database error counts as Free.
pub fn is_pro(db_path: &Path) -> bool {
    is_pro_for(db_path, release_date())
}

fn is_pro_for(db_path: &Path, release: NaiveDate) -> bool {
    matches!(load(db_path), Ok(Some(activation)) if activation.covers(release))
}

/// Fails with a `PRO_REQUIRED:` error unless Pro is active. `feature` reads mid-sentence, e.g. "speaker labels".
pub fn require_pro(db_path: &Path, feature: &str) -> Result<()> {
    require_pro_for(db_path, release_date(), feature)
}

fn require_pro_for(db_path: &Path, release: NaiveDate, feature: &str) -> Result<()> {
    match load(db_path) {
        Ok(Some(activation)) if activation.covers(release) => Ok(()),
        Ok(Some(Activation { updates_until: Some(until), .. })) => bail!(
            "{PRO_REQUIRED}: Your Pro updates ended on {until}, and this version of Murmela is newer, so {feature} is locked here. Renew Pro or get Lifetime in Settings → License."
        ),
        _ => bail!("{PRO_REQUIRED}: Murmela Pro unlocks {feature}. Add your license key in Settings → License."),
    }
}

/// When installing a release published on `published` would lock Pro features that work
/// today, returns the last day of the key's update year. An unknown date counts as newer.
pub fn update_ends_pro(db_path: &Path, published: Option<NaiveDate>) -> Option<NaiveDate> {
    update_ends_pro_for(db_path, release_date(), published)
}

fn update_ends_pro_for(db_path: &Path, release: NaiveDate, published: Option<NaiveDate>) -> Option<NaiveDate> {
    let activation = load(db_path).ok()??;
    let until = activation.updates_until.filter(|_| activation.tier == Tier::Pro)?;
    let covered = |day: Option<NaiveDate>| day.is_some_and(|day| day <= until);
    (covered(Some(release)) && !covered(published)).then_some(until)
}

/// The last day of a Pro key's update year: one year after its purchase.
fn pro_updates_until(purchased: Option<&str>, today: NaiveDate) -> NaiveDate {
    let purchased = purchased
        .and_then(|day| DateTime::parse_from_rfc3339(day).ok())
        .map(|day| day.with_timezone(&Utc).date_naive())
        .unwrap_or(today);
    purchased
        .checked_add_months(Months::new(PRO_UPDATE_MONTHS))
        .unwrap_or(NaiveDate::MAX)
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
    activate_for(api, db_path, license_key, release_date(), Utc::now())
}

/// Activates `license_key` on this Mac. A Pro key already here can be replaced by a newer
/// Pro key (renewal) or a Lifetime key (upgrade) without deactivating it first; its slot
/// is freed afterwards when possible.
fn activate_for(
    api: &dyn LicenseApi,
    db_path: &Path,
    license_key: &str,
    release: NaiveDate,
    now: DateTime<Utc>,
) -> Result<LicenseStatus> {
    let license_key = license_key.trim();
    if license_key.is_empty() {
        bail!("Paste the license key from your receipt email.");
    }
    let previous = load(db_path)?;
    if let Some(existing) = &previous {
        if existing.license_key.eq_ignore_ascii_case(license_key) {
            return Ok(existing.status(release));
        }
        if existing.tier == Tier::Lifetime {
            bail!("This Mac already has Murmela Lifetime. Deactivate it first to use a different key.");
        }
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
    let tier = (meta.store_id == Some(STORE_ID))
        .then(|| Tier::for_product(meta.product_id, meta.variant_id))
        .flatten();
    let Some(tier) = tier else {
        // Free the slot we just used on someone else's product before refusing it.
        let _ = api.post(
            "deactivate",
            &[("license_key", license_key), ("instance_id", &instance_id)],
        );
        bail!("This license key isn't for Murmela Pro. Use the key from your Murmela receipt.");
    };
    let updates_until = match tier {
        Tier::Lifetime => None,
        Tier::Pro => {
            let purchased = response.license_key.as_ref().and_then(|key| key.created_at.as_deref());
            let until = pro_updates_until(purchased, now.date_naive());
            // A renewal bought early never shortens the year already paid for.
            let paid = previous.as_ref().and_then(|old| old.updates_until);
            Some(paid.map_or(until, |paid| paid.max(until)))
        }
    };

    let activation = Activation {
        license_key: license_key.to_string(),
        instance_id,
        customer_name: meta.customer_name,
        customer_email: meta.customer_email,
        activated_at: now.to_rfc3339(),
        tier,
        updates_until,
    };
    open(db_path)?.execute(
        "INSERT OR REPLACE INTO license_activation
            (id, license_key, instance_id, instance_name, customer_name, customer_email, product_id,
             activated_at, variant_id, tier, updates_until)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            activation.license_key,
            activation.instance_id,
            instance_name,
            activation.customer_name,
            activation.customer_email,
            meta.product_id.map(|id| id as i64),
            activation.activated_at,
            meta.variant_id.map(|id| id as i64),
            tier.as_str(),
            updates_until.map(|until| until.to_string()),
        ],
    )?;
    if let Some(old) = previous {
        // Best effort: the replaced key's slot stays taken if this fails.
        let _ = api.post(
            "deactivate",
            &[
                ("license_key", old.license_key.as_str()),
                ("instance_id", old.instance_id.as_str()),
            ],
        );
    }
    Ok(activation.status(release))
}

fn deactivate_with(api: &dyn LicenseApi, db_path: &Path) -> Result<LicenseStatus> {
    let Some(activation) = load(db_path)? else {
        return Ok(LicenseStatus::free(release_date()));
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
    Ok(LicenseStatus::free(release_date()))
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
    /// When the key was bought, e.g. "2026-10-16T12:00:00.000000Z".
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiInstance {
    id: String,
}

#[derive(Debug, Default, Deserialize)]
struct ApiMeta {
    store_id: Option<u64>,
    product_id: Option<u64>,
    variant_id: Option<u64>,
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

    const PRO_VARIANT: u64 = 2256677;

    fn activated(store_id: u64, product_id: u64) -> Result<(u16, String)> {
        answer(store_id, product_id, None, None, "47596ad9-a811-4ebf-ac8a-03fc7b6d2a17")
    }

    /// A Pro key bought on `created_at`, activated as `instance`.
    fn activated_pro(created_at: Option<&str>, instance: &str) -> Result<(u16, String)> {
        answer(STORE_ID, 1445554, Some(PRO_VARIANT), created_at, instance)
    }

    fn answer(
        store_id: u64,
        product_id: u64,
        variant_id: Option<u64>,
        created_at: Option<&str>,
        instance: &str,
    ) -> Result<(u16, String)> {
        Ok((
            200,
            serde_json::json!({
                "activated": true, "error": null,
                "license_key": { "status": "active", "activation_limit": 3, "activation_usage": 1,
                                 "created_at": created_at, "expires_at": null },
                "instance": { "id": instance, "name": "Mac abc123" },
                "meta": { "store_id": store_id, "product_id": product_id, "variant_id": variant_id,
                          "customer_name": "Ada Lovelace", "customer_email": "ada@example.com" }
            })
            .to_string(),
        ))
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&Utc)
    }

    fn deactivated() -> Result<(u16, String)> {
        Ok((200, r#"{"deactivated":true,"error":null}"#.to_string()))
    }

    #[test]
    fn existing_keys_without_a_tier_are_lifetime() {
        let db = temp_db();
        let connection = open_connection_by_path(&db).unwrap();
        // The table as it was before the Pro tier.
        connection
            .execute_batch(
                "CREATE TABLE license_activation (
                    id INTEGER PRIMARY KEY CHECK (id = 1), license_key TEXT NOT NULL,
                    instance_id TEXT NOT NULL, instance_name TEXT NOT NULL, customer_name TEXT,
                    customer_email TEXT, product_id INTEGER, activated_at TEXT NOT NULL);
                 INSERT INTO license_activation VALUES
                    (1, 'founder-key-0001', 'i-1', 'Mac 1', 'Ada', NULL, 1439649, '2026-10-14T10:00:00Z');",
            )
            .unwrap();
        drop(connection);
        let status = status_for(&db, day("2099-01-01")).unwrap();
        assert!(status.pro);
        assert_eq!(status.tier, Some(Tier::Lifetime));
        assert_eq!(status.updates_until, None);
        assert!(is_pro_for(&db, day("2099-01-01")));
        assert_eq!(update_ends_pro_for(&db, day("2026-10-16"), None), None);
    }

    #[test]
    fn a_pro_key_unlocks_releases_from_its_first_year_only() {
        let db = temp_db();
        let api = FakeApi::new(vec![activated_pro(Some("2026-10-16T09:30:00.000000Z"), "pro-1")]);
        let status = activate_for(&api, &db, "pro-key-0001", day("2026-10-16"), at("2026-10-20T08:00:00Z")).unwrap();
        assert!(status.pro);
        assert_eq!(status.tier, Some(Tier::Pro));
        assert_eq!(status.updates_until.as_deref(), Some("2027-10-16"));
        assert_eq!(status.release_date, "2026-10-16");

        assert!(is_pro_for(&db, day("2027-10-16")));
        assert!(!is_pro_for(&db, day("2027-10-17")));
        let later = status_for(&db, day("2027-12-01")).unwrap();
        assert!(!later.pro, "a release after the update year runs as Free");
        assert_eq!(later.tier, Some(Tier::Pro), "the key stays, for a later downgrade or renewal");
        let error = require_pro_for(&db, day("2027-12-01"), "speaker labels").unwrap_err().to_string();
        assert!(error.starts_with("PRO_REQUIRED: Your Pro updates ended on 2027-10-16"), "{error}");
        assert!(require_pro_for(&db, day("2027-10-01"), "speaker labels").is_ok());
    }

    #[test]
    fn a_pro_key_without_a_purchase_date_counts_from_activation() {
        let db = temp_db();
        let api = FakeApi::new(vec![activated_pro(None, "pro-1")]);
        let status = activate_for(&api, &db, "pro-key-0001", day("2026-10-16"), at("2026-11-02T23:00:00Z")).unwrap();
        assert_eq!(status.updates_until.as_deref(), Some("2027-11-02"));
    }

    #[test]
    fn the_updater_holds_back_releases_that_would_lock_pro() {
        let db = temp_db();
        let api = FakeApi::new(vec![activated_pro(Some("2026-10-16T09:30:00Z"), "pro-1")]);
        activate_for(&api, &db, "pro-key-0001", day("2026-10-16"), at("2026-10-16T10:00:00Z")).unwrap();
        let running = day("2027-03-01");
        assert_eq!(update_ends_pro_for(&db, running, Some(day("2027-10-16"))), None);
        assert_eq!(update_ends_pro_for(&db, running, Some(day("2027-10-17"))), Some(day("2027-10-16")));
        assert_eq!(update_ends_pro_for(&db, running, None), Some(day("2027-10-16")), "an undated release counts as newer");
        // Already running a version past the update year: Pro is locked anyway, so updates flow as for Free.
        assert_eq!(update_ends_pro_for(&db, day("2027-11-01"), Some(day("2028-01-01"))), None);

        let free = temp_db();
        assert_eq!(update_ends_pro_for(&free, running, None), None);
    }

    #[test]
    fn a_new_pro_key_renews_and_frees_the_old_slot() {
        let db = temp_db();
        let api = FakeApi::new(vec![activated_pro(Some("2026-10-16T09:30:00Z"), "pro-1")]);
        activate_for(&api, &db, "pro-key-0001", day("2026-10-16"), at("2026-10-16T10:00:00Z")).unwrap();

        let api = FakeApi::new(vec![activated_pro(Some("2027-11-20T12:00:00Z"), "pro-2"), deactivated()]);
        let status = activate_for(&api, &db, "pro-key-0002", day("2027-11-15"), at("2027-11-20T12:05:00Z")).unwrap();
        assert!(status.pro);
        assert_eq!(status.updates_until.as_deref(), Some("2028-11-20"));
        let calls = api.calls.borrow();
        assert_eq!(calls[1].0, "deactivate");
        assert!(calls[1].1.contains("license_key=pro-key-0001") && calls[1].1.contains("instance_id=pro-1"));
    }

    #[test]
    fn an_early_renewal_never_shortens_the_paid_year() {
        let db = temp_db();
        let api = FakeApi::new(vec![activated_pro(Some("2026-10-16T09:30:00Z"), "pro-1")]);
        activate_for(&api, &db, "pro-key-0001", day("2026-10-16"), at("2026-10-16T10:00:00Z")).unwrap();
        // A key bought earlier than the one it replaces, e.g. a spare from the same order.
        let api = FakeApi::new(vec![activated_pro(Some("2026-09-01T09:30:00Z"), "pro-2"), deactivated()]);
        let status = activate_for(&api, &db, "pro-key-0002", day("2026-10-16"), at("2026-10-17T10:00:00Z")).unwrap();
        assert_eq!(status.updates_until.as_deref(), Some("2027-10-16"));
    }

    #[test]
    fn lifetime_replaces_pro_but_nothing_replaces_lifetime() {
        let db = temp_db();
        let api = FakeApi::new(vec![activated_pro(Some("2026-10-16T09:30:00Z"), "pro-1")]);
        activate_for(&api, &db, "pro-key-0001", day("2026-10-16"), at("2026-10-16T10:00:00Z")).unwrap();

        // The old slot staying taken doesn't stop the upgrade.
        let api = FakeApi::new(vec![activated(STORE_ID, LIFETIME_PRODUCT_IDS[0]), Err(anyhow!("offline"))]);
        let status = activate_for(&api, &db, "lifetime-key-1", day("2028-01-01"), at("2028-01-02T10:00:00Z")).unwrap();
        assert!(status.pro);
        assert_eq!(status.tier, Some(Tier::Lifetime));
        assert_eq!(status.updates_until, None);
        assert!(is_pro_for(&db, day("2099-01-01")));

        let api = FakeApi::new(vec![]);
        let error = activate_for(&api, &db, "pro-key-0003", day("2028-01-01"), at("2028-01-02T10:00:00Z"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("already has Murmela Lifetime"), "{error}");
        assert!(api.calls.borrow().is_empty());
    }

    #[test]
    fn a_refused_replacement_keeps_the_old_key() {
        let db = temp_db();
        let api = FakeApi::new(vec![activated_pro(Some("2026-10-16T09:30:00Z"), "pro-1")]);
        activate_for(&api, &db, "pro-key-0001", day("2026-10-16"), at("2026-10-16T10:00:00Z")).unwrap();
        let api = FakeApi::new(vec![activated(STORE_ID + 1, 1), deactivated()]);
        assert!(activate_for(&api, &db, "other-store-key", day("2026-10-16"), at("2026-10-17T10:00:00Z")).is_err());
        // Only the new, foreign activation was freed.
        assert_eq!(api.calls.borrow().len(), 2);
        assert!(api.calls.borrow()[1].1.contains("license_key=other-store-key"));
        assert_eq!(status_for(&db, day("2026-10-16")).unwrap().tier, Some(Tier::Pro));
    }

    #[test]
    fn activation_unlocks_pro_and_survives_without_network() {
        let db = temp_db();
        assert!(!is_pro(&db));
        let api = FakeApi::new(vec![activated(STORE_ID, LIFETIME_PRODUCT_IDS[0])]);
        let status = activate_with(&api, &db, "  38B1460A-5104-4067-A91D-77B872934D51 \n").unwrap();
        assert!(status.pro);
        assert_eq!(status.key_hint.as_deref(), Some("••••4D51"));
        assert_eq!(status.licensed_to.as_deref(), Some("Ada Lovelace"));
        assert_eq!(status.tier, Some(Tier::Lifetime));
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
        let api = FakeApi::new(vec![activated(STORE_ID, LIFETIME_PRODUCT_IDS[0])]);
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
        let api = FakeApi::new(vec![activated(STORE_ID, LIFETIME_PRODUCT_IDS[0])]);
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
