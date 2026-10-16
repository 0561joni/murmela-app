//! Background updates from the public `murmela-releases` repository.
//!
//! Murmela checks `latest.json` (see `plugins.updater` in tauri.conf.json)
//! shortly after launch and then every few hours, while
//! `auto_update_enabled` is on. A newer release is downloaded quietly and its
//! signature checked against the public key in the app. The user then chooses
//! "Restart to update"; otherwise the update is installed when Murmela quits.
//! Nothing is installed while Murmela is running. Each check and download is
//! logged in Settings → Network activity.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::app_state::AppState;
use crate::network_activity::{self, Destination, Outcome};

pub const UPDATE_STATUS_EVENT: &str = "update-status";
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Downloading,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub state: UpdateState,
    pub current_version: String,
    /// The release being downloaded or ready to install.
    pub version: Option<String>,
    pub notes: Option<String>,
    pub message: Option<String>,
    pub last_checked_ms: Option<i64>,
}

#[derive(Default)]
pub struct Updates {
    status: Mutex<UpdateStatus>,
    /// A downloaded, signature-checked release waiting to be installed.
    pending: Mutex<Option<(Update, Vec<u8>)>>,
    /// Serialises checks so the timer and a manual check never overlap.
    check_lock: tauri::async_runtime::Mutex<()>,
}

impl Updates {
    pub fn status(&self) -> UpdateStatus {
        self.status.lock().map(|status| status.clone()).unwrap_or_default()
    }

    fn set(&self, app: &AppHandle, update: impl FnOnce(&mut UpdateStatus)) {
        let snapshot = match self.status.lock() {
            Ok(mut status) => {
                update(&mut status);
                status.current_version = app.package_info().version.to_string();
                status.clone()
            }
            Err(_) => return,
        };
        let _ = app.emit(UPDATE_STATUS_EVENT, snapshot);
    }
}

/// Starts the periodic check. Release builds only: a development build would
/// keep offering the published version.
pub fn start(app: &AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("murmela-updates".into())
        .spawn(move || {
            std::thread::sleep(FIRST_CHECK_DELAY);
            loop {
                if automatic_checks_enabled(&app) {
                    if let Err(error) = tauri::async_runtime::block_on(check(&app)) {
                        eprintln!("[updates] {error:#}");
                    }
                }
                std::thread::sleep(CHECK_INTERVAL);
            }
        });
}

fn automatic_checks_enabled(app: &AppHandle) -> bool {
    app.try_state::<AppState>()
        .and_then(|state| crate::storage::get_settings(state.inner()).ok())
        .is_some_and(|settings| settings.auto_update_enabled)
}

/// Looks for a newer release and downloads it. Returns the resulting status;
/// a failed check is reported in the status rather than as an error.
pub async fn check(app: &AppHandle) -> Result<UpdateStatus> {
    let updates = app.state::<Updates>();
    let _guard = updates.check_lock.lock().await;
    if updates.status().state == UpdateState::Ready {
        return Ok(updates.status());
    }
    updates.set(app, |status| {
        status.state = UpdateState::Checking;
        status.message = None;
    });
    match download_newer_release(app, &updates).await {
        Ok(found) => updates.set(app, |status| {
            status.last_checked_ms = Some(chrono::Utc::now().timestamp_millis());
            if !found {
                status.state = UpdateState::UpToDate;
                status.version = None;
                status.notes = None;
            }
        }),
        Err(error) => {
            eprintln!("[updates] check failed: {error:#}");
            updates.set(app, |status| {
                status.state = UpdateState::Failed;
                status.last_checked_ms = Some(chrono::Utc::now().timestamp_millis());
                status.message =
                    Some("Murmela could not check for updates. It will try again later.".into());
            });
        }
    }
    Ok(updates.status())
}

async fn download_newer_release(app: &AppHandle, updates: &Updates) -> Result<bool> {
    let db_path = app
        .try_state::<AppState>()
        .map(|state| state.db_path.clone())
        .context("the network activity log is unavailable")?;
    let mut connection =
        network_activity::Connection::begin(&db_path, Destination::GitHub, "Update check");
    let endpoints = feed_urls(app);
    if endpoints.is_empty() {
        return Err(anyhow!("no release feed is configured"));
    }
    for endpoint in endpoints {
        connection.record_request(&endpoint)?;
    }
    let found = app
        .updater()
        .context("updater unavailable")?
        .check()
        .await
        .context("could not read the release feed")?;
    connection.finish(Outcome::Completed);
    let Some(update) = found else {
        return Ok(false);
    };
    let version = update.version.clone();
    let notes = update.body.clone();
    updates.set(app, |status| {
        status.state = UpdateState::Downloading;
        status.version = Some(version.clone());
        status.notes = notes.clone();
    });
    let mut connection = network_activity::Connection::begin(
        &db_path,
        Destination::GitHub,
        format!("Download Murmela {version}"),
    );
    connection.record_request(update.download_url.as_str())?;
    let bytes = update
        .download(|chunk, _| connection.add_bytes(chunk as u64), || {})
        .await
        .with_context(|| format!("could not download Murmela {version}"))?;
    connection.finish(Outcome::Completed);
    *updates
        .pending
        .lock()
        .map_err(|_| anyhow!("update state unavailable"))? = Some((update, bytes));
    updates.set(app, |status| status.state = UpdateState::Ready);
    Ok(true)
}

/// Installs the downloaded release over the running app and restarts.
pub fn install_and_restart(app: &AppHandle) -> Result<()> {
    if !install_pending(app)? {
        return Err(anyhow!("No downloaded update is waiting to be installed."));
    }
    crate::shutdown::request_exit(app, crate::shutdown::ExitAction::Restart);
    Ok(())
}

/// Installs a downloaded release when Murmela quits normally.
pub fn install_pending_on_exit(app: &AppHandle) {
    if let Err(error) = install_pending(app) {
        eprintln!("[updates] could not install the downloaded update: {error:#}");
    }
}

fn install_pending(app: &AppHandle) -> Result<bool> {
    let Some(updates) = app.try_state::<Updates>() else {
        return Ok(false);
    };
    let Some((update, bytes)) = updates
        .pending
        .lock()
        .map_err(|_| anyhow!("update state unavailable"))?
        .take()
    else {
        return Ok(false);
    };
    if let Err(error) = update.install(&bytes) {
        updates.set(app, |status| {
            status.state = UpdateState::Failed;
            status.message = Some(
                "The update could not be installed. Move Murmela to your Applications folder and try again.".into(),
            );
        });
        return Err(anyhow!(error).context(format!("installing Murmela {}", update.version)));
    }
    Ok(true)
}

/// The release feeds from `plugins.updater.endpoints` in tauri.conf.json.
fn feed_urls(app: &AppHandle) -> Vec<String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|updater| updater.get("endpoints"))
        .and_then(|endpoints| endpoints.as_array())
        .map(|endpoints| {
            endpoints
                .iter()
                .filter_map(|endpoint| endpoint.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

