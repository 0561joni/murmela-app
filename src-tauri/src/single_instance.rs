//! One desktop instance per user, including simultaneous launches and app copies.
//! The OS lock owns the instance; the loopback socket only requests activation.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tauri::{plugin::TauriPlugin, Manager, RunEvent};

const LOCK_FILE: &str = "desktop-instance.lock";
const ENDPOINT_FILE: &str = "desktop-instance.json";
const IO_TIMEOUT: Duration = Duration::from_millis(250);
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(3);

type ManagedInstance = Mutex<Option<InstanceGuard>>;

pub fn init() -> TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("single-instance")
        .setup(|app, _| {
            let activation_app = app.clone();
            let instance = acquire(&app.path().app_local_data_dir()?, move || {
                let app = activation_app.clone();
                if let Err(error) = activation_app.run_on_main_thread(move || {
                    if let Err(error) = crate::desktop_shell::show_main_window(&app) {
                        eprintln!("[desktop] could not restore existing Murmela window: {error:#}");
                    }
                }) {
                    eprintln!("[desktop] could not dispatch window activation: {error}");
                }
            })?;
            let Some(instance) = instance else {
                // No other plugins, workspace state, audio or workers have started.
                std::process::exit(0);
            };
            app.manage(Mutex::new(Some(instance)));
            Ok(())
        })
        .on_event(|app, event| {
            if matches!(event, RunEvent::Exit) {
                release(app);
            }
        })
        .on_drop(|app| release(&app))
        .build()
}

fn release(app: &tauri::AppHandle) {
    if let Some(instance) = app.try_state::<ManagedInstance>() {
        if let Ok(mut instance) = instance.lock() {
            // Tauri starts the replacement process before terminating this one.
            // Release on actual Exit, never on a cancellable ExitRequested.
            instance.take();
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Endpoint {
    port: u16,
    token: [u8; 16],
}

struct InstanceGuard {
    _lock: File,
    address: SocketAddr,
    stopped: Arc<AtomicBool>,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Wake the blocking accept so the listener and its app handle are released.
        let _ = TcpStream::connect_timeout(&self.address, IO_TIMEOUT);
        // Closing _lock releases the OS lock, even after a crash. Never unlink it:
        // replacing its inode would allow independent locks on the same pathname.
    }
}

fn acquire(
    directory: &Path,
    activate: impl Fn() + Send + 'static,
) -> Result<Option<InstanceGuard>> {
    fs::create_dir_all(directory).context("could not create instance directory")?;
    let lock = private_file(&directory.join(LOCK_FILE), false)?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            if let Err(error) = notify_existing(directory) {
                // Failure to focus must never allow a second desktop instance.
                eprintln!("[desktop] Murmela is already running; could not activate it: {error:#}");
            }
            return Ok(None);
        }
        Err(error) => return Err(error).context("could not acquire desktop instance lock"),
    }

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let address = listener.local_addr()?;
    let endpoint = Endpoint {
        port: address.port(),
        token: *uuid::Uuid::new_v4().as_bytes(),
    };
    // A contender retries while the owner is still publishing this small record.
    serde_json::to_writer(
        private_file(&directory.join(ENDPOINT_FILE), true)?,
        &endpoint,
    )?;

    let stopped = Arc::new(AtomicBool::new(false));
    let listener_stopped = stopped.clone();
    std::thread::Builder::new()
        .name("murmela-activation".into())
        .spawn(move || {
            for stream in listener.incoming() {
                if listener_stopped.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                if stream.set_read_timeout(Some(IO_TIMEOUT)).is_err() {
                    continue;
                }
                let mut token = [0; 16];
                if stream.read_exact(&mut token).is_ok()
                    && token == endpoint.token
                    && !listener_stopped.load(Ordering::SeqCst)
                {
                    activate();
                }
            }
        })?;
    Ok(Some(InstanceGuard {
        _lock: lock,
        address,
        stopped,
    }))
}

fn private_file(path: &Path, truncate: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .create(true)
        .read(true)
        .write(true)
        .truncate(truncate);
    options.mode(0o600);
    options.open(path)
}

