//! Every connection Murmela makes to the internet, and the only places it may go.
//!
//! Murmela works offline. It contacts three services, each for one reason that
//! Settings → General → Network activity lists: Hugging Face for model
//! downloads, GitHub for the optional update check, and Lemon Squeezy to
//! activate Murmela Pro. Every request goes through a [`Connection`], which
//! refuses any other host and writes one row to `network_activity` when it
//! ends, also when it fails. A row holds the date, destination, purpose, size
//! and outcome; never recordings, text, license keys or other personal data.
//! There is no analytics, crash reporting or telemetry.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::{params, Connection as Database};
use serde::Serialize;

use crate::storage::open_connection_by_path;

/// Oldest rows beyond this are dropped, so the log never grows without bound.
const MAX_ENTRIES: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    HuggingFace,
    GitHub,
    LemonSqueezy,
}

impl Destination {
    pub const ALL: [Destination; 3] = [
        Destination::HuggingFace,
        Destination::GitHub,
        Destination::LemonSqueezy,
    ];

    fn key(self) -> &'static str {
        match self {
            Destination::HuggingFace => "hugging_face",
            Destination::GitHub => "github",
            Destination::LemonSqueezy => "lemon_squeezy",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|destination| destination.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Destination::HuggingFace => "Hugging Face",
            Destination::GitHub => "GitHub",
            Destination::LemonSqueezy => "Lemon Squeezy",
        }
    }

    /// The one host a request to this destination may start at.
    pub fn host(self) -> &'static str {
        match self {
            Destination::HuggingFace => "huggingface.co",
            Destination::GitHub => "github.com",
            Destination::LemonSqueezy => "api.lemonsqueezy.com",
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Destination::HuggingFace => {
                "Downloads the speech, translation and voice-activity models you choose. Nothing is uploaded."
            }
            Destination::GitHub => {
                "Checks for a new version of Murmela while automatic updates are on or when you press Check now, and downloads it when you choose Download update."
            }
            Destination::LemonSqueezy => {
                "Activates Murmela Pro once with your license key, and frees this Mac's slot if you deactivate it. Pro then works offline."
            }
        }
    }

    pub fn allows(self, url: &str) -> bool {
        let Ok(url) = tauri::Url::parse(url) else {
            return false;
        };
        let host = url.host_str().unwrap_or_default();
        if cfg!(test) && url.scheme() == "http" && host == "127.0.0.1" {
            // Unit tests download from a local server.
            return true;
        }
        url.scheme() == "https" && host == self.host()
    }
}

/// One of the places Murmela can contact, as listed in Settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationInfo {
    pub id: Destination,
    pub name: &'static str,
    pub host: &'static str,
    pub reason: &'static str,
}

pub fn destinations() -> Vec<DestinationInfo> {
    Destination::ALL
        .into_iter()
        .map(|destination| DestinationInfo {
            id: destination,
            name: destination.name(),
            host: destination.host(),
            reason: destination.reason(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Failed,
    Canceled,
}

impl Outcome {
    fn key(self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::Failed => "failed",
            Outcome::Canceled => "canceled",
        }
    }

    fn from_key(key: &str) -> Self {
        match key {
            "completed" => Outcome::Completed,
            "canceled" => Outcome::Canceled,
            _ => Outcome::Failed,
        }
    }
}

/// A logged connection, newest first in [`list`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkActivityEntry {
    pub id: i64,
    pub started_at: String,
    pub destination: Destination,
    pub host: &'static str,
    pub purpose: String,
    /// Bytes sent and received, or `None` when the library making the request doesn't report it.
    pub bytes: Option<i64>,
    pub outcome: Outcome,
}

/// A connection to one destination for one purpose. It may send several
/// requests (a model with many files); it is logged once, when dropped, if it
/// sent any. A connection that is never finished is logged as failed.
pub struct Connection {
    db_path: PathBuf,
    destination: Destination,
    purpose: String,
    started_at: String,
    requests: u32,
    bytes: Option<u64>,
    outcome: Outcome,
}

impl Connection {
    pub fn begin(db_path: &Path, destination: Destination, purpose: impl Into<String>) -> Self {
        Self {
            db_path: db_path.to_path_buf(),
            destination,
            purpose: purpose.into(),
            started_at: Utc::now().to_rfc3339(),
            requests: 0,
            bytes: None,
            outcome: Outcome::Failed,
        }
    }

    /// Sends a request, refusing any URL outside this connection's destination.
    pub fn send(
        &mut self,
        client: &reqwest::blocking::Client,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::Response> {
        let request = request.build()?;
        self.record_request(request.url().as_str())?;
        let sent = request
            .body()
            .and_then(|body| body.as_bytes())
            .map_or(0, |body| body.len() as u64);
        self.add_bytes(sent);
        Ok(client.execute(request)?)
    }

    /// Counts a request that a library makes for Murmela (the updater), after checking its URL.
    pub fn record_request(&mut self, url: &str) -> Result<()> {
        if !self.destination.allows(url) {
            return Err(anyhow!(
                "Murmela refused to connect to {url}: it only contacts {} for this.",
                self.destination.host()
            ));
        }
        self.requests += 1;
        Ok(())
    }

    pub fn add_bytes(&mut self, bytes: u64) {
        *self.bytes.get_or_insert(0) += bytes;
    }

    pub fn finish(mut self, outcome: Outcome) {
        self.outcome = outcome;
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.requests == 0 {
            return;
        }
        if let Err(error) = record(self) {
            eprintln!("[network-activity] could not log a connection: {error:#}");
        }
    }
}

pub fn ensure_schema(connection: &Database) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS network_activity (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            started_at TEXT NOT NULL,
            destination TEXT NOT NULL,
            purpose TEXT NOT NULL,
            bytes INTEGER,
            outcome TEXT NOT NULL
        );",
    )?;
    Ok(())
}

