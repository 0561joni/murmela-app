//! Background updates from the public `murmela-releases` repository.
//!
//! Murmela checks `latest.json` (see `plugins.updater` in tauri.conf.json)
//! shortly after launch and then every few hours, while
//! `auto_update_enabled` is on. A newer release is only announced; it is
//! downloaded when the user asks, and its signature checked against the public
//! key in the app. The user then chooses "Restart to update"; otherwise the
//! update is installed when Murmela quits. Nothing is installed while Murmela
//! is running. Each check and download is logged in Settings → Network activity.
//!
//! Installing doesn't need admin rights when the app bundle is the user's (see
//! self_install.rs). Where it can't be installed, the status says so and the
//! user can put the release into ~/Applications instead.
//!
//! A Pro key unlocks only releases from its year of updates (see license.rs).
//! When a newer release would turn Pro off, it isn't downloaded: the status says
//! so, and the user chooses to renew or to install it anyway as Free.

use std::sync::atomic::{AtomicBool, Ordering};
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
    /// A newer release exists and waits for the user to download it.
    Available,
    Downloading,
    Ready,
    /// The downloaded release couldn't replace this copy of Murmela.
    CannotInstall,
    Failed,
    /// A newer release exists, but this Mac's Pro key doesn't cover it.
    NeedsRenewal,
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
    /// While downloading: percent done, when the size is known.
    pub progress: Option<f64>,
    /// With `NeedsRenewal`: the last day of the Pro key's update year (YYYY-MM-DD).
    pub pro_updates_until: Option<String>,
}

