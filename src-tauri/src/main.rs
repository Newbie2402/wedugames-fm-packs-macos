#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use fmup_core::detect::{detect, game_is_running, GameInfo};
use fmup_core::download::Client;
use fmup_core::engine::{self, PackagePlan, Progress};
use fmup_core::license::{self, Ticket};
use fmup_core::state::Store;
use fmup_core::{API_URL, PUBLIC_KEY_HEX};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::{Emitter, Manager, State};

/// The pack Worker takes a ticket for 24 hours, so an open app renews it well before then.
const RENEW_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

struct App {
    store: Store,
    cancel: Arc<AtomicBool>,
    busy: AtomicBool,
    plans: Mutex<Vec<PackagePlan>>,
    /// The license ticket as the pack Worker wants it; empty until the server says this computer is licensed.
    token: Arc<RwLock<String>>,
}

/// Asks the license server for a fresh ticket and hands it to every download. A no clears it.
async fn renew(store: &Store, token: &RwLock<String>) -> Result<Ticket, Option<String>> {
    let result = license::load(store).await;
    match &result {
        Ok(ticket) => *token.write().unwrap() = ticket.token(),
        // Unreachable: keep the ticket we have, the Worker still takes it for a while.
        Err(reason) if reason.as_deref() != Some(license::UNREACHABLE) => token.write().unwrap().clear(),
        Err(_) => {}
    }
    result
}

/// Releases the single-operation lock on every exit path.
struct BusyGuard<'a>(&'a AtomicBool);

