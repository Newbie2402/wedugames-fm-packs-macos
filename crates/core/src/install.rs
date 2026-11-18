use crate::state::FileEntry;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn crc_of(path: &Path) -> io::Result<u32> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = crc32fast::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(hasher.finalize());
        }
        hasher.update(&buf[..n]);
    }
}

fn permission_hint(e: anyhow::Error, path: &Path) -> anyhow::Error {
    let denied = e
        .downcast_ref::<io::Error>()
        .is_some_and(|io| io.kind() == io::ErrorKind::PermissionDenied);
    if denied {
        #[cfg(target_os = "macos")]
        let hint = "Tutup FM, lalu beri izin menulis ke folder tersebut (System Settings > Privacy & Security, atau Folder & Disks)";
        #[cfg(not(target_os = "macos"))]
        let hint = "Tutup FM lalu jalankan tool sebagai Administrator";
        anyhow::anyhow!("Tidak bisa menulis ke {}. {hint}", path.display())
    } else {
        e
    }
}

fn copy_replacing(from: &Path, to: &Path) -> Result<()> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::copy(from, to)?;
    Ok(())
}

/// Extracts one chunk into `root`.
///
/// `owned` holds the files a previous install of this package put there. An
/// existing file that is not in it is the game's or the user's, so it goes to
/// `backup` first. That also holds after a game patch replaced one of our
/// files: the patched file becomes the new original.
pub fn apply_chunk(
    zip_path: &Path,
    root: &Path,
    owned: &HashMap<String, Vec<u32>>,
    backup: &Path,
    cancel: &AtomicBool,
    mut on_file: impl FnMut(),
) -> Result<Vec<FileEntry>> {
    let mut archive = zip::ZipArchive::new(BufReader::new(File::open(zip_path)?))
        .with_context(|| format!("Arsip rusak: {}", zip_path.display()))?;
    let mut entries = Vec::with_capacity(archive.len());

    for i in 0..archive.len() {
        if cancel.load(Ordering::Relaxed) {
            bail!("Dibatalkan");
        }
        let mut file = archive.by_index(i)?;
        if file.is_dir() {
            continue;
        }
        // enclosed_name rejects absolute paths and `..`.
        let Some(rel) = file.enclosed_name() else {
            bail!("Path berbahaya di arsip: {}", file.name());
        };
        let path = file.name().to_string();
        let crc = file.crc32();
        let dest = root.join(&rel);

        if dest.exists() {
            let current = crc_of(&dest)?;
            if current == crc {
                entries.push(FileEntry { path, size: file.size(), crc });
                on_file();
                continue;
            }
            if !owned.get(&path).is_some_and(|crcs| crcs.contains(&current)) {
                copy_replacing(&dest, &backup.join(&rel)).context("Gagal membuat backup")?;
            }
        }

        let mut write = || -> Result<()> {
            if let Some(dir) = dest.parent() {
                fs::create_dir_all(dir)?;
            }
            let tmp = dest.with_extension("fmup-tmp");
            {
                let mut out = BufWriter::new(File::create(&tmp)?);
                io::copy(&mut file, &mut out)?;
            }
            fs::rename(&tmp, &dest)?;
            Ok(())
        };
        write().map_err(|e| permission_hint(e, &dest))?;
        entries.push(FileEntry { size: dest.metadata()?.len(), path, crc });
        on_file();
    }
    Ok(entries)
}

/// Removes files we installed and puts their originals back. A file whose
/// content matches none of the versions we wrote is not ours any more, so it
/// stays. Patches mean one path can appear several times in `files`.
pub fn remove_files(root: &Path, files: &[FileEntry], backup: &Path) -> Result<()> {
    let mut versions: HashMap<&str, Vec<u32>> = HashMap::new();
    for entry in files {
        versions.entry(&entry.path).or_default().push(entry.crc);
    }
    for (path, crcs) in versions {
        let dest = root.join(path);
        let saved = backup.join(path);
        let ours = crc_of(&dest).is_ok_and(|crc| crcs.contains(&crc));
        if ours {
            fs::remove_file(&dest).map_err(|e| permission_hint(e.into(), &dest))?;
            if saved.exists() {
                copy_replacing(&saved, &dest)?;
            }
        }
        if saved.exists() {
            fs::remove_file(&saved)?;
        }
        prune_empty_dirs(dest.parent(), root);
    }
    Ok(())
}

fn prune_empty_dirs(mut dir: Option<&Path>, root: &Path) {
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || fs::remove_dir(d).is_err() {
            return;
        }
        dir = d.parent();
    }
}