#[derive(Default)]
pub struct Updates {
    status: Mutex<UpdateStatus>,
    /// A newer release found by the last check, not downloaded yet.
    found: Mutex<Option<Update>>,
    /// A downloaded, signature-checked release waiting to be installed.
    pending: Mutex<Option<(Update, Vec<u8>)>>,
    /// Serialises checks so the timer and a manual check never overlap.
    check_lock: tauri::async_runtime::Mutex<()>,
    /// The user chose to install a release that Pro doesn't cover, as Free.
    install_as_free: AtomicBool,
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

/// Downloads the release that Pro doesn't cover after the user chose to run it as Free.
pub async fn download_as_free(app: &AppHandle) -> Result<UpdateStatus> {
    app.state::<Updates>().install_as_free.store(true, Ordering::SeqCst);
    check(app).await?;
    download(app).await
}

/// Downloads the release found by the last check.
pub async fn download(app: &AppHandle) -> Result<UpdateStatus> {
    let updates = app.state::<Updates>();
    let _guard = updates.check_lock.lock().await;
    let Some(update) = updates
        .found
        .lock()
        .map_err(|_| anyhow!("update state unavailable"))?
        .take()
    else {
        return Ok(updates.status());
    };
    if let Err(error) = download_release(app, &updates, update.clone()).await {
        eprintln!("[updates] download failed: {error:#}");
        // Keep the release on offer so the user can try again.
        if let Ok(mut found) = updates.found.lock() {
            *found = Some(update);
        }
        updates.set(app, |status| {
            status.state = UpdateState::Available;
            status.progress = None;
            status.message = Some("The download stopped. Check your connection and try again.".into());
        });
    }
    Ok(updates.status())
}

/// Looks for a newer release; `download` fetches it. Returns the resulting status;
/// a failed check is reported in the status rather than as an error.
pub async fn check(app: &AppHandle) -> Result<UpdateStatus> {
    let updates = app.state::<Updates>();
    let _guard = updates.check_lock.lock().await;
    if matches!(
        updates.status().state,
        UpdateState::Downloading | UpdateState::Ready | UpdateState::CannotInstall
    ) {
        return Ok(updates.status());
    }
    updates.set(app, |status| {
        status.state = UpdateState::Checking;
        status.message = None;
    });
    match find_newer_release(app, &updates).await {
        Ok(found) => updates.set(app, |status| {
            status.last_checked_ms = Some(chrono::Utc::now().timestamp_millis());
            if !found {
                status.state = UpdateState::UpToDate;
                status.version = None;
                status.notes = None;
                status.pro_updates_until = None;
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

async fn find_newer_release(app: &AppHandle, updates: &Updates) -> Result<bool> {
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
    let published = update
        .date
        .and_then(|date| chrono::DateTime::from_timestamp(date.unix_timestamp(), 0))
        .map(|date| date.date_naive());
    let as_free = updates.install_as_free.swap(false, Ordering::SeqCst);
    if let Some(until) = crate::license::update_ends_pro(&db_path, published).filter(|_| !as_free) {
        updates.set(app, |status| {
            status.state = UpdateState::NeedsRenewal;
            status.version = Some(version.clone());
            status.notes = notes.clone();
            status.pro_updates_until = Some(until.to_string());
        });
        return Ok(true);
    }
    updates.set(app, |status| {
        status.state = UpdateState::Available;
        status.version = Some(version.clone());
        status.notes = notes.clone();
        status.pro_updates_until = None;
    });
    *updates
        .found
        .lock()
        .map_err(|_| anyhow!("update state unavailable"))? = Some(update);
    Ok(true)
}

async fn download_release(app: &AppHandle, updates: &Updates, update: Update) -> Result<()> {
    let db_path = app
        .try_state::<AppState>()
        .map(|state| state.db_path.clone())
        .context("the network activity log is unavailable")?;
    let version = update.version.clone();
    updates.set(app, |status| {
        status.state = UpdateState::Downloading;
        status.progress = None;
        status.message = None;
    });
    let mut connection = network_activity::Connection::begin(
        &db_path,
        Destination::GitHub,
        format!("Download Murmela {version}"),
    );
    connection.record_request(update.download_url.as_str())?;
    let mut received = 0u64;
    let mut last_percent = -1.0;
    let bytes = update
        .download(
            |chunk, total| {
                connection.add_bytes(chunk as u64);
                received += chunk as u64;
                if let Some(total) = total.filter(|total| *total > 0) {
                    let percent = (received as f64 * 100.0 / total as f64).min(100.0).floor();
                    if percent > last_percent {
                        last_percent = percent;
                        updates.set(app, |status| status.progress = Some(percent));
                    }
                }
            },
            || {},
        )
        .await
        .with_context(|| format!("could not download Murmela {version}"))?;
    connection.finish(Outcome::Completed);
    *updates
        .pending
        .lock()
        .map_err(|_| anyhow!("update state unavailable"))? = Some((update, bytes));
    updates.set(app, |status| {
        status.state = UpdateState::Ready;
        status.progress = None;
    });
    Ok(())
}

/// Installs the downloaded release over the running app and restarts. When
/// this copy can't be replaced, the status turns to `CannotInstall` and the
/// download is kept for another try or for `install_in_home_applications`.
pub fn install_and_restart(app: &AppHandle) -> Result<()> {
    if !install_pending(app)? {
        return Err(anyhow!("No downloaded update is waiting to be installed."));
    }
    crate::shutdown::request_exit(app, crate::shutdown::ExitAction::Restart);
    Ok(())
}

/// Installs a downloaded release when Murmela quits normally.
pub fn install_pending_on_exit(app: &AppHandle) {
    // Already known not to work here; trying again would only ask for a password at quit.
    if app.state::<Updates>().status().state == UpdateState::CannotInstall {
        return;
    }
    if let Err(error) = install_pending(app) {
        eprintln!("[updates] could not install the downloaded update: {error:#}");
    }
}

/// Puts the downloaded release into ~/Applications, which needs no admin
/// rights, then quits and opens that copy.
pub fn install_in_home_applications(app: &AppHandle) -> Result<()> {
    let updates = app.state::<Updates>();
    let pending = updates
        .pending
        .lock()
        .map_err(|_| anyhow!("update state unavailable"))?
        .take();
    let Some((update, bytes)) = pending else {
        return Err(anyhow!("No downloaded update is waiting to be installed."));
    };
    let current = crate::self_install::current_app_bundle()?;
    let name = current
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "Murmela.app".into());
    let folder = crate::self_install::home_applications()?;
    let installed = match crate::self_install::install_into(&folder, &name.to_string_lossy(), &bytes) {
        Ok(path) => path,
        Err(error) => {
            *updates
                .pending
                .lock()
                .map_err(|_| anyhow!("update state unavailable"))? = Some((update, bytes));
            return Err(error.context("could not install into your Applications folder"));
        }
    };
    // Open the new copy once this one has quit, so they don't run side by side.
    let pid = std::process::id();
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("while kill -0 \"$0\" 2>/dev/null; do sleep 0.2; done; /usr/bin/open \"$1\"")
        .arg(pid.to_string())
        .arg(&installed)
        .spawn()
        .context("could not open the new copy of Murmela")?;
    crate::shutdown::request_exit(app, crate::shutdown::ExitAction::Quit);
    Ok(())
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
    // First without admin rights: swap the inside of the user's own app bundle.
    let swapped = crate::self_install::current_app_bundle()
        .and_then(|bundle| crate::self_install::swap_contents(&bundle, &bytes));
    let Err(swap_error) = swapped else {
        return Ok(true);
    };
    eprintln!("[updates] in-place update unavailable: {swap_error:#}");
    // Then the updater's way, which asks for an administrator's password when needed.
    if let Err(error) = update.install(&bytes) {
        let version = update.version.clone();
        if let Ok(mut pending) = updates.pending.lock() {
            *pending = Some((update, bytes));
        }
        updates.set(app, |status| {
            status.state = UpdateState::CannotInstall;
            status.message = None;
        });
        return Err(anyhow!(error).context(format!("installing Murmela {version}")));
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

