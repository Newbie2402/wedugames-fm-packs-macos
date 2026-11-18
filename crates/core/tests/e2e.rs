//! Full client flow against a local HTTP server and a throwaway game folder.

use ed25519_dalek::{Signer, SigningKey};
use fmup_core::detect::GameInfo;
use fmup_core::download::Client;
use fmup_core::engine::{execute, plan, plan_repair, uninstall, PackagePlan, Status};
use fmup_core::manifest::{Chunk, Cover, Manifest, Package, SignedManifest, Target};
use fmup_core::state::Store;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Response, Server, StatusCode};
use zip::write::SimpleFileOptions;

const LICENSE: &str = "test-license";

#[derive(Default)]
struct Remote {
    objects: Mutex<HashMap<String, Vec<u8>>>,
    manifest: Mutex<String>,
    object_requests: AtomicUsize,
    range_requests: AtomicUsize,
    /// Next object response is cut to this many bytes.
    truncate_next: Mutex<Option<usize>>,
}

fn serve(server: Server, remote: Arc<Remote>) {
    for request in server.incoming_requests() {
        let authorized = request
            .headers()
            .iter()
            .any(|h| h.field.equiv("Authorization") && h.value.as_str() == format!("Bearer {LICENSE}"));
        if !authorized {
            let _ = request.respond(Response::empty(StatusCode(401)));
            continue;
        }
        let url = request.url().to_string();
        if url == "/manifest" {
            let body = remote.manifest.lock().unwrap().clone();
            let _ = request.respond(Response::from_string(body));
            continue;
        }
        let key = url.trim_start_matches("/objects/").to_string();
        let Some(body) = remote.objects.lock().unwrap().get(&key).cloned() else {
            let _ = request.respond(Response::empty(StatusCode(404)));
            continue;
        };
        remote.object_requests.fetch_add(1, Ordering::SeqCst);
        let range = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Range"))
            .and_then(|h| h.value.as_str().strip_prefix("bytes=")?.strip_suffix('-')?.parse::<usize>().ok());
        if let Some(from) = range {
            remote.range_requests.fetch_add(1, Ordering::SeqCst);
            let slice = body[from..].to_vec();
            let header = format!("Content-Range: bytes {from}-{}/{}", body.len() - 1, body.len());
            let response = Response::from_data(slice)
                .with_status_code(206)
                .with_header(header.parse::<Header>().unwrap());
            let _ = request.respond(response);
            continue;
        }
        let mut data = body;
        if let Some(cut) = remote.truncate_next.lock().unwrap().take() {
            // A short body under a correct Content-Length, as a dropped
            // connection looks to the client.
            data.truncate(cut);
        }
        let _ = request.respond(Response::from_data(data));
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    base: String,
    remote: Arc<Remote>,
    public_key: String,
    signing: SigningKey,
    game: GameInfo,
    store: Store,
    game_dir: std::path::PathBuf,
    user_dir: std::path::PathBuf,
}

fn zip_bytes(files: &[(&str, &str)]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut out);
    for (name, body) in files {
        w.start_file(*name, SimpleFileOptions::default()).unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap();
    out.into_inner()
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let game_dir = tmp.path().join("game");
        let user_dir = tmp.path().join("user");
        let db = game_dir.join("shared/data/database/db/2700/lnc");
        fs::create_dir_all(&db).unwrap();
        fs::write(db.join("names.lnc"), "original").unwrap();

        let server = Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr());
        let remote = Arc::new(Remote::default());
        std::thread::spawn({
            let remote = remote.clone();
            move || serve(server, remote)
        });

        let signing = SigningKey::from_bytes(&[9u8; 32]);
        Self {
            public_key: hex::encode(signing.verifying_key().to_bytes()),
            signing,
            base,
            remote,
            game: GameInfo::from_dirs(&game_dir, &user_dir, Some("1".into())).unwrap(),
            store: Store { root: tmp.path().join("store") },
            game_dir,
            user_dir,
            _tmp: tmp,
        }
    }

    fn rescan(&mut self) {
        self.game = GameInfo::from_dirs(&self.game_dir, &self.user_dir, Some("2".into())).unwrap();
    }

    fn chunk(&self, id: &str, files: &[(&str, &str)]) -> Chunk {
        let bytes = zip_bytes(files);
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let key = format!("packs/{id}/{sha256}.zip");
        let chunk = Chunk {
            key: key.clone(),
            size: bytes.len() as u64,
            sha256,
            files: files.len() as u64,
            unpacked: files.iter().map(|f| f.1.len() as u64).sum(),
            patch: false,
        };
        self.remote.objects.lock().unwrap().insert(key, bytes);
        chunk
    }

    fn publish(&self, packages: Vec<Package>) {
        let payload = serde_json::to_string(&Manifest { schema: 1, packages }).unwrap();
        let signature = hex::encode(self.signing.sign(payload.as_bytes()).to_bytes());
        *self.remote.manifest.lock().unwrap() =
            serde_json::to_string(&SignedManifest { payload, signature }).unwrap();
    }

    fn client(&self) -> Client {
        Client::new(&self.base, LICENSE).unwrap()
    }

    async fn plans(&self, verify: bool) -> Vec<PackagePlan> {
        let manifest = self.client().manifest(&self.public_key).await.unwrap();
        plan(&manifest, &self.game, &self.store, verify).unwrap()
    }

    async fn install(&self, plans: &[PackagePlan]) -> anyhow::Result<()> {
        let ids: Vec<String> = plans.iter().map(|p| p.id.clone()).collect();
        execute(
            &self.client(),
            plans,
            &ids,
            &self.game,
            &self.store,
            Arc::new(|_| {}),
            Arc::new(AtomicBool::new(false)),
        )
        .await
    }

    fn db_file(&self, version: u32, rel: &str) -> std::path::PathBuf {
        self.game.db_root(version).join(rel)
    }
}