impl<'a> BusyGuard<'a> {
    fn take(flag: &'a AtomicBool) -> Result<Self, String> {
        if flag.swap(true, Ordering::SeqCst) {
            return Err("Masih ada proses yang berjalan".into());
        }
        Ok(Self(flag))
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn ensure_game_closed() -> Result<(), String> {
    if game_is_running() {
        return Err("Tutup Football Manager dulu".into());
    }
    Ok(())
}

fn text(e: anyhow::Error) -> String {
    format!("{e:#}")
}

fn find_game(store: &Store) -> Result<GameInfo, String> {
    let meta = store.load_meta().map_err(text)?;
    detect(meta.game_dir.as_deref(), meta.user_dir.as_deref()).map_err(text)
}

/// The pack Worker serves nothing without the license ticket.
fn client(app: &App) -> Result<Client, String> {
    if app.token.read().unwrap().is_empty() {
        return Err("Aktifkan license dulu".into());
    }
    Client::shared(API_URL, app.token.clone()).map_err(text)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LicenseStatus {
    licensed: bool,
    key: Option<String>,
    name: Option<String>,
    expire: Option<String>,
    /// Why the register window shows, when there is a reason to give.
    reason: Option<String>,
}

impl LicenseStatus {
    fn of(ticket: &Ticket) -> Self {
        Self {
            licensed: true,
            key: Some(ticket.key.clone()),
            name: ticket.name.clone(),
            expire: Some(ticket.expire.clone()).filter(|e| !e.is_empty()),
            reason: None,
        }
    }
}

#[cfg(not(feature = "demo"))]
#[tauri::command]
async fn license_status(app: State<'_, App>) -> Result<LicenseStatus, String> {
    // Opening the app always asks the server: there is no offline grace.
    app.token.write().unwrap().clear();
    Ok(match renew(&app.store, &app.token).await {
        Ok(ticket) => LicenseStatus::of(&ticket),
        Err(reason) => LicenseStatus { licensed: false, key: None, name: None, expire: None, reason },
    })
}

#[cfg(feature = "demo")]
#[tauri::command]
async fn license_status() -> Result<LicenseStatus, String> {
    Ok(LicenseStatus { licensed: true, key: Some("FMP-DEMO".into()), name: None, expire: None, reason: None })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Activation {
    #[serde(flatten)]
    reply: license::Reply,
    status: Option<LicenseStatus>,
}

/// `transfer` moves the license to this computer when its device quota is full.
#[tauri::command]
async fn license_activate(app: State<'_, App>, identifier: String, transfer: bool) -> Result<Activation, String> {
    let (reply, ticket) = license::activate(&app.store, &identifier, transfer).await;
    let status = ticket.as_ref().map(LicenseStatus::of);
    if let Some(ticket) = ticket {
        *app.token.write().unwrap() = ticket.token();
    }
    Ok(Activation { reply, status })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Overview {
    game: Result<GameInfo, String>,
    temp_dir: Option<PathBuf>,
}

#[tauri::command]
fn overview(app: State<'_, App>) -> Result<Overview, String> {
    let temp_dir = app.store.load_meta().map_err(text)?.temp_dir;
    Ok(Overview { game: find_game(&app.store), temp_dir })
}

#[tauri::command]
fn set_temp_dir(app: State<'_, App>, path: String) -> Result<(), String> {
    let mut meta = app.store.load_meta().map_err(text)?;
    meta.temp_dir = Some(PathBuf::from(path));
    app.store.save_meta(&meta).map_err(text)
}

#[tauri::command]
fn set_game_dir(app: State<'_, App>, path: String) -> Result<(), String> {
    let path = PathBuf::from(path.trim());
    let mut meta = app.store.load_meta().map_err(text)?;
    detect(Some(&path), meta.user_dir.as_deref()).map_err(text)?;
    meta.game_dir = Some(path);
    app.store.save_meta(&meta).map_err(text)
}

/// Lets the customer point at their own Football Manager folder in Documents when
/// auto-detection picks the wrong one (unusual OneDrive setups). An empty path
/// clears it and goes back to auto-detection.
#[tauri::command]
fn set_user_dir(app: State<'_, App>, path: String) -> Result<(), String> {
    let path = path.trim();
    let mut meta = app.store.load_meta().map_err(text)?;
    meta.user_dir = if path.is_empty() {
        None
    } else {
        let path = PathBuf::from(path);
        if !path.is_dir() {
            return Err("Folder itu tidak ada".into());
        }
        Some(path)
    };
    app.store.save_meta(&meta).map_err(text)
}

#[cfg(not(feature = "demo"))]
#[tauri::command]
async fn check(app: State<'_, App>, force_verify: bool) -> Result<Vec<PackagePlan>, String> {
    use fmup_core::engine::Status;
    use fmup_core::manifest::Target;

    let game = find_game(&app.store)?;
    let manifest = client(&app)?.manifest(PUBLIC_KEY_HEX).await.map_err(text)?;
    let mut meta = app.store.load_meta().map_err(text)?;
    let store = app.store.clone();
    let game_clone = game.clone();
    let plans = tauri::async_runtime::spawn_blocking(move || {
        engine::plan(&manifest, &game_clone, &store, force_verify)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(text)?;

    if game.build_id.is_some()
        && plans
            .iter()
            .filter(|p| p.target == Target::Database)
            .all(|p| p.status == Status::NotInstalled || p.status == Status::UpToDate)
    {
        meta.last_build_id = game.build_id.clone();
        app.store.save_meta(&meta).map_err(text)?;
    }

    *app.plans.lock().unwrap() = plans.clone();
    Ok(plans)
}

#[cfg(not(feature = "demo"))]
#[tauri::command]
async fn run(handle: tauri::AppHandle, app: State<'_, App>, ids: Vec<String>) -> Result<(), String> {
    let _busy = BusyGuard::take(&app.busy)?;
    ensure_game_closed()?;
    app.cancel.store(false, Ordering::SeqCst);
    let game = find_game(&app.store)?;
    // A fresh ticket for the download; a big pack can take longer than one ticket lasts, so the loop
    // started in main keeps renewing it.
    if let Err(Some(reason)) = renew(&app.store, &app.token).await {
        if reason != license::UNREACHABLE {
            return Err(reason);
        }
    }
    let client = client(&app)?;
    let plans = app.plans.lock().unwrap().clone();
    let progress = Arc::new(move |p: Progress| {
        let _ = handle.emit("progress", p);
    });
    engine::execute(&client, &plans, &ids, &game, &app.store, progress, app.cancel.clone())
        .await
        .map_err(text)
}

#[tauri::command]
fn cancel(app: State<'_, App>) {
    app.cancel.store(true, Ordering::SeqCst);
}

#[cfg(not(feature = "demo"))]
#[tauri::command]
async fn uninstall(app: State<'_, App>, id: String) -> Result<(), String> {
    let _busy = BusyGuard::take(&app.busy)?;
    ensure_game_closed()?;
    let store = app.store.clone();
    tauri::async_runtime::spawn_blocking(move || engine::uninstall(&id, &store))
        .await
        .map_err(|e| e.to_string())?
        .map_err(text)
}

#[cfg(not(feature = "demo"))]
#[tauri::command]
async fn repair(app: State<'_, App>, id: String) -> Result<PackagePlan, String> {
    let game = find_game(&app.store)?;
    let manifest = client(&app)?.manifest(PUBLIC_KEY_HEX).await.map_err(text)?;
    let pkg = manifest
        .packages
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("Pack '{id}' tidak ditemukan di manifest"))?;
    let repair_plan = engine::plan_repair(pkg, &game, &app.store).map_err(text)?;

    let mut plans = app.plans.lock().unwrap();
    if let Some(existing) = plans.iter_mut().find(|p| p.id == id) {
        *existing = repair_plan.clone();
    } else {
        plans.push(repair_plan.clone());
    }
    Ok(repair_plan)
}

/// Cover of a pack as a data URL. None means the app draws its built-in art.
#[tauri::command]
async fn cover_image(app: State<'_, App>, id: String) -> Result<Option<String>, String> {
    use base64::Engine;
    let cover = app.plans.lock().unwrap().iter().find(|p| p.id == id).and_then(|p| p.cover.clone());
    let Some(cover) = cover else {
        return Ok(None);
    };
    let path = client(&app)?.fetch_cover(&cover, &app.store.cover_dir()).await.map_err(text)?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(Some(format!("data:{mime};base64,{encoded}")))
}

#[cfg(feature = "demo")]
fn sample_plans() -> Vec<PackagePlan> {
    use fmup_core::engine::Status::*;
    use fmup_core::manifest::Target::{Database, Graphics};
    let pack = |id: &str, name: &str, group: Option<&str>, status, installed: Option<&str>, download, total, files, target| {
        PackagePlan {
            id: id.into(),
            name: name.into(),
            target,
            group: group.map(Into::into),
            cover: None,
            total_bytes: total,
            total_files: files,
            latest_version: "2026.11".into(),
            installed_version: installed.map(Into::into),
            status,
            download_bytes: download,
            unpacked_bytes: download,
            actions: Vec::new(),
        }
    };
    let (faces, logos, kits) = (Some("faces"), Some("logos"), Some("kits-3d"));
    vec![
        pack("cutout", "Cut-Out Player Faces", faces, UpdateAvailable, Some("2026.10"), 1_200_000_000, 9_800_000_000, 412_000, Graphics),
        pack("df11", "DF11 Player Faces", faces, NotInstalled, None, 8_400_000_000, 8_400_000_000, 388_000, Graphics),
        pack("fmsite", "FMSite Facepack", faces, NotInstalled, None, 6_100_000_000, 6_100_000_000, 205_000, Graphics),
        pack("fmg", "FMG Logos", logos, NotInstalled, None, 3_200_000_000, 3_200_000_000, 93_000, Graphics),
        pack("steel", "Steel Logos", logos, UpToDate, Some("2026.11"), 0, 2_900_000_000, 93_000, Graphics),
        pack("metallic", "Metallic Logos", logos, NotInstalled, None, 2_700_000_000, 2_700_000_000, 93_000, Graphics),
        pack("kits", "Kits Megapack", kits, NotInstalled, None, 2_100_000_000, 2_100_000_000, 41_000, Graphics),
        pack("rnf", "Real Name Fix", None, NeedsRepair, Some("2026.11"), 4_000_000, 4_000_000, 12, Database),
        pack("trophies", "Trophies Pack", None, UpToDate, Some("2026.11"), 0, 310_000_000, 3_000, Graphics),
    ]
}

#[cfg(feature = "demo")]
#[tauri::command]
async fn check(app: State<'_, App>, force_verify: bool) -> Result<Vec<PackagePlan>, String> {
    let _ = force_verify;
    let mut plans = app.plans.lock().unwrap();
    if plans.is_empty() {
        *plans = sample_plans();
    }
    Ok(plans.clone())
}

/// Plays the progress bar and changes in-memory state only.
#[cfg(feature = "demo")]
#[tauri::command]
async fn run(handle: tauri::AppHandle, app: State<'_, App>, ids: Vec<String>) -> Result<(), String> {
    use fmup_core::engine::Status;
    use std::time::Duration;
    let _busy = BusyGuard::take(&app.busy)?;
    app.cancel.store(false, Ordering::SeqCst);
    let cancel = app.cancel.clone();
    let total: u64 = app.plans.lock().unwrap().iter().filter(|p| ids.contains(&p.id)).map(|p| p.download_bytes).sum();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        for step in 0..=60u64 {
            if cancel.load(Ordering::SeqCst) {
                return Err("Dibatalkan".into());
            }
            let progress = if step <= 40 {
                Progress::Downloading { done: total * step / 40, total }
            } else {
                Progress::Installing { done: 2_500 * (step - 40), total: 50_000 }
            };
            let _ = handle.emit("progress", progress);
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())??;

    let mut plans = app.plans.lock().unwrap();
    let groups: Vec<String> = plans.iter().filter(|p| ids.contains(&p.id)).filter_map(|p| p.group.clone()).collect();
    for plan in plans.iter_mut() {
        if ids.contains(&plan.id) {
            plan.status = Status::UpToDate;
            plan.installed_version = Some(plan.latest_version.clone());
            plan.download_bytes = 0;
        } else if plan.group.as_ref().is_some_and(|g| groups.contains(g)) {
            plan.status = Status::NotInstalled;
            plan.installed_version = None;
            plan.download_bytes = plan.total_bytes;
        }
    }
    Ok(())
}

#[cfg(feature = "demo")]
#[tauri::command]
async fn uninstall(app: State<'_, App>, id: String) -> Result<(), String> {
    let mut plans = app.plans.lock().unwrap();
    if let Some(plan) = plans.iter_mut().find(|p| p.id == id) {
        plan.status = fmup_core::engine::Status::NotInstalled;
        plan.installed_version = None;
        plan.download_bytes = plan.total_bytes;
    }
    Ok(())
}

#[cfg(feature = "demo")]
#[tauri::command]
async fn repair(app: State<'_, App>, id: String) -> Result<PackagePlan, String> {
    let mut plans = app.plans.lock().unwrap();
    let plan = plans
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("Pack '{id}' tidak ditemukan"))?;
    plan.status = fmup_core::engine::Status::NeedsRepair;
    plan.download_bytes = plan.total_bytes;
    Ok(plan.clone())
}

fn main() {
    let store = Store::default_location().expect("folder data tidak tersedia");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(App {
            store,
            cancel: Arc::new(AtomicBool::new(false)),
            busy: AtomicBool::new(false),
            plans: Mutex::new(Vec::new()),
            token: Arc::new(RwLock::new(String::new())),
        })
        .setup(|tauri_app| {
            let app = tauri_app.state::<App>();
            let (store, token) = (app.store.clone(), app.token.clone());
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(RENEW_EVERY).await;
                    if !token.read().unwrap().is_empty() {
                        let _ = renew(&store, &token).await;
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            license_status,
            license_activate,
            overview,
            set_game_dir,
            set_user_dir,
            set_temp_dir,
            check,
            repair,
            run,
            cancel,
            uninstall,
            cover_image
        ])
        .run(tauri::generate_context!())
        .expect("gagal menjalankan aplikasi");
}
