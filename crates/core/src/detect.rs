use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

pub const APP_ID: u32 = 4530370;
const GAME_FOLDER: &str = "Football Manager 27";
const DB_SUBDIR: &str = "shared/data/database/db";

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GameInfo {
    pub game_dir: PathBuf,
    pub build_id: Option<String>,
    pub graphics_dir: PathBuf,
    pub db_dir: PathBuf,
    pub db_versions: Vec<u32>,
    /// Packs here sync to the customer's cloud storage unless they exclude them.
    pub in_onedrive: bool,
}

impl GameInfo {
    pub fn from_dirs(game_dir: &Path, user_dir: &Path, build_id: Option<String>) -> Result<Self> {
        let db_dir = resolve_db_dir(game_dir)
            .ok_or_else(|| anyhow!("Folder database FM tidak ditemukan di {}", game_dir.display()))?;
        let mut db_versions: Vec<u32> = std::fs::read_dir(&db_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .collect();
        db_versions.sort_unstable();
        Ok(Self {
            game_dir: game_dir.to_path_buf(),
            build_id,
            graphics_dir: user_dir.join("graphics"),
            db_dir,
            db_versions,
            in_onedrive: user_dir.to_string_lossy().to_lowercase().contains("onedrive"),
        })
    }

    pub fn db_root(&self, version: u32) -> PathBuf {
        self.db_dir.join(version.to_string())
    }
}

/// Where `shared/data/database/db` lives: next to the game on Windows, and on
/// macOS inside the game's `.app` bundle. FM 27 on macOS keeps it in a nested
/// plugin bundle (`fm.app/Contents/PlugIns/game_plugin.bundle/Contents/Resources/...`),
/// so the app's own Resources and any `PlugIns/*.bundle` Resources are checked.
fn resolve_db_dir(game_dir: &Path) -> Option<PathBuf> {
    let direct = game_dir.join(DB_SUBDIR);
    if direct.is_dir() {
        return Some(direct);
    }
    let entries = std::fs::read_dir(game_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.extension().and_then(|e| e.to_str()) == Some("app") {
            if let Some(found) = db_in_bundle(&path) {
                return Some(found);
            }
        }
    }
    None
}

/// `shared/data/database/db` inside a macOS `.app` bundle: directly under
/// `Contents/Resources`, or inside a nested `PlugIns/*.bundle` (FM 27's layout,
/// where the database sits in `game_plugin.bundle`).
fn db_in_bundle(bundle: &Path) -> Option<PathBuf> {
    for cand in [
        bundle.join("Contents/Resources").join(DB_SUBDIR),
        bundle.join("Contents").join(DB_SUBDIR),
        bundle.join(DB_SUBDIR),
    ] {
        if cand.is_dir() {
            return Some(cand);
        }
    }
    for entry in std::fs::read_dir(bundle.join("Contents/PlugIns")).ok()?.flatten() {
        let nested = entry.path();
        if nested.is_dir() && nested.extension().and_then(|e| e.to_str()) == Some("bundle") {
            let cand = nested.join("Contents/Resources").join(DB_SUBDIR);
            if cand.is_dir() {
                return Some(cand);
            }
        }
    }
    None
}

/// Value of `"key" "value"` on a VDF/ACF line.
fn vdf_value(line: &str, key: &str) -> Option<String> {
    let parts: Vec<&str> = line.trim().split('"').collect();
    (parts.len() >= 4 && parts[1].eq_ignore_ascii_case(key)).then(|| parts[3].replace("\\\\", "\\"))
}

pub fn parse_library_paths(vdf: &str) -> Vec<PathBuf> {
    vdf.lines().filter_map(|l| vdf_value(l, "path")).map(PathBuf::from).collect()
}

pub fn parse_app_manifest(acf: &str) -> (Option<String>, Option<String>) {
    let find = |key| acf.lines().find_map(|l| vdf_value(l, key));
    (find("installdir"), find("buildid"))
}

#[cfg(windows)]
fn steam_root() -> Option<PathBuf> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey("Software\\Valve\\Steam")
        .ok()?;
    let path: String = key.get_value("SteamPath").ok()?;
    Some(PathBuf::from(path))
}