fn open(db_path: &Path) -> Result<Database> {
    let connection = open_connection_by_path(db_path)?;
    // Downloads may finish while the app writes elsewhere; wait rather than lose a row.
    connection.busy_timeout(Duration::from_secs(10))?;
    ensure_schema(&connection)?;
    Ok(connection)
}

fn record(entry: &Connection) -> Result<()> {
    let connection = open(&entry.db_path)?;
    connection.execute(
        "INSERT INTO network_activity (started_at, destination, purpose, bytes, outcome)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            entry.started_at,
            entry.destination.key(),
            entry.purpose,
            entry.bytes.map(|bytes| bytes as i64),
            entry.outcome.key(),
        ],
    )?;
    connection.execute(
        "DELETE FROM network_activity WHERE id <= (SELECT MAX(id) FROM network_activity) - ?1",
        params![MAX_ENTRIES],
    )?;
    Ok(())
}

pub fn list(db_path: &Path) -> Result<Vec<NetworkActivityEntry>> {
    let connection = open(db_path)?;
    let mut statement = connection.prepare(
        "SELECT id, started_at, destination, purpose, bytes, outcome
         FROM network_activity ORDER BY id DESC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, started_at, destination, purpose, bytes, outcome)| {
            let destination = Destination::from_key(&destination)?;
            Some(NetworkActivityEntry {
                id,
                started_at,
                destination,
                host: destination.host(),
                purpose,
                bytes,
                outcome: Outcome::from_key(&outcome),
            })
        })
        .collect())
}

