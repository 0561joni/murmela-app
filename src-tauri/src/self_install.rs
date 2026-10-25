//! Installs a downloaded release without administrator rights where possible.
//!
//! The updater plugin replaces the whole `Murmela.app` folder, which means
//! changing `/Applications`. Only administrators may do that, so on a standard
//! account (common on company Macs) it falls back to a password prompt that
//! the user can't answer. The app bundle itself, though, belongs to whoever
//! copied it there. Swapping the bundle's `Contents` folder for the new one
//! only touches the bundle, so it works without admin rights. The new
//! `Contents` is the complete, signed and notarized app from the release.
//!
//! When even that isn't allowed (an app installed by IT, for example), the
//! release can go into `~/Applications` instead, which every account may change.

use std::ffi::CString;
use std::io::Cursor;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

/// The `.app` bundle the running executable belongs to.
pub fn current_app_bundle() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("could not locate the running app")?;
    exe.ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("Murmela isn't running from an app bundle"))
}

/// Replaces `app/Contents` with the `Contents` of the release archive
/// (`Murmela.app.tar.gz` as published). The old `Contents` is deleted only
/// once the new one is in place; on any error the app is left as it was.
pub fn swap_contents(app: &Path, archive: &[u8]) -> Result<()> {
    let staging = tempfile::Builder::new()
        .prefix("murmela-update")
        .tempdir()
        .context("could not create a folder for the update")?;
    // Renames only work within one file system.
    let app_device = std::fs::metadata(app)?.dev();
    if std::fs::metadata(staging.path())?.dev() != app_device {
        bail!("the update folder is on another disk than Murmela");
    }
    let new_contents = unpack_contents(archive, staging.path())?;
    let current = app.join("Contents");
    if !swap(&new_contents, &current)? {
        // No atomic swap on this file system: two renames, undone on failure.
        let backup = staging.path().join("previous-Contents");
        std::fs::rename(&current, &backup)?;
        if let Err(error) = std::fs::rename(&new_contents, &current) {
            let _ = std::fs::rename(&backup, &current);
            return Err(error.into());
        }
    }
    touch(app);
    // Dropping `staging` deletes the previous Contents.
    Ok(())
}

/// Installs the release as `folder/Murmela.app`, creating the folder if
/// needed. An existing copy there is updated in place. Returns the app path.
pub fn install_into(folder: &Path, app_name: &str, archive: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(folder).with_context(|| format!("could not create {}", folder.display()))?;
    let target = folder.join(app_name);
    if target.exists() {
        swap_contents(&target, archive)?;
        return Ok(target);
    }
    let staging = tempfile::Builder::new()
        .prefix(".murmela-install")
        .tempdir_in(folder)
        .context("could not prepare the new copy")?;
    let contents = unpack_contents(archive, staging.path())?;
    let bundle = staging.path().join(app_name);
    std::fs::create_dir(&bundle)?;
    std::fs::rename(&contents, bundle.join("Contents"))?;
    std::fs::rename(&bundle, &target)?;
    touch(&target);
    Ok(target)
}

/// The Applications folder in the user's home folder.
pub fn home_applications() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("the home folder is unknown"))?;
    Ok(PathBuf::from(home).join("Applications"))
}

/// Unpacks the archive's single `<Name>.app/` folder into `into` and returns
/// the path of its `Contents`.
fn unpack_contents(archive: &[u8], into: &Path) -> Result<PathBuf> {
    let root = into.join("unpacked");
    std::fs::create_dir_all(&root)?;
    let mut entries = tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(archive)));
    entries.set_preserve_permissions(true);
    for entry in entries.entries().context("the update archive is unreadable")? {
        let mut entry = entry?;
        // Drop the leading `Murmela.app/`.
        let inner: PathBuf = entry.path()?.components().skip(1).collect();
        if inner.as_os_str().is_empty() {
            continue;
        }
        if !inner.components().all(|part| matches!(part, std::path::Component::Normal(_))) {
            bail!("the update archive contains an unexpected path");
        }
        let path = root.join(&inner);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&path).context("the update archive is damaged")?;
    }
    let contents = root.join("Contents");
    if !contents.join("Info.plist").is_file() || !contents.join("MacOS").is_dir() {
        bail!("the update archive doesn't contain an app");
    }
    Ok(contents)
}

