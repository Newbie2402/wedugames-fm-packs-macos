use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub crc: u32,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackageState {
    pub version: String,
    /// install root -> sha256 of every chunk applied there
    pub roots: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub license_key: Option<String>,
    pub game_dir: Option<PathBuf>,
    pub last_build_id: Option<String>,
    /// Where downloads wait until installed, when the customer picked a folder.
    #[serde(default)]
    pub temp_dir: Option<PathBuf>,
    /// The customer's Football Manager folder in Documents, when auto-detection
    /// missed it (unusual OneDrive setups).
    #[serde(default)]
    pub user_dir: Option<PathBuf>,
}

/// Everything the tool keeps, under `%LOCALAPPDATA%\FMUpdater`.
#[derive(Clone, Debug)]
pub struct Store {
    pub root: PathBuf,
}

impl Store {
    pub fn default_location() -> Result<Self> {
        let base = dirs::data_local_dir().context("Folder AppData tidak ditemukan")?;
        Ok(Self { root: base.join("FMUpdater") })
    }

    pub fn cache_dir(&self) -> PathBuf {
        match self.load_meta().ok().and_then(|meta| meta.temp_dir) {
            Some(dir) => dir.join("FMUpdaterTemp"),
            None => self.root.join("cache"),
        }
    }

    pub fn cover_dir(&self) -> PathBuf {
        self.root.join("covers")
    }

    fn pkg_dir(&self, id: &str) -> PathBuf {
        self.root.join("packages").join(id)
    }

    pub fn backup_dir(&self, id: &str, root: &str) -> PathBuf {
        let hash = hex::encode(Sha256::digest(root.as_bytes()));
        self.root.join("backup").join(id).join(&hash[..16])
    }

    fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
        match std::fs::read(path) {
            Ok(bytes) => {
                if bytes.is_empty() {
                    return Ok(None);
                }
                match serde_json::from_slice(&bytes) {
                    Ok(val) => Ok(Some(val)),
                    Err(e) => {
                        eprintln!("Peringatan: format JSON di {} tidak valid ({}), diabaikan", path.display(), e);
                        Ok(None)
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Write then rename, so a crash never leaves half a JSON file.
    fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        {
            use std::io::Write;
            let bytes = serde_json::to_vec(value)?;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load_meta(&self) -> Result<Meta> {
        Ok(Self::read(&self.root.join("meta.json"))?.unwrap_or_default())
    }

    pub fn save_meta(&self, meta: &Meta) -> Result<()> {
        Self::write(&self.root.join("meta.json"), meta)
    }

    pub fn load_package(&self, id: &str) -> Result<Option<PackageState>> {
        Self::read(&self.pkg_dir(id).join("state.json"))
    }

    pub fn save_package(&self, id: &str, state: &PackageState) -> Result<()> {
        Self::write(&self.pkg_dir(id).join("state.json"), state)
    }

    pub fn load_files(&self, id: &str, sha: &str) -> Result<Vec<FileEntry>> {
        Ok(Self::read(&self.pkg_dir(id).join("files").join(format!("{sha}.json")))?.unwrap_or_default())
    }

    pub fn save_files(&self, id: &str, sha: &str, files: &[FileEntry]) -> Result<()> {
        Self::write(&self.pkg_dir(id).join("files").join(format!("{sha}.json")), &files)
    }

    /// Deletes file lists no root refers to any more.
    pub fn prune_files(&self, id: &str, state: &PackageState) -> Result<()> {
        let live: BTreeSet<&String> = state.roots.values().flatten().collect();
        let Ok(entries) = std::fs::read_dir(self.pkg_dir(id).join("files")) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let sha = name.to_string_lossy().trim_end_matches(".json").to_string();
            if !live.contains(&sha) {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    pub fn forget_package(&self, id: &str) -> Result<()> {
        for dir in [self.pkg_dir(id), self.root.join("backup").join(id)] {
            if dir.exists() {
                std::fs::remove_dir_all(dir)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_empty_or_corrupt_files_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store { root: dir.path().to_path_buf() };

        // Missing file
        let meta = store.load_meta().unwrap();
        assert_eq!(meta.license_key, None);

        // 0-byte file (simulates abrupt shutdown)
        std::fs::write(dir.path().join("meta.json"), b"").unwrap();
        let meta = store.load_meta().unwrap();
        assert_eq!(meta.license_key, None);

        // Invalid JSON content
        std::fs::write(dir.path().join("meta.json"), b"{ invalid json").unwrap();
        let meta = store.load_meta().unwrap();
        assert_eq!(meta.license_key, None);

        // Normal write and read
        let mut new_meta = Meta::default();
        new_meta.license_key = Some("TEST-KEY".into());
        store.save_meta(&new_meta).unwrap();
        let loaded = store.load_meta().unwrap();
        assert_eq!(loaded.license_key.as_deref(), Some("TEST-KEY"));
    }
}