fn package(id: &str, version: &str, target: Target, chunks: Vec<Chunk>) -> Package {
    Package {
        id: id.into(),
        name: id.into(),
        version: version.into(),
        target,
        min_db: (target == Target::Database).then_some(2700),
        group: None,
        cover: None,
        buckets: chunks.len() as u64,
        chunks,
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn install_update_patch_and_uninstall() {
    let mut f = Fixture::new();
    let faces_a = f.chunk("faces", &[("faces/1.png", "a1")]);
    let faces_b = f.chunk("faces", &[("faces/2.png", "b1"), ("faces/old.png", "gone soon")]);
    let names = f.chunk("rnf", &[("lnc/names.lnc", "fixed"), ("lnc/extra.lnc", "extra")]);
    f.publish(vec![
        package("faces", "1", Target::Graphics, vec![faces_a.clone(), faces_b]),
        package("rnf", "1", Target::Database, vec![names.clone()]),
    ]);

    // First install: backs up the game's file and fills the graphics folder.
    let plans = f.plans(false).await;
    assert!(plans.iter().all(|p| p.status == Status::NotInstalled));
    f.install(&plans).await.unwrap();
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "a1");
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "fixed");
    assert!(f.plans(false).await.iter().all(|p| p.status == Status::UpToDate));
    assert_eq!(fs::read_dir(f.store.cache_dir()).unwrap().count(), 0);

    // Content update: only the changed chunk travels, and a deleted file goes.
    let faces_b2 = f.chunk("faces", &[("faces/2.png", "b2")]);
    f.publish(vec![
        package("faces", "2", Target::Graphics, vec![faces_a.clone(), faces_b2.clone()]),
        package("rnf", "1", Target::Database, vec![names.clone()]),
    ]);
    let plans = f.plans(false).await;
    assert_eq!(plans[0].status, Status::UpdateAvailable);
    assert_eq!(plans[0].download_bytes, faces_b2.size);
    assert_eq!(plans[1].status, Status::UpToDate);
    let before = f.remote.object_requests.load(Ordering::SeqCst);
    f.install(&plans).await.unwrap();
    assert_eq!(f.remote.object_requests.load(Ordering::SeqCst) - before, 1);
    assert_eq!(read(&f.game.graphics_dir.join("faces/2.png")), "b2");
    assert!(!f.game.graphics_dir.join("faces/old.png").exists());

    // A patch overwrites our file: the deep check notices and repairs it.
    fs::write(f.db_file(2700, "lnc/names.lnc"), "patched").unwrap();
    assert_eq!(f.plans(false).await[1].status, Status::UpToDate);
    let plans = f.plans(true).await;
    assert_eq!(plans[1].status, Status::NeedsRepair);
    f.install(&plans).await.unwrap();
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "fixed");

    // A patch adds a database folder: the fix follows without a new version.
    fs::create_dir_all(f.db_file(2710, "lnc")).unwrap();
    fs::write(f.db_file(2710, "lnc/names.lnc"), "new db").unwrap();
    f.rescan();
    let plans = f.plans(false).await;
    assert_eq!(plans[1].status, Status::NeedsRepair);
    f.install(&plans).await.unwrap();
    assert_eq!(read(&f.db_file(2710, "lnc/names.lnc")), "fixed");

    // Uninstall restores what the game had at the last repair.
    uninstall("rnf", &f.store).unwrap();
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "patched");
    assert_eq!(read(&f.db_file(2710, "lnc/names.lnc")), "new db");
    assert!(!f.db_file(2700, "lnc/extra.lnc").exists());
    assert_eq!(f.plans(false).await[1].status, Status::NotInstalled);
}