/// Atomically swaps two paths. Returns false where the file system can't.
fn swap(a: &Path, b: &Path) -> Result<bool> {
    let a_c = CString::new(a.as_os_str().as_bytes())?;
    let b_c = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: both are valid NUL-terminated paths for the duration of the call.
    let result = unsafe { libc::renamex_np(a_c.as_ptr(), b_c.as_ptr(), libc::RENAME_SWAP) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOTSUP) | Some(libc::EINVAL) => Ok(false),
        _ => Err(error.into()),
    }
}

/// Lets Finder and Launch Services notice the new version.
fn touch(path: &Path) {
    let _ = std::process::Command::new("/usr/bin/touch").arg(path).status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A `Murmela.app.tar.gz` like the published one, whose Info.plist says `version`.
    fn release_archive(version: &str) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, body) in [
            ("Murmela.app/Contents/Info.plist", version.as_bytes()),
            ("Murmela.app/Contents/MacOS/murmela", b"binary".as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, body).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn installed_app(folder: &Path, version: &str) -> PathBuf {
        let app = folder.join("Murmela.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), version).unwrap();
        std::fs::write(app.join("Contents/MacOS/murmela"), "old binary").unwrap();
        app
    }

    fn set_mode(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The standard-account case: the app is the user's, `/Applications` isn't.
    #[test]
    fn updates_an_own_app_inside_a_locked_applications_folder() {
        let root = tempfile::tempdir().unwrap();
        let applications = root.path().join("Applications");
        let app = installed_app(&applications, "1.0");
        set_mode(&applications, 0o555);

        // What the updater plugin does first, and why it needs admin rights.
        let moved = std::fs::rename(&app, root.path().join("moved.app"));
        assert_eq!(moved.unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);

        swap_contents(&app, &release_archive("2.0")).unwrap();
        assert_eq!(std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap(), "2.0");
        assert_eq!(std::fs::read_to_string(app.join("Contents/MacOS/murmela")).unwrap(), "binary");
        assert_eq!(std::fs::read_dir(&app).unwrap().count(), 1, "nothing but Contents in the bundle");
        set_mode(&applications, 0o755);
    }

    #[test]
    fn leaves_the_app_untouched_when_the_archive_is_not_an_app() {
        let root = tempfile::tempdir().unwrap();
        let app = installed_app(root.path(), "1.0");
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_cksum();
        builder.append_data(&mut header, "Murmela.app/readme", b"oops".as_slice()).unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();

        assert!(swap_contents(&app, &archive).is_err());
        assert!(swap_contents(&app, b"not an archive").is_err());
        assert_eq!(std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap(), "1.0");
    }

    #[test]
    fn refuses_an_app_bundle_it_may_not_change() {
        let root = tempfile::tempdir().unwrap();
        let app = installed_app(root.path(), "1.0");
        set_mode(&app, 0o555);
        assert!(swap_contents(&app, &release_archive("2.0")).is_err());
        assert_eq!(std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap(), "1.0");
        set_mode(&app, 0o755);
    }

    #[test]
    fn installs_into_a_personal_applications_folder_and_updates_it_there() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("Applications");
        let app = install_into(&folder, "Murmela.app", &release_archive("2.0")).unwrap();
        assert_eq!(app, folder.join("Murmela.app"));
        assert_eq!(std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap(), "2.0");
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1, "no staging folder left behind");

        install_into(&folder, "Murmela.app", &release_archive("3.0")).unwrap();
        assert_eq!(std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap(), "3.0");
    }
}

/// Manual check against a real release: `MURMELA_SWAP_APP=<copy of Murmela.app>
/// MURMELA_SWAP_ARCHIVE=<Murmela_x.app.tar.gz> cargo test --lib -- --ignored real_release`.
#[cfg(test)]
#[test]
#[ignore]
fn real_release_swaps_into_a_real_app() {
    let app = PathBuf::from(std::env::var("MURMELA_SWAP_APP").unwrap());
    let archive = std::fs::read(std::env::var("MURMELA_SWAP_ARCHIVE").unwrap()).unwrap();
    swap_contents(&app, &archive).unwrap();
}