pub fn clear(db_path: &Path) -> Result<()> {
    open(db_path)?.execute("DELETE FROM network_activity", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("murmela-network-{}.sqlite", uuid::Uuid::new_v4()))
    }

    #[test]
    fn a_finished_connection_is_logged_with_its_size() {
        let db = temp_db();
        let mut connection =
            Connection::begin(&db, Destination::HuggingFace, "Download Whisper Small");
        connection
            .record_request(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
            )
            .unwrap();
        connection.add_bytes(1500);
        connection.add_bytes(500);
        connection.finish(Outcome::Completed);

        let entries = list(&db).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].destination, Destination::HuggingFace);
        assert_eq!(entries[0].host, "huggingface.co");
        assert_eq!(entries[0].purpose, "Download Whisper Small");
        assert_eq!(entries[0].bytes, Some(2000));
        assert_eq!(entries[0].outcome, Outcome::Completed);
    }

    #[test]
    fn an_unfinished_connection_is_logged_as_failed_and_an_unused_one_not_at_all() {
        let db = temp_db();
        {
            let mut connection = Connection::begin(&db, Destination::GitHub, "Update check");
            connection.record_request("https://github.com/0561joni/murmela-releases/releases/latest/download/latest.json").unwrap();
            // Dropped early, e.g. by `?` on an error.
        }
        Connection::begin(&db, Destination::HuggingFace, "Already installed")
            .finish(Outcome::Completed);

        let entries = list(&db).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].purpose, "Update check");
        assert_eq!(entries[0].outcome, Outcome::Failed);
        assert_eq!(
            entries[0].bytes, None,
            "the updater doesn't report the feed's size"
        );
    }

    #[test]
    fn requests_outside_the_destination_are_refused_and_not_counted() {
        let db = temp_db();
        let mut connection = Connection::begin(&db, Destination::HuggingFace, "Download");
        for url in [
            "https://example.com/model.bin",
            "http://huggingface.co/model.bin",
            "https://huggingface.co.evil.example/model.bin",
            "https://github.com/model.bin",
            "not a url",
        ] {
            assert!(
                connection.record_request(url).is_err(),
                "{url} must be refused"
            );
        }
        let client = reqwest::blocking::Client::new();
        let request = client.get("https://example.com/");
        assert!(connection.send(&client, request).is_err());
        connection.finish(Outcome::Failed);
        assert!(list(&db).unwrap().is_empty());
    }

    #[test]
    fn send_counts_the_request_body_and_logs_through_the_helper() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let db = temp_db();
        let client = reqwest::blocking::Client::new();
        let mut connection =
            Connection::begin(&db, Destination::LemonSqueezy, "Activate Murmela Pro");
        let response = connection
            .send(
                &client,
                client
                    .post(format!("http://{address}/activate"))
                    .body("license_key=x"),
            )
            .unwrap();
        let body = response.text().unwrap();
        connection.add_bytes(body.len() as u64);
        connection.finish(Outcome::Completed);
        server.join().unwrap();

        let entries = list(&db).unwrap();
        assert_eq!(entries[0].bytes, Some(("license_key=x".len() + 2) as i64));
        assert_eq!(entries[0].destination, Destination::LemonSqueezy);
    }

    #[test]
    fn clear_empties_the_log_and_old_rows_are_dropped() {
        let db = temp_db();
        for index in 0..(MAX_ENTRIES + 5) {
            let mut connection =
                Connection::begin(&db, Destination::GitHub, format!("Update check {index}"));
            connection.record_request("https://github.com/x").unwrap();
            connection.finish(Outcome::Completed);
        }
        let entries = list(&db).unwrap();
        assert_eq!(entries.len() as i64, MAX_ENTRIES);
        assert_eq!(
            entries[0].purpose,
            format!("Update check {}", MAX_ENTRIES + 4)
        );
        clear(&db).unwrap();
        assert!(list(&db).unwrap().is_empty());
    }

    #[test]
    fn exactly_three_destinations_are_listed() {
        let listed = destinations();
        let hosts: Vec<_> = listed.iter().map(|destination| destination.host).collect();
        assert_eq!(
            hosts,
            ["huggingface.co", "github.com", "api.lemonsqueezy.com"]
        );
        assert!(listed
            .iter()
            .all(|destination| !destination.reason.is_empty()));
    }

    #[test]
    fn the_updater_feed_is_on_github() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let endpoints = config["plugins"]["updater"]["endpoints"]
            .as_array()
            .unwrap();
        assert!(!endpoints.is_empty());
        for endpoint in endpoints {
            assert!(
                Destination::GitHub.allows(endpoint.as_str().unwrap()),
                "{endpoint}"
            );
        }
    }

    /// Every network call site goes through `Connection`, so none can be left out of the log.
    #[test]
    fn every_network_call_site_goes_through_the_log() {
        const CALL_SITES: [&str; 3] = ["license.rs", "model_downloads.rs", "updates.rs"];
        let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        let mut pending = vec![source_dir.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    files.push(path);
                }
            }
        }
        let mut call_sites = Vec::new();
        for path in files {
            let name = path
                .strip_prefix(&source_dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if name == "network_activity.rs" {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let networked = [
                "reqwest",
                "UpdaterExt",
                ".updater()",
                "ureq::",
                "hyper::",
                "isahc::",
            ]
            .iter()
            .any(|marker| source.contains(marker));
            if !networked {
                continue;
            }
            assert!(
                CALL_SITES.contains(&name.as_str()),
                "{name} talks to the network; route it through network_activity::Connection and list it here"
            );
            assert!(
                source.contains("network_activity::Connection"),
                "{name} must log through network_activity::Connection"
            );
            for direct in [".send()", "client.execute("] {
                assert!(
                    !source.contains(direct),
                    "{name} sends a request directly ({direct}); use Connection::send"
                );
            }
            call_sites.push(name);
        }
        call_sites.sort();
        assert_eq!(call_sites, CALL_SITES);

        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .unwrap();
        for client in [
            "ureq",
            "hyper",
            "isahc",
            "surf",
            "attohttpc",
            "curl ",
            "curl=",
        ] {
            assert!(
                !manifest.contains(client),
                "Cargo.toml adds another HTTP client: {client}"
            );
        }
    }
}