#[tokio::test]
async fn switching_pack_in_a_group_replaces_the_old_one() {
    let f = Fixture::new();
    let cutout = f.chunk("cutout", &[("faces/1.png", "cutout"), ("faces/only-cutout.png", "c")]);
    let df11 = f.chunk("df11", &[("faces/1.png", "df11")]);
    let trophies = f.chunk("trophies", &[("trophies/1.png", "t")]);
    let grouped = |id, chunk| Package {
        group: Some("faces".into()),
        ..package(id, "1", Target::Graphics, vec![chunk])
    };
    f.publish(vec![
        grouped("cutout", cutout),
        grouped("df11", df11),
        package("trophies", "1", Target::Graphics, vec![trophies]),
    ]);

    let plans = f.plans(false).await;
    let run = |ids: &[&str], plans: &[PackagePlan]| {
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        let plans = plans.to_vec();
        let (client, game, store) = (f.client(), f.game.clone(), f.store.clone());
        async move {
            let cancel = Arc::new(AtomicBool::new(false));
            execute(&client, &plans, &ids, &game, &store, Arc::new(|_| {}), cancel).await
        }
    };

    assert!(run(&["cutout", "df11"], &plans).await.is_err());

    run(&["cutout", "trophies"], &plans).await.unwrap();
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "cutout");

    let plans = f.plans(false).await;
    run(&["df11"], &plans).await.unwrap();
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "df11");
    assert!(!f.game.graphics_dir.join("faces/only-cutout.png").exists());
    assert_eq!(read(&f.game.graphics_dir.join("trophies/1.png")), "t");

    let plans = f.plans(false).await;
    assert_eq!(plans[0].status, Status::NotInstalled);
    assert_eq!(plans[1].status, Status::UpToDate);
    assert_eq!(plans[2].status, Status::UpToDate);
}

#[tokio::test]
async fn failed_download_keeps_current_pack() {
    let f = Fixture::new();
    let cutout = f.chunk("cutout", &[("faces/1.png", "cutout")]);
    let df11 = f.chunk("df11", &[("faces/1.png", "df11")]);
    let grouped = |id, chunk| Package {
        group: Some("faces".into()),
        ..package(id, "1", Target::Graphics, vec![chunk])
    };
    f.publish(vec![grouped("cutout", cutout), grouped("df11", df11.clone())]);

    let plans = f.plans(false).await;
    let first: Vec<PackagePlan> = plans.iter().filter(|p| p.id == "cutout").cloned().collect();
    f.install(&first).await.unwrap();

    f.remote.objects.lock().unwrap().remove(&df11.key);
    let plans = f.plans(false).await;
    let ids = vec!["df11".to_string()];
    let cancel = Arc::new(AtomicBool::new(false));
    let result = execute(&f.client(), &plans, &ids, &f.game, &f.store, Arc::new(|_| {}), cancel).await;
    assert!(result.is_err());
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "cutout");
    assert!(f.store.load_package("cutout").unwrap().is_some());
}