#[cfg(target_os = "macos")]
fn steam_root() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join("Library/Application Support/Steam"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn steam_root() -> Option<PathBuf> {
    None
}

fn find_in_steam() -> Option<(PathBuf, Option<String>)> {
    let root = steam_root()?;
    let vdf = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")).unwrap_or_default();
    let mut libraries = parse_library_paths(&vdf);
    libraries.push(root);
    for lib in libraries {
        let apps = lib.join("steamapps");
        let Ok(acf) = std::fs::read_to_string(apps.join(format!("appmanifest_{APP_ID}.acf"))) else {
            continue;
        };
        let (install_dir, build_id) = parse_app_manifest(&acf);
        let dir = apps.join("common").join(install_dir.unwrap_or_else(|| GAME_FOLDER.into()));
        if dir.is_dir() {
            return Some((dir, build_id));
        }
    }
    None
}

/// Finds FM 27, or validates the folders the user picked by hand. `user_dir_override`
/// is the customer's own Football Manager folder in Documents, for odd OneDrive setups.
pub fn detect(game_dir_override: Option<&Path>, user_dir_override: Option<&Path>) -> Result<GameInfo> {
    let (game_dir, build_id) = match game_dir_override {
        Some(dir) => (dir.to_path_buf(), None),
        None => find_in_steam().ok_or_else(|| anyhow!("FM 27 tidak ditemukan di library Steam"))?,
    };
    let user_dir = match user_dir_override {
        Some(dir) => normalize_user_dir(dir),
        None => pick_user_dir(&user_dir_candidates())
            .ok_or_else(|| anyhow!("Folder user FM tidak ditemukan"))?,
    };
    GameInfo::from_dirs(&game_dir, &user_dir, build_id)
}

/// Turns whatever folder the customer picked into the FM user folder: the Documents
/// parent, the "Sports Interactive" folder, the FM folder itself, or its "graphics"
/// subfolder all lead to the same place.
fn normalize_user_dir(path: &Path) -> PathBuf {
    let inside = path.join("Sports Interactive").join(GAME_FOLDER);
    if inside.is_dir() {
        return inside;
    }
    match path.file_name().and_then(|n| n.to_str()) {
        Some("graphics") => path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.to_path_buf()),
        Some("Sports Interactive") => path.join(GAME_FOLDER),
        _ => path.to_path_buf(),
    }
}

