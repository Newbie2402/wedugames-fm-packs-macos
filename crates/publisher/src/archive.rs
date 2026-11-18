//! Turns a folder of downloaded archives into pack files: oldest version first,
//! each one overlaid on the last, the way the pack makers tell people to.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const EXTENSIONS: [&str; 3] = ["zip", "rar", "7z"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Archive {
    pub path: PathBuf,
    pub name: String,
    /// `2026.11` from the file name, when it has one.
    pub version: Option<String>,
}

pub fn find_seven_zip() -> Option<PathBuf> {
    ["C:\\Program Files\\7-Zip\\7z.exe", "C:\\Program Files (x86)\\7-Zip\\7z.exe"]
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// The first `dddd.dd` in the name: `FMG Steel Logos 2026.05.zip`.
fn version_in(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    bytes.windows(7).position(|w| {
        w[..4].iter().all(u8::is_ascii_digit) && w[4] == b'.' && w[5..].iter().all(u8::is_ascii_digit)
    })
    .map(|at| name[at..at + 7].to_string())
}

/// Archives directly in `dir`, in the order they must be applied.
pub fn archives_in(dir: &Path) -> Result<Vec<Archive>> {
    let mut archives = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("Folder tidak bisa dibaca: {}", dir.display()))? {
        let path = entry?.path();
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
        if path.is_file() && EXTENSIONS.contains(&extension.as_str()) {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            archives.push(Archive { version: version_in(&name), name, path });
        }
    }
    archives.sort_by(|a, b| (&a.version, &a.name).cmp(&(&b.version, &b.name)));
    if archives.len() > 1 && archives.iter().any(|a| a.version.is_none()) {
        bail!("{} berisi arsip tanpa nomor versi. Tiap pack harus punya folder sendiri", dir.display());
    }
    Ok(archives)
}

fn run(mut command: Command, cancel: &AtomicBool) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().context("7-Zip tidak bisa dijalankan")?;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("7-Zip gagal membuka arsip (kode {})", status.code().unwrap_or(-1));
            }
            return Ok(());
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Dibatalkan");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Archives wrap everything in one folder, named after the version in most
/// packs. Without removing it an update would land beside the megapack.
fn content_root(extracted: &Path) -> Result<PathBuf> {
    let entries: Vec<PathBuf> = fs::read_dir(extracted)?.map(|e| Ok(e?.path())).collect::<Result<_>>()?;
    match entries.as_slice() {
        [only] if only.is_dir() => Ok(only.clone()),
        _ => Ok(extracted.to_path_buf()),
    }
}

/// Moves every file from `from` into `to`, replacing what is already there.
fn overlay(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            overlay(&entry.path(), &target)?;
        } else {
            fs::rename(entry.path(), &target)
                .with_context(|| format!("Gagal memindahkan {}", target.display()))?;
        }
    }
    Ok(())
}

/// Extracts `archive` over `dest`. `temp` must be on the same drive as `dest`.
pub fn extract_over(seven_zip: &Path, archive: &Archive, temp: &Path, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    if temp.exists() {
        fs::remove_dir_all(temp)?;
    }
    fs::create_dir_all(temp)?;
    let mut command = Command::new(seven_zip);
    command.arg("x").arg("-y").arg("-bd").arg("-bso0").arg("-bsp0");
    command.arg(format!("-o{}", temp.display())).arg(&archive.path);
    run(command, cancel).with_context(|| archive.name.clone())?;
    overlay(&content_root(temp)?, dest)?;
    fs::remove_dir_all(temp)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions_from_real_file_names() {
        let cases = [
            ("FMG Steel Logos 2026.05.zip", Some("2026.05")),
            ("metallic_logos_2026.02_changes.rar", Some("2026.02")),
            ("Cutout_2027.00.rar", Some("2027.00")),
            ("Megapack TCM26.rar", None),
            ("DF11 Classic.zip", None),
        ];
        for (name, version) in cases {
            assert_eq!(version_in(name).as_deref(), version, "{name}");
        }
    }

    #[test]
    fn sorts_oldest_first_and_ignores_other_files() {
        let t = tempfile::tempdir().unwrap();
        for name in ["FM Vapour Logos 2026.00.zip", "FMG Vapour Logos 2026.11.zip", "FMG Vapour Logos 2026.02.zip", "notes.txt"] {
            fs::write(t.path().join(name), "x").unwrap();
        }
        let names: Vec<_> = archives_in(t.path()).unwrap().into_iter().map(|a| a.version.unwrap()).collect();
        assert_eq!(names, ["2026.00", "2026.02", "2026.11"]);
    }

    #[test]
    fn refuses_two_unversioned_packs_in_one_folder() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("DF11 Classic.zip"), "x").unwrap();
        fs::write(t.path().join("FMSite.rar"), "x").unwrap();
        assert!(archives_in(t.path()).is_err());
    }

    #[test]
    fn overlay_replaces_and_keeps_the_rest() {
        let t = tempfile::tempdir().unwrap();
        let (from, to) = (t.path().join("from"), t.path().join("to"));
        fs::create_dir_all(from.join("Clubs/Normal")).unwrap();
        fs::create_dir_all(to.join("Clubs/Normal")).unwrap();
        fs::write(from.join("Clubs/Normal/1.png"), "new").unwrap();
        fs::write(to.join("Clubs/Normal/1.png"), "old").unwrap();
        fs::write(to.join("Clubs/Normal/2.png"), "keep").unwrap();
        overlay(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(to.join("Clubs/Normal/1.png")).unwrap(), "new");
        assert_eq!(fs::read_to_string(to.join("Clubs/Normal/2.png")).unwrap(), "keep");
    }

    #[test]
    fn strips_only_a_lone_wrapper_folder() {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("FMG Standard Logos 2026.01/Clubs")).unwrap();
        assert!(content_root(t.path()).unwrap().ends_with("FMG Standard Logos 2026.01"));
        fs::create_dir_all(t.path().join("iconfaces")).unwrap();
        assert_eq!(content_root(t.path()).unwrap(), t.path());
    }
}