#[tokio::test]
async fn downloads_wait_in_the_folder_the_customer_picked() {
    let f = Fixture::new();
    let picked = f.game_dir.parent().unwrap().join("picked");
    let chunk = f.chunk("faces", &[("faces/1.png", "one")]);
    f.publish(vec![package("faces", "1", Target::Graphics, vec![chunk.clone()])]);

    let mut meta = f.store.load_meta().unwrap();
    meta.temp_dir = Some(picked.clone());
    f.store.save_meta(&meta).unwrap();
    assert!(f.store.cache_dir().starts_with(&picked));

    // A half download left there resumes from that folder.
    let temp = f.store.cache_dir();
    fs::create_dir_all(&temp).unwrap();
    let bytes = f.remote.objects.lock().unwrap()[&chunk.key].clone();
    fs::write(temp.join(format!("{}.part", chunk.sha256)), &bytes[..10]).unwrap();

    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();
    assert_eq!(f.remote.range_requests.load(Ordering::SeqCst), 1);
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "one");
    assert_eq!(fs::read_dir(&temp).unwrap().count(), 0);
}

fn as_patch(chunk: Chunk) -> Chunk {
    Chunk { patch: true, ..chunk }
}

#[tokio::test]
async fn patches_update_only_what_changed_and_stay_on_top() {
    let f = Fixture::new();
    let base_a = f.chunk("rnf", &[("lnc/names.lnc", "base"), ("lnc/keep.lnc", "keep")]);
    let base_b = f.chunk("rnf", &[("lnc/other.lnc", "other")]);
    let patch_1 = as_patch(f.chunk("rnf", &[("lnc/names.lnc", "patch one"), ("lnc/new.lnc", "new")]));
    let patch_2 = as_patch(f.chunk("rnf", &[("lnc/names.lnc", "patch two")]));

    f.publish(vec![package("rnf", "1", Target::Database, vec![base_a.clone(), base_b.clone()])]);
    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();

    // An existing customer pays for the patch alone.
    let all = vec![base_a.clone(), base_b.clone(), patch_1.clone(), patch_2.clone()];
    f.publish(vec![package("rnf", "2", Target::Database, all.clone())]);
    let plans = f.plans(false).await;
    assert_eq!(plans[0].status, Status::UpdateAvailable);
    assert_eq!(plans[0].download_bytes, patch_1.size + patch_2.size);
    f.install(&plans).await.unwrap();
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "patch two");

    // Files a patch replaced on purpose are not damage.
    assert_eq!(f.plans(true).await[0].status, Status::UpToDate);

    // Damage under a patch: the base comes back and the patches go on top again.
    fs::remove_file(f.db_file(2700, "lnc/keep.lnc")).unwrap();
    let plans = f.plans(true).await;
    assert_eq!(plans[0].status, Status::NeedsRepair);
    assert_eq!(plans[0].download_bytes, base_a.size + patch_1.size + patch_2.size);
    f.install(&plans).await.unwrap();
    assert_eq!(read(&f.db_file(2700, "lnc/keep.lnc")), "keep");
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "patch two");

    // Uninstall hands back the game's original, not an older version of ours.
    uninstall("rnf", &f.store).unwrap();
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "original");
    assert!(!f.db_file(2700, "lnc/new.lnc").exists());
    assert!(!f.db_file(2700, "lnc/keep.lnc").exists());
}

#[tokio::test]
async fn new_customer_gets_base_then_patches_in_order() {
    let f = Fixture::new();
    let base = f.chunk("rnf", &[("lnc/names.lnc", "base")]);
    let patch_1 = as_patch(f.chunk("rnf", &[("lnc/names.lnc", "one")]));
    let patch_2 = as_patch(f.chunk("rnf", &[("lnc/names.lnc", "two")]));
    f.publish(vec![package("rnf", "3", Target::Database, vec![base, patch_1, patch_2])]);

    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "two");
    assert_eq!(f.plans(true).await[0].status, Status::UpToDate);

    // The backup is the game's file, not a chunk from this same install.
    uninstall("rnf", &f.store).unwrap();
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "original");
}

#[tokio::test]
async fn caches_cover_and_rejects_a_tampered_one() {
    let f = Fixture::new();
    let image = b"fake image".to_vec();
    let sha256 = hex::encode(Sha256::digest(&image));
    let cover = Cover { key: format!("covers/faces/{sha256}.png"), size: image.len() as u64, sha256 };
    f.remote.objects.lock().unwrap().insert(cover.key.clone(), b"tampered".to_vec());
    let dir = f.store.cover_dir();
    assert!(f.client().fetch_cover(&cover, &dir).await.is_err());

    f.remote.objects.lock().unwrap().insert(cover.key.clone(), image.clone());
    let path = f.client().fetch_cover(&cover, &dir).await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), image);

    f.remote.objects.lock().unwrap().clear();
    assert_eq!(f.client().fetch_cover(&cover, &dir).await.unwrap(), path);
}

