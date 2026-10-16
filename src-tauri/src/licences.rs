//! The licence texts that ship with Murmela (`licenses/` and THIRD_PARTY_NOTICES.md
//! in the app resources), for Settings → About.
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::Serialize;
use tauri::{AppHandle, Manager};

const NOTICES: &str = "THIRD_PARTY_NOTICES.md";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenceDocument {
    /// File name; also the key for `read_document`.
    pub name: String,
    pub size_bytes: u64,
}

/// Packaged resources first; debug builds fall back to the repository files.
fn roots(app: &AppHandle) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(resources) = app.path().resource_dir() {
        roots.push(resources);
    }
    if cfg!(debug_assertions) {
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    }
    roots
}

fn documents_in(root: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    // The repository keeps the notices one level above src-tauri.
    for notices in [root.join(NOTICES), root.join("..").join(NOTICES)] {
        if notices.is_file() {
            found.push((NOTICES.to_string(), notices));
            break;
        }
    }
    if let Ok(entries) = std::fs::read_dir(root.join("licenses")) {
        let mut files: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .filter_map(|path| Some((path.file_name()?.to_str()?.to_string(), path)))
            .filter(|(name, _)| !name.starts_with('.'))
            .collect();
        files.sort_by_key(|(name, _)| name.to_lowercase());
        found.extend(files);
    }
    found
}

fn documents(app: &AppHandle) -> Vec<(String, PathBuf)> {
    roots(app)
        .iter()
        .map(|root| documents_in(root))
        .find(|found| !found.is_empty())
        .unwrap_or_default()
}

pub fn list_documents(app: &AppHandle) -> Vec<LicenceDocument> {
    documents(app)
        .into_iter()
        .map(|(name, path)| LicenceDocument {
            size_bytes: std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0),
            name,
        })
        .collect()
}

/// Only names from `list_documents` are readable, so no path can be smuggled in.
pub fn read_document(app: &AppHandle, name: &str) -> Result<String> {
    let (_, path) = documents(app)
        .into_iter()
        .find(|(candidate, _)| candidate == name)
        .ok_or_else(|| anyhow!("Unknown licence document: {name}"))?;
    Ok(std::fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_licences_are_found_with_notices_first() {
        let found = documents_in(Path::new(env!("CARGO_MANIFEST_DIR")));
        assert_eq!(found.first().map(|(name, _)| name.as_str()), Some(NOTICES));
        for required in [
            "Apache-2.0.txt",
            "CC-BY-4.0.txt",
            "OpenMDW-1.1.txt",
            "Gemma-Terms-of-Use.txt",
            "THIRD-PARTY-LIBRARIES.txt",
        ] {
            assert!(found.iter().any(|(name, _)| name == required), "{required} missing");
        }
    }
}