/// Every place Documents can be on this PC, the system's own answer first.
/// document_dir() follows OneDrive redirection, but a game started before the
/// redirection can keep using the local folder, so the others count too. OneDrive
/// roots are also found by name under the profile, since the folder can be called
/// "OneDrive - <Company>" with no matching environment variable set.
#[cfg(not(target_os = "macos"))]
fn documents_candidates() -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = dirs::document_dir().into_iter().collect();
    if let Some(home) = dirs::home_dir() {
        folders.push(home.join("Documents"));
        if let Ok(entries) = std::fs::read_dir(&home) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().to_lowercase().starts_with("onedrive")
                    && entry.path().is_dir()
                {
                    folders.push(entry.path().join("Documents"));
                    folders.push(entry.path().join("Dokumen"));
                }
            }
        }
    }
    for variable in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(root) = std::env::var_os(variable) {
            folders.push(PathBuf::from(&root).join("Documents"));
            folders.push(PathBuf::from(&root).join("Dokumen"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    folders.retain(|p| seen.insert(p.clone()));
    folders
}

/// macOS keeps FM's user data (saves, graphics) in Application Support, not
/// Documents. These are the base folders only; pick_user_dir() appends
/// "Sports Interactive/<game>" to each, exactly as it does on Windows.
#[cfg(target_os = "macos")]
fn user_dir_candidates() -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(base) = dirs::data_dir() {
        folders.push(base);
    }
    if let Some(home) = dirs::home_dir() {
        // In case the game or an old install used Documents anyway.
        folders.push(home.join("Documents"));
    }
    folders
}

#[cfg(not(target_os = "macos"))]
fn user_dir_candidates() -> Vec<PathBuf> {
    documents_candidates()
}

/// The folder FM really uses is the one it has already written saves into.
fn pick_user_dir(documents: &[PathBuf]) -> Option<PathBuf> {
    let user_dirs: Vec<PathBuf> =
        documents.iter().map(|d| d.join("Sports Interactive").join(GAME_FOLDER)).collect();
    user_dirs
        .iter()
        .find(|dir| dir.join("games").is_dir())
        .or_else(|| user_dirs.iter().find(|dir| dir.is_dir()))
        .or_else(|| user_dirs.first())
        .cloned()
}

/// FM locks its files while running, so nothing may be installed during that.
pub fn game_is_running() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq fm.exe", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| {
                // The no-match message is localized, so match the image name itself.
                let out = String::from_utf8_lossy(&o.stdout).to_lowercase();
                out.lines().any(|l| l.split_whitespace().next() == Some("fm.exe"))
            })
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        // No tasklist on macOS: pgrep by process name. Candidates cover the
        // "fm" binary and the game's display name; -x + -i keeps it exact.
        ["fm", "Football Manager 27", "football"].iter().any(|name| {
            std::process::Command::new("pgrep")
                .args(["-i", "-x", name])
                .output()
                .is_ok_and(|o| o.status.success())
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_library_paths() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n}";
        assert_eq!(parse_library_paths(vdf), vec![PathBuf::from("C:\\Program Files (x86)\\Steam")]);
    }

    #[test]
    fn parses_app_manifest() {
        let acf = "\t\"installdir\"\t\t\"Football Manager 27\"\n\t\"buildid\"\t\t\"26227640\"\n";
        let (dir, build) = parse_app_manifest(acf);
        assert_eq!(dir.as_deref(), Some("Football Manager 27"));
        assert_eq!(build.as_deref(), Some("26227640"));
    }

    fn user_dir_in(documents: &Path) -> PathBuf {
        documents.join("Sports Interactive").join(GAME_FOLDER)
    }

    #[test]
    fn prefers_the_folder_fm_saves_into() {
        let t = tempfile::tempdir().unwrap();
        let onedrive = t.path().join("OneDrive/Documents");
        let local = t.path().join("Documents");
        std::fs::create_dir_all(user_dir_in(&onedrive)).unwrap();
        std::fs::create_dir_all(user_dir_in(&local).join("games")).unwrap();
        let picked = pick_user_dir(&[onedrive, local.clone()]).unwrap();
        assert_eq!(picked, user_dir_in(&local));
    }

    #[test]
    fn falls_back_to_the_system_documents_folder() {
        let t = tempfile::tempdir().unwrap();
        let first = t.path().join("OneDrive/Documents");
        let picked = pick_user_dir(&[first.clone(), t.path().join("Documents")]).unwrap();
        assert_eq!(picked, user_dir_in(&first));
    }

    #[test]
    fn flags_a_user_folder_inside_onedrive() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join(DB_SUBDIR)).unwrap();
        let user = t.path().join("OneDrive/Documents/Sports Interactive/Football Manager 27");
        assert!(GameInfo::from_dirs(t.path(), &user, None).unwrap().in_onedrive);
        assert!(!GameInfo::from_dirs(t.path(), t.path(), None).unwrap().in_onedrive);
    }

    #[test]
    fn normalizes_any_folder_the_user_picks() {
        let t = tempfile::tempdir().unwrap();
        let fm = user_dir_in(&t.path().join("Documents"));
        std::fs::create_dir_all(fm.join("graphics")).unwrap();
        // The FM folder itself, its graphics subfolder, and the Documents parent
        // all resolve to the FM folder.
        assert_eq!(normalize_user_dir(&fm), fm);
        assert_eq!(normalize_user_dir(&fm.join("graphics")), fm);
        assert_eq!(normalize_user_dir(&t.path().join("Documents")), fm);
        assert_eq!(normalize_user_dir(&t.path().join("Documents/Sports Interactive")), fm);
    }

    #[test]
    fn lists_db_versions_sorted() {
        let t = tempfile::tempdir().unwrap();
        for v in ["2700", "2620", "notes"] {
            std::fs::create_dir_all(t.path().join(DB_SUBDIR).join(v)).unwrap();
        }
        let g = GameInfo::from_dirs(t.path(), t.path(), None).unwrap();
        assert_eq!(g.db_versions, vec![2620, 2700]);
    }

    #[test]
    fn finds_db_in_nested_plugin_bundle() {
        // FM 27 on macOS: db lives in fm.app/Contents/PlugIns/game_plugin.bundle/
        // Contents/Resources/shared/data/database/db, not the app's own Resources.
        let t = tempfile::tempdir().unwrap();
        let db = t
            .path()
            .join("fm.app/Contents/PlugIns/game_plugin.bundle/Contents/Resources")
            .join(DB_SUBDIR)
            .join("2620");
        std::fs::create_dir_all(&db).unwrap();
        let g = GameInfo::from_dirs(t.path(), t.path(), None).unwrap();
        assert_eq!(g.db_versions, vec![2620]);
        assert!(g.db_dir.ends_with(DB_SUBDIR));
    }
}