#[tokio::test]
async fn resumes_interrupted_download() {
    let f = Fixture::new();
    let body = "x".repeat(200_000);
    let chunk = f.chunk("faces", &[("faces/1.png", body.as_str())]);
    f.publish(vec![package("faces", "1", Target::Graphics, vec![chunk.clone()])]);

    fs::create_dir_all(f.store.cache_dir()).unwrap();
    let bytes = f.remote.objects.lock().unwrap()[&chunk.key].clone();
    let half = bytes.len() / 2;
    fs::write(f.store.cache_dir().join(format!("{}.part", chunk.sha256)), &bytes[..half]).unwrap();

    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();
    assert_eq!(f.remote.range_requests.load(Ordering::SeqCst), 1);
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), body);
}

#[tokio::test]
async fn retries_after_truncated_download() {
    let f = Fixture::new();
    let chunk = f.chunk("faces", &[("faces/1.png", "hello")]);
    f.publish(vec![package("faces", "1", Target::Graphics, vec![chunk.clone()])]);
    *f.remote.truncate_next.lock().unwrap() = Some(10);

    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "hello");
}

#[tokio::test]
async fn refuses_corrupted_chunk_without_touching_game() {
    let f = Fixture::new();
    let chunk = f.chunk("rnf", &[("lnc/names.lnc", "fixed")]);
    f.publish(vec![package("rnf", "1", Target::Database, vec![chunk.clone()])]);
    f.remote.objects.lock().unwrap().insert(chunk.key.clone(), b"not the real archive".to_vec());

    let plans = f.plans(false).await;
    assert!(f.install(&plans).await.is_err());
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "original");
}

#[tokio::test]
async fn rejects_bad_license_and_forged_manifest() {
    let f = Fixture::new();
    f.publish(vec![]);
    let bad = Client::new(&f.base, "wrong").unwrap();
    let error = bad.manifest(&f.public_key).await.unwrap_err().to_string();
    assert!(error.contains("License"), "{error}");

    let other = hex::encode(SigningKey::from_bytes(&[1u8; 32]).verifying_key().to_bytes());
    assert!(f.client().manifest(&other).await.is_err());
}

#[tokio::test]
async fn repair_when_graphics_folder_deleted() {
    let f = Fixture::new();
    let chunk = f.chunk("faces", &[("faces/1.png", "face1"), ("faces/2.png", "face2")]);
    f.publish(vec![package("faces", "1", Target::Graphics, vec![chunk.clone()])]);

    // Install faces
    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "face1");
    assert!(f.game.graphics_dir.exists());

    // User deletes the entire graphics directory manually
    fs::remove_dir_all(&f.game.graphics_dir).unwrap();
    assert!(!f.game.graphics_dir.exists());

    // Even without deep verification (verify_content = false), the missing root
    // is automatically detected and flagged as NeedsRepair!
    let plans = f.plans(false).await;
    assert_eq!(plans[0].status, Status::NeedsRepair);
    assert_eq!(plans[0].download_bytes, chunk.size);

    // Re-install / repair restores the entire directory and its files!
    f.install(&plans).await.unwrap();
    assert!(f.game.graphics_dir.exists());
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "face1");
    assert_eq!(read(&f.game.graphics_dir.join("faces/2.png")), "face2");
    assert_eq!(f.plans(false).await[0].status, Status::UpToDate);
}

