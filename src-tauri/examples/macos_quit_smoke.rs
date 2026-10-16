//! Native integration probe. Runs without workspace windows or user data.
//! cargo run --example macos_quit_smoke -- --native
//! cargo run --example macos_quit_smoke -- --tauri
fn main() {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use murmela_lib::shutdown::{self, ExitAction};
    let native = std::env::args().any(|arg| arg == "--native");
    assert!(
        native || std::env::args().any(|arg| arg == "--tauri"),
        "choose --native or --tauri"
    );
    let mut context = tauri::generate_context!();
    context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            shutdown::install_macos_termination_handler(app.handle())?;
            let app = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                let target = app.clone();
                app.run_on_main_thread(move || {
                    if native {
                        eprintln!("[quit-smoke] requesting AppKit terminate (Cmd-Q/Dock path)");
                        // SAFETY: invoked on AppKit's main thread.
                        unsafe {
                            let nsapp: *mut AnyObject =
                                msg_send![class!(NSApplication), sharedApplication];
                            let _: () = msg_send![nsapp, terminate: std::ptr::null::<AnyObject>()];
                        }
                    } else {
                        eprintln!("[quit-smoke] requesting Tauri exit (tray path)");
                        target.exit(0);
                    }
                })
                .unwrap();
            });
            Ok(())
        })
        .build(context)
        .unwrap()
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { api, .. } if !shutdown::ready_to_exit() => {
                api.prevent_exit();
                shutdown::request_exit(app, ExitAction::Quit);
            }
            tauri::RunEvent::Exit => {
                assert!(
                    shutdown::ready_to_exit(),
                    "exit must follow completed cleanup"
                );
                eprintln!("[quit-smoke] graceful exit reached after cleanup");
            }
            _ => {}
        });
}