fn notify_existing(directory: &Path) -> Result<()> {
    let deadline = Instant::now() + ACTIVATION_TIMEOUT;
    loop {
        let attempt = (|| -> Result<()> {
            let endpoint: Endpoint =
                serde_json::from_reader(File::open(directory.join(ENDPOINT_FILE))?.take(1024))?;
            let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
            let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
            stream.set_write_timeout(Some(IO_TIMEOUT))?;
            stream.write_all(&endpoint.token)?;
            Ok(())
        })();
        match attempt {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error),
            Err(_) => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;

    const CHILD_DIRECTORY: &str = "MURMELA_SINGLE_INSTANCE_TEST_DIRECTORY";
    const CHILD_ID: &str = "MURMELA_SINGLE_INSTANCE_TEST_ID";
    const TEST_TIMEOUT: Duration = Duration::from_secs(20);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "murmela-single-instance-test-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct TestProcess(Child);

    impl TestProcess {
        fn spawn(directory: &Path, id: usize) -> Self {
            Self(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "single_instance::tests::instance_process",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env(CHILD_DIRECTORY, directory)
                    .env(CHILD_ID, id.to_string())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            )
        }

        fn kill_and_wait(&mut self) {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }

    impl Drop for TestProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool, description: &str) {
        let deadline = Instant::now() + TEST_TIMEOUT;
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // Executed only by the concurrency test in a separate process. Using the
    // test executable exercises real OS locks without starting Tauri or audio.
    #[test]
    #[ignore = "subprocess helper for the single-instance concurrency test"]
    fn instance_process() {
        let directory = PathBuf::from(std::env::var_os(CHILD_DIRECTORY).unwrap());
        let id = std::env::var(CHILD_ID).unwrap();
        fs::write(directory.join(format!("ready-{id}")), b"").unwrap();
        wait_until(|| directory.join("start").exists(), "launch barrier");

        let activated = directory.join("activated");
        let instance = acquire(&directory, move || {
            fs::write(&activated, b"").unwrap();
        })
        .unwrap();
        fs::write(
            directory.join(format!("outcome-{id}")),
            if instance.is_some() {
                "primary"
            } else {
                "secondary"
            },
        )
        .unwrap();
        if instance.is_some() {
            wait_until(|| directory.join("stop").exists(), "owner shutdown");
        }
    }

    #[test]
    fn concurrent_processes_have_one_owner_and_recover_after_crash() {
        let directory = TestDirectory::new();
        let mut processes: Vec<_> = (0..4)
            .map(|id| TestProcess::spawn(&directory.0, id))
            .collect();
        wait_until(
            || (0..processes.len()).all(|id| directory.0.join(format!("ready-{id}")).exists()),
            "contenders to be ready",
        );
        fs::write(directory.0.join("start"), b"").unwrap();
        wait_until(
            || {
                (0..processes.len()).all(|id| {
                    matches!(
                        fs::read_to_string(directory.0.join(format!("outcome-{id}"))).as_deref(),
                        Ok("primary" | "secondary")
                    )
                })
            },
            "concurrent acquisition results",
        );

        let owners: Vec<_> = (0..processes.len())
            .filter(|id| {
                fs::read_to_string(directory.0.join(format!("outcome-{id}"))).unwrap() == "primary"
            })
            .collect();
        assert_eq!(owners.len(), 1, "simultaneous launches must have one owner");
        wait_until(
            || directory.0.join("activated").exists(),
            "duplicate launch to activate the owner",
        );

        let endpoint_path = directory.0.join(ENDPOINT_FILE);
        let stale_endpoint = fs::read(&endpoint_path).unwrap();
        processes[owners[0]].kill_and_wait();
        assert_eq!(fs::read(&endpoint_path).unwrap(), stale_endpoint);

        let replacement = acquire(&directory.0, || {}).unwrap().unwrap();
        assert_ne!(fs::read(&endpoint_path).unwrap(), stale_endpoint);
        drop(replacement);
        assert!(directory.0.join(LOCK_FILE).exists());
        let restarted = acquire(&directory.0, || {}).unwrap().unwrap();
        drop(restarted);
    }

    #[test]
    fn duplicate_launch_activates_the_existing_owner() {
        let directory = TestDirectory::new();
        let (activated, activation) = mpsc::channel();
        let _owner = acquire(&directory.0, move || {
            activated.send(()).unwrap();
        })
        .unwrap()
        .unwrap();
        let (unexpected, unexpected_activation) = mpsc::channel();

        let duplicate = acquire(&directory.0, move || {
            let _ = unexpected.send(());
        })
        .unwrap();

        assert!(duplicate.is_none());
        activation.recv_timeout(TEST_TIMEOUT).unwrap();
        assert!(unexpected_activation.try_recv().is_err());
    }

    #[test]
    fn unavailable_activation_does_not_admit_a_second_instance() {
        let directory = TestDirectory::new();
        let lock = private_file(&directory.0.join(LOCK_FILE), false).unwrap();
        lock.try_lock_exclusive().unwrap();
        // Model a primary process that owns its lock but cannot publish or
        // service activation. Failure to focus it must never bypass the lock.
        assert!(!directory.0.join(ENDPOINT_FILE).exists());
        let duplicate = acquire(&directory.0, || panic!("secondary became an owner")).unwrap();
        assert!(duplicate.is_none());

        drop(lock);
        assert!(acquire(&directory.0, || {}).unwrap().is_some());
    }
}