#[tokio::test]
async fn repair_multiple_packs_when_graphics_folder_deleted_and_one_restored() {
    let f = Fixture::new();
    let chunk_f = f.chunk("faces", &[("faces/1.png", "face1")]);
    let chunk_l = f.chunk("logos", &[("logos/1.png", "logo1")]);
    f.publish(vec![
        package("faces", "1", Target::Graphics, vec![chunk_f.clone()]),
        package("logos", "1", Target::Graphics, vec![chunk_l.clone()]),
    ]);

    // Install both packs
    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "face1");
    assert_eq!(read(&f.game.graphics_dir.join("logos/1.png")), "logo1");

    // Customer deletes the entire graphics directory
    fs::remove_dir_all(&f.game.graphics_dir).unwrap();
    assert!(!f.game.graphics_dir.exists());

    // Both packs are detected as NeedsRepair
    let plans_after_delete = f.plans(false).await;
    assert_eq!(plans_after_delete[0].status, Status::NeedsRepair);
    assert_eq!(plans_after_delete[1].status, Status::NeedsRepair);

    // Customer repairs only faces first
    let faces_plan = plans_after_delete.iter().filter(|p| p.id == "faces").cloned().collect::<Vec<_>>();
    f.install(&faces_plan).await.unwrap();
    assert!(f.game.graphics_dir.exists());
    assert_eq!(read(&f.game.graphics_dir.join("faces/1.png")), "face1");

    // Graphics directory now exists and is NOT empty!
    // Logos must STILL be detected as NeedsRepair even with verify_content = false!
    let plans_after_faces_repair = f.plans(false).await;
    let faces_status = plans_after_faces_repair.iter().find(|p| p.id == "faces").unwrap().status;
    let logos_status = plans_after_faces_repair.iter().find(|p| p.id == "logos").unwrap().status;
    assert_eq!(faces_status, Status::UpToDate);
    assert_eq!(logos_status, Status::NeedsRepair);

    // Now repair logos
    let logos_plan = plans_after_faces_repair.iter().filter(|p| p.id == "logos").cloned().collect::<Vec<_>>();
    f.install(&logos_plan).await.unwrap();
    assert_eq!(read(&f.game.graphics_dir.join("logos/1.png")), "logo1");

    // Both packs are now UpToDate
    let final_plans = f.plans(false).await;
    assert_eq!(final_plans[0].status, Status::UpToDate);
    assert_eq!(final_plans[1].status, Status::UpToDate);
}

#[tokio::test]
async fn steam_patch_checks_only_database_and_skips_graphics() {
    let mut f = Fixture::new();
    let faces_chunk = f.chunk("faces", &[("faces/1.png", "face1")]);
    let rnf_chunk = f.chunk("rnf", &[("lnc/names.lnc", "fixed")]);

    f.publish(vec![
        package("faces", "1", Target::Graphics, vec![faces_chunk]),
        package("rnf", "1", Target::Database, vec![rnf_chunk]),
    ]);

    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();

    // Verify meta has recorded build_id "1"
    let meta = f.store.load_meta().unwrap();
    assert_eq!(meta.last_build_id.as_deref(), Some("1"));

    // Modify a graphics file content (without deleting) and modify database file content (simulating Steam patch)
    fs::write(f.game.graphics_dir.join("faces/1.png"), "tampered").unwrap();
    fs::write(f.db_file(2700, "lnc/names.lnc"), "steam patch").unwrap();

    // When build_id is unchanged and verify = false, neither triggers deep check
    let plans_same_build = f.plans(false).await;
    assert_eq!(plans_same_build.iter().find(|p| p.id == "faces").unwrap().status, Status::UpToDate);
    assert_eq!(plans_same_build.iter().find(|p| p.id == "rnf").unwrap().status, Status::UpToDate);

    // Steam updates game build from "1" to "2"
    f.game.build_id = Some("2".into());

    // When build_id changes, plan should deep check ONLY Database packs, NOT Graphics packs
    let plans_patched = f.plans(false).await;
    let faces_plan = plans_patched.iter().find(|p| p.id == "faces").unwrap();
    let rnf_plan = plans_patched.iter().find(|p| p.id == "rnf").unwrap();

    assert_eq!(faces_plan.status, Status::UpToDate, "Graphics packs must NOT do deep verify on game patch");
    assert_eq!(rnf_plan.status, Status::NeedsRepair, "Database packs MUST do deep verify on game patch");

    // Repair database pack
    f.install(&[rnf_plan.clone()]).await.unwrap();
    assert_eq!(read(&f.db_file(2700, "lnc/names.lnc")), "fixed");

    // Meta last_build_id is now updated to "2"
    let meta = f.store.load_meta().unwrap();
    assert_eq!(meta.last_build_id.as_deref(), Some("2"));

    // Next check without verify finds both UpToDate
    let final_plans = f.plans(false).await;
    assert_eq!(final_plans.iter().find(|p| p.id == "faces").unwrap().status, Status::UpToDate);
    assert_eq!(final_plans.iter().find(|p| p.id == "rnf").unwrap().status, Status::UpToDate);
}

