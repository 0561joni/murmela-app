//! "Launch Murmela when you log in": a LaunchAgent in ~/Library/LaunchAgents.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tauri::AppHandle;

pub fn sync_launch_at_login(app: &AppHandle, enabled: bool) -> Result<()> {
    let identifier = app.config().identifier.clone();
    let plist_path = launch_agent_path(&identifier)?;
    if enabled {
        let executable = std::env::current_exe().context("failed to resolve current executable")?;
        if let Some(parent) = plist_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(plist_path, launch_agent_plist(&identifier, &executable))?;
    } else if plist_path.exists() {
        fs::remove_file(plist_path)?;
    }
    Ok(())
}

fn launch_agent_path(identifier: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{identifier}.plist")))
}

fn launch_agent_plist(identifier: &str, executable: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{identifier}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#,
        executable.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_starts_the_app_at_login() {
        let plist = launch_agent_plist(
            "com.murmela.app",
            Path::new("/Applications/Murmela.app/Contents/MacOS/murmela"),
        );
        assert!(plist.contains("<string>com.murmela.app</string>"));
        assert!(plist.contains("<string>/Applications/Murmela.app/Contents/MacOS/murmela</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
    }
}