/// `deep` compares content. The shallow check only looks at size, which is
/// what a pack of several hundred thousand images can afford.
pub fn verify_files(root: &Path, files: &[FileEntry], deep: bool) -> bool {
    files.iter().all(|entry| {
        let dest = root.join(&entry.path);
        if deep {
            crc_of(&dest).is_ok_and(|crc| crc == entry.crc)
        } else {
            dest.metadata().is_ok_and(|m| m.len() == entry.size)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_zip(path: &Path, files: &[(&str, &str)]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, body) in files {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    fn owned_by(files: &[FileEntry]) -> HashMap<String, Vec<u32>> {
        files.iter().map(|f| (f.path.clone(), vec![f.crc])).collect()
    }

    fn apply(zip: &Path, root: &Path, owned: &HashMap<String, Vec<u32>>, backup: &Path) -> Result<Vec<FileEntry>> {
        apply_chunk(zip, root, owned, backup, &AtomicBool::new(false), || {})
    }

    #[test]
    fn backs_up_then_restores_game_file() {
        let t = tempfile::tempdir().unwrap();
        let (zip, root, backup) = (t.path().join("a.zip"), t.path().join("root"), t.path().join("bak"));
        fs::create_dir_all(root.join("lnc")).unwrap();
        fs::write(root.join("lnc/a.lnc"), "original").unwrap();
        make_zip(&zip, &[("lnc/a.lnc", "fixed"), ("lnc/b.lnc", "new")]);

        let files = apply(&zip, &root, &HashMap::new(), &backup).unwrap();
        assert_eq!(fs::read_to_string(root.join("lnc/a.lnc")).unwrap(), "fixed");
        assert_eq!(fs::read_to_string(backup.join("lnc/a.lnc")).unwrap(), "original");

        remove_files(&root, &files, &backup).unwrap();
        assert_eq!(fs::read_to_string(root.join("lnc/a.lnc")).unwrap(), "original");
        assert!(!root.join("lnc/b.lnc").exists());
    }

    #[test]
    fn update_of_own_file_keeps_original_backup() {
        let t = tempfile::tempdir().unwrap();
        let (zip, root, backup) = (t.path().join("a.zip"), t.path().join("root"), t.path().join("bak"));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a"), "original").unwrap();
        make_zip(&zip, &[("a", "v1")]);
        let files = apply(&zip, &root, &HashMap::new(), &backup).unwrap();

        make_zip(&zip, &[("a", "v2")]);
        apply(&zip, &root, &owned_by(&files), &backup).unwrap();
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "v2");
        assert_eq!(fs::read_to_string(backup.join("a")).unwrap(), "original");
    }

    #[test]
    fn patched_file_becomes_new_original() {
        let t = tempfile::tempdir().unwrap();
        let (zip, root, backup) = (t.path().join("a.zip"), t.path().join("root"), t.path().join("bak"));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a"), "original").unwrap();
        make_zip(&zip, &[("a", "fixed")]);
        let files = apply(&zip, &root, &HashMap::new(), &backup).unwrap();

        fs::write(root.join("a"), "patched by steam").unwrap();
        apply(&zip, &root, &owned_by(&files), &backup).unwrap();
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "fixed");
        assert_eq!(fs::read_to_string(backup.join("a")).unwrap(), "patched by steam");
    }

    #[test]
    fn rejects_path_traversal() {
        let t = tempfile::tempdir().unwrap();
        let (zip, root, backup) = (t.path().join("a.zip"), t.path().join("root"), t.path().join("bak"));
        fs::create_dir_all(&root).unwrap();
        make_zip(&zip, &[("../evil.txt", "x")]);
        assert!(apply(&zip, &root, &HashMap::new(), &backup).is_err());
        assert!(!t.path().join("evil.txt").exists());
    }

    #[test]
    fn keeps_file_changed_after_install() {
        let t = tempfile::tempdir().unwrap();
        let (zip, root, backup) = (t.path().join("a.zip"), t.path().join("root"), t.path().join("bak"));
        make_zip(&zip, &[("a", "ours")]);
        let files = apply(&zip, &root, &HashMap::new(), &backup).unwrap();
        fs::write(root.join("a"), "edited by user").unwrap();
        remove_files(&root, &files, &backup).unwrap();
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "edited by user");
    }

    #[test]
    fn deep_verify_catches_same_size_change() {
        let t = tempfile::tempdir().unwrap();
        let (zip, root, backup) = (t.path().join("a.zip"), t.path().join("root"), t.path().join("bak"));
        make_zip(&zip, &[("a", "aaaa")]);
        let files = apply(&zip, &root, &HashMap::new(), &backup).unwrap();
        fs::write(root.join("a"), "bbbb").unwrap();
        assert!(verify_files(&root, &files, false));
        assert!(!verify_files(&root, &files, true));
    }
}
