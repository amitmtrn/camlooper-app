//! Persistence of the one pairing this desktop holds: `<app_data_dir>/remote.json`.
//! The file contains a bearer token, so it is written atomically and (on Unix) with mode 0600.
//! Its absence *is* the unlinked state — when it does not exist, the remote module performs
//! no network activity.
use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

pub const STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pairing {
    pub version: u32,
    pub pair_id: String,
    pub desktop_token: String,
    pub desktop_name: String,
    #[serde(default)]
    pub mobile_device_name: Option<String>,
    pub paired_at: String,
}

pub struct Store {
    path: PathBuf,
}

impl Store {
    pub fn at(path: PathBuf) -> Self {
        Store { path }
    }

    #[cfg(test)]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// `None` when there is no pairing. A corrupt file is deleted and treated as unlinked
    /// rather than wedging the app.
    pub fn load(&self) -> Option<Pairing> {
        let bytes = fs::read(&self.path).ok()?;
        match serde_json::from_slice::<Pairing>(&bytes) {
            Ok(p) if p.version == STORE_VERSION && !p.pair_id.is_empty() && !p.desktop_token.is_empty() => Some(p),
            _ => {
                eprintln!("remote: ignoring unreadable {}", self.path.display());
                let _ = fs::remove_file(&self.path);
                None
            }
        }
    }

    pub fn save(&self, pairing: &Pairing) -> anyhow::Result<()> {
        let dir = self.path.parent().context("remote.json has no parent directory")?;
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = self.path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(pairing)?;
        fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, &self.path).with_context(|| format!("replacing {}", self.path.display()))?;
        Ok(())
    }

    pub fn delete(&self) -> anyhow::Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("deleting {}", self.path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Pairing {
        Pairing {
            version: STORE_VERSION,
            pair_id: "7f2c1b4e-0000-4000-8000-000000000001".into(),
            desktop_token: "tok".into(),
            desktop_name: "box".into(),
            mobile_device_name: Some("Pixel".into()),
            paired_at: "2026-11-08T10:00:00Z".into(),
        }
    }

    #[test]
    fn round_trip_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("nested").join("remote.json"));
        assert!(store.load().is_none());
        store.save(&sample()).unwrap();
        assert_eq!(store.load(), Some(sample()));
        assert!(!dir.path().join("nested").join("remote.json.tmp").exists());
        store.delete().unwrap();
        assert!(store.load().is_none());
        store.delete().unwrap(); // idempotent
    }

    #[test]
    fn corrupt_file_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote.json");
        fs::write(&path, b"{not json").unwrap();
        let store = Store::at(path.clone());
        assert!(store.load().is_none());
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().join("remote.json"));
        store.save(&sample()).unwrap();
        let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