#[tokio::test]
async fn repair_drops_chunks_no_longer_in_the_pack() {
    let f = Fixture::new();
    let a = f.chunk("rnf", &[("lnc/a.lnc", "a")]);
    let b = f.chunk("rnf", &[("lnc/b.lnc", "b")]);
    f.publish(vec![package("rnf", "1", Target::Database, vec![a.clone(), b.clone()])]);

    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();
    assert!(f.db_file(2700, "lnc/b.lnc").exists());

    // The pack is republished at the SAME version with chunk b dropped (as a
    // clean republish that shrinks the chunk set does).
    f.publish(vec![package("rnf", "1", Target::Database, vec![a.clone()])]);
    let manifest = f.client().manifest(&f.public_key).await.unwrap();
    let pkg = manifest.packages.iter().find(|p| p.id == "rnf").unwrap();

    // Repair must mark b stale, not just re-apply a — otherwise the pack stays
    // NeedsRepair after every repair.
    let repair = plan_repair(pkg, &f.game, &f.store).unwrap();
    assert!(repair.actions.iter().flat_map(|act| &act.stale).any(|s| *s == b.sha256), "chunk b harus stale");

    let cancel = Arc::new(AtomicBool::new(false));
    let ids = vec!["rnf".to_string()];
    execute(&f.client(), &[repair], &ids, &f.game, &f.store, Arc::new(|_| {}), cancel).await.unwrap();
    assert!(!f.db_file(2700, "lnc/b.lnc").exists(), "file chunk lama harus kehapus setelah repair");
    assert_eq!(f.plans(false).await[0].status, Status::UpToDate);
}

#[tokio::test]
async fn installing_graphics_after_patch_does_not_hide_broken_database() {
    let mut f = Fixture::new();
    let faces_a = f.chunk("faces", &[("faces/1.png", "a")]);
    let rnf = f.chunk("rnf", &[("lnc/names.lnc", "fixed")]);
    f.publish(vec![
        package("faces", "1", Target::Graphics, vec![faces_a.clone()]),
        package("rnf", "1", Target::Database, vec![rnf.clone()]),
    ]);

    // Install both; the game build "1" is recorded as reconciled.
    let plans = f.plans(false).await;
    f.install(&plans).await.unwrap();
    assert_eq!(f.store.load_meta().unwrap().last_build_id.as_deref(), Some("1"));

    // Steam patches the game: build 1 -> 2 overwrites the Real Name Fix, and a
    // new faces version is published.
    fs::write(f.db_file(2700, "lnc/names.lnc"), "steam patch").unwrap();
    f.game.build_id = Some("2".into());
    let faces_b = f.chunk("faces", &[("faces/1.png", "b")]);
    f.publish(vec![
        package("faces", "2", Target::Graphics, vec![faces_b.clone()]),
        package("rnf", "1", Target::Database, vec![rnf.clone()]),
    ]);

    // On open the patch flags the Database pack for repair.
    let plans = f.plans(false).await;
    assert_eq!(plans.iter().find(|p| p.id == "rnf").unwrap().status, Status::NeedsRepair);

    // The customer installs only the Graphics update, leaving the DB pack
    // unrepaired. The app always hands execute() every plan with just the
    // chosen id, so the DB pack's pending state is visible to the reconcile check.
    let ids = vec!["faces".to_string()];
    let cancel = Arc::new(AtomicBool::new(false));
    execute(&f.client(), &plans, &ids, &f.game, &f.store, Arc::new(|_| {}), cancel).await.unwrap();

    // The build must NOT be marked reconciled while a Database pack still needs
    // work, so a plain re-open keeps detecting the broken Real Name Fix.
    assert_eq!(f.store.load_meta().unwrap().last_build_id.as_deref(), Some("1"));
    let plans = f.plans(false).await;
    assert_eq!(plans.iter().find(|p| p.id == "rnf").unwrap().status, Status::NeedsRepair);
}

