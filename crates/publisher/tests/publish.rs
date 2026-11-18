//! Publishing against a local server speaking the parts of S3 that R2 exposes.

use ed25519_dalek::SigningKey;
use fmup_core::manifest::{Manifest, SignedManifest, Target};
use packer::public_key_hex;
use publisher::r2::R2;
use publisher::{fetch_manifest, prune, publish, unpublish, PackSpec, Progress, ProgressFn};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Response, Server, StatusCode};

const BUCKET: &str = "fm-updater";

#[derive(Default)]
struct Store {
    objects: BTreeMap<String, Vec<u8>>,
    uploads: HashMap<String, BTreeMap<u32, Vec<u8>>>,
    writes: Vec<String>,
    multipart_parts: u32,
    reject_all: bool,
}

fn header(text: &str) -> Header {
    text.parse().unwrap()
}

fn serve(server: Server, store: Arc<Mutex<Store>>) {
    for mut request in server.incoming_requests() {
        let url = request.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        let key = path.trim_start_matches(&format!("/{BUCKET}")).trim_start_matches('/').to_string();
        let params: HashMap<&str, &str> = query.split('&').filter_map(|p| p.split_once('=')).collect();
        let method = request.method().to_string();
        let mut body = Vec::new();
        request.as_reader().read_to_end(&mut body).unwrap();
        let mut s = store.lock().unwrap();

        if s.reject_all {
            let xml = "<Error><Code>InvalidAccessKeyId</Code><Message>no</Message></Error>";
            let _ = request.respond(Response::from_string(xml).with_status_code(403));
            continue;
        }

        let response = match method.as_str() {
            "GET" if params.get("list-type") == Some(&"2") => {
                let prefix = params.get("prefix").map(|p| p.replace("%2F", "/")).unwrap_or_default();
                let items: String = s
                    .objects
                    .iter()
                    .filter(|(k, _)| k.starts_with(&prefix))
                    .map(|(k, v)| {
                        format!("<Contents><Key>{k}</Key><Size>{}</Size><LastModified>2026-01-01T00:00:00.000Z</LastModified></Contents>", v.len())
                    })
                    .collect();
                Response::from_string(format!("<ListBucketResult><IsTruncated>false</IsTruncated>{items}</ListBucketResult>"))
            }
            "POST" if query.starts_with("uploads") => {
                let id = format!("up{}", s.uploads.len());
                s.uploads.insert(id.clone(), BTreeMap::new());
                Response::from_string(format!(
                    "<InitiateMultipartUploadResult><Bucket>{BUCKET}</Bucket><Key>{key}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
                ))
            }
            "PUT" if params.contains_key("partNumber") => {
                let number: u32 = params["partNumber"].parse().unwrap();
                s.uploads.get_mut(params["uploadId"]).unwrap().insert(number, body);
                s.multipart_parts += 1;
                Response::from_string("").with_header(header(&format!("ETag: \"{number}\"")))
            }
            "POST" if params.contains_key("uploadId") => {
                let parts = s.uploads.remove(params["uploadId"]).unwrap();
                s.objects.insert(key.clone(), parts.into_values().flatten().collect());
                s.writes.push(key.clone());
                Response::from_string(format!(
                    "<CompleteMultipartUploadResult><Key>{key}</Key><ETag>\"x\"</ETag></CompleteMultipartUploadResult>"
                ))
            }
            "PUT" => {
                s.objects.insert(key.clone(), body);
                s.writes.push(key.clone());
                Response::from_string("").with_header(header("ETag: \"x\""))
            }
            "DELETE" => {
                s.objects.remove(&key);
                Response::from_string("").with_status_code(204)
            }
            "HEAD" => match s.objects.get(&key) {
                Some(v) => {
                    // The threshold stops tiny_http dropping the length for
                    // chunked encoding, which real S3 HEAD responses never use.
                    let response = Response::empty(200)
                        .with_header(header(&format!("Content-Length: {}", v.len())))
                        .with_chunked_threshold(usize::MAX);
                    let _ = request.respond(response);
                    continue;
                }
                None => {
                    let _ = request.respond(Response::empty(StatusCode(404)));
                    continue;
                }
            },
            _ => match s.objects.get(&key) {
                Some(v) => Response::from_data(v.clone()),
                None => Response::from_string("<Error><Code>NoSuchKey</Code></Error>").with_status_code(404),
            },
        };
        let _ = request.respond(response);
    }
}

struct Fixture {
    tmp: tempfile::TempDir,
    r2: R2,
    store: Arc<Mutex<Store>>,
    key: SigningKey,
}

fn quiet() -> ProgressFn {
    Arc::new(|_| {})
}

fn cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

impl Fixture {
    fn new() -> Self {
        let server = Server::http("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", server.server_addr());
        let store = Arc::new(Mutex::new(Store::default()));
        std::thread::spawn({
            let store = store.clone();
            move || serve(server, store)
        });
        Self {
            tmp: tempfile::tempdir().unwrap(),
            r2: R2::with_endpoint(&endpoint, "access", "secret", BUCKET),
            store,
            key: SigningKey::from_bytes(&[5u8; 32]),
        }
    }

    fn pack(&self, name: &str, files: &[(&str, &str)]) -> PackSpec {
        let src = self.tmp.path().join("src").join(name);
        let _ = fs::remove_dir_all(&src);
        for (path, body) in files {
            let dest = src.join(path);
            fs::create_dir_all(dest.parent().unwrap()).unwrap();
            fs::write(dest, body).unwrap();
        }
        PackSpec {
            id: name.into(),
            name: name.into(),
            target: Target::Graphics,
            min_db: None,
            group: Some("faces".into()),
            src,
            cover: None,
            dest: None,
        }
    }

    fn work(&self) -> std::path::PathBuf {
        self.tmp.path().join("work")
    }

    async fn publish(&self, specs: &[PackSpec], today: &str) -> publisher::Report {
        publish(&self.r2, &self.key, &self.work(), specs, today, None, quiet(), cancel()).await.unwrap()
    }

    fn manifest(&self) -> Manifest {
        let store = self.store.lock().unwrap();
        let signed: SignedManifest = serde_json::from_slice(&store.objects["manifest.json"]).unwrap();
        signed.verify(&public_key_hex(&self.key)).unwrap()
    }

    fn writes(&self) -> Vec<String> {
        std::mem::take(&mut self.store.lock().unwrap().writes)
    }
}

#[tokio::test]
async fn publishes_chunks_first_and_manifest_last() {
    let f = Fixture::new();
    let cover = f.tmp.path().join("cover.jpg");
    fs::write(&cover, "image").unwrap();
    let mut spec = f.pack("faces", &[("faces/1.png", "one"), ("faces/2.png", "two")]);
    spec.cover = Some(cover);

    let report = f.publish(&[spec], "2026.10.31").await;
    assert_eq!(report.published, vec!["faces"]);

    let writes = f.writes();
    let manifest_at = writes.iter().position(|k| k == "manifest.json").unwrap();
    assert!(writes[..manifest_at].iter().any(|k| k.starts_with("packs/")));
    assert!(writes[manifest_at + 1..].iter().all(|k| k.starts_with("index/")));

    let manifest = f.manifest();
    let package = &manifest.packages[0];
    assert_eq!(package.version, "2026.10.31");
    let store = f.store.lock().unwrap();
    for chunk in &package.chunks {
        assert_eq!(hex::encode(Sha256::digest(&store.objects[&chunk.key])), chunk.sha256);
    }
    assert_eq!(store.objects[&package.cover.as_ref().unwrap().key], b"image");
    drop(store);
    assert!(!f.work().join("objects").exists());
}

#[tokio::test]
async fn unchanged_pack_stays_silent_and_a_change_bumps_the_version() {
    let f = Fixture::new();
    let spec = f.pack("faces", &[("a.png", "one")]);
    f.publish(&[spec.clone()], "2026.10.31").await;
    f.writes();

    let report = f.publish(&[spec], "2026.11.01").await;
    assert_eq!(report.unchanged, vec!["faces"]);
    assert!(f.writes().is_empty());
    assert_eq!(f.manifest().packages[0].version, "2026.10.31");

    let spec = f.pack("faces", &[("a.png", "two")]);
    f.publish(&[spec], "2026.11.01").await;
    assert_eq!(f.manifest().packages[0].version, "2026.11.01");
}

#[tokio::test]
async fn an_update_becomes_a_small_patch_over_the_base() {
    let f = Fixture::new();
    let names: Vec<String> = (0..500).map(|i| format!("faces/{i}.png")).collect();
    let originals: Vec<String> = (0..500).map(|i| format!("face {i}")).collect();
    let files: Vec<(&str, &str)> = names.iter().map(String::as_str).zip(originals.iter().map(String::as_str)).collect();
    let spec = f.pack("faces", &files);
    f.publish(&[spec.clone()], "2026.10.31").await;
    let base = f.manifest().packages[0].chunks.clone();

    // Forty changed files plus a new one, overlaid like a real update file.
    for i in 0..40 {
        fs::write(spec.src.join(format!("faces/{i}.png")), format!("new face {i}")).unwrap();
    }
    fs::write(spec.src.join("faces/new.png"), "brand new").unwrap();
    f.writes();
    f.publish(&[spec], "2026.11.01").await;

    let chunks = f.manifest().packages[0].chunks.clone();
    assert_eq!(chunks.len(), base.len() + 1);
    assert_eq!(chunks[..base.len()].iter().map(|c| &c.sha256).collect::<Vec<_>>(), base.iter().map(|c| &c.sha256).collect::<Vec<_>>());
    let patch = chunks.last().unwrap();
    assert!(patch.patch);
    assert_eq!(patch.files, 41);

    // Nothing from the base travels again.
    let uploaded: Vec<String> = f.writes().into_iter().filter(|k| k.starts_with("packs/")).collect();
    assert_eq!(uploaded, vec![patch.key.clone()]);
}

#[tokio::test]
async fn patches_stack_and_restoring_a_file_is_still_a_new_patch() {
    let f = Fixture::new();
    let spec = f.pack("faces", &[("a.png", "one"), ("b.png", "keep")]);
    f.publish(&[spec.clone()], "2026.10.31").await;

    fs::write(spec.src.join("a.png"), "two").unwrap();
    f.publish(&[spec.clone()], "2026.11.01").await;
    fs::write(spec.src.join("a.png"), "three").unwrap();
    f.publish(&[spec.clone()], "2026.12.01").await;
    fs::write(spec.src.join("a.png"), "two").unwrap();
    f.publish(&[spec], "2027.01.01").await;

    let manifest = f.manifest();
    let chunks = &manifest.packages[0].chunks;
    assert_eq!(chunks.iter().filter(|c| c.patch).count(), 3);
    let keys: std::collections::HashSet<_> = chunks.iter().map(|c| &c.key).collect();
    assert_eq!(keys.len(), chunks.len());
}

#[tokio::test]
async fn a_file_removed_from_the_folder_is_reported_and_stays_published() {
    let f = Fixture::new();
    let spec = f.pack("faces", &[("a.png", "one"), ("b.png", "two")]);
    f.publish(&[spec.clone()], "2026.10.31").await;

    fs::remove_file(spec.src.join("b.png")).unwrap();
    fs::write(spec.src.join("a.png"), "changed").unwrap();
    let report = f.publish(&[spec.clone()], "2026.11.01").await;
    assert_eq!(report.missing_files, 1);

    // Still known, so restoring it unchanged is not an update.
    fs::write(spec.src.join("b.png"), "two").unwrap();
    let report = f.publish(&[spec], "2026.12.01").await;
    assert_eq!(report.unchanged, vec!["faces"]);
}

/// Makes a zip shaped like the real packs: one wrapper folder named after the version.
fn write_archive(dir: &Path, name: &str, wrapper: &str, files: &[(&str, &str)]) {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    fs::create_dir_all(dir).unwrap();
    let mut zip = zip::ZipWriter::new(fs::File::create(dir.join(name)).unwrap());
    for (path, body) in files {
        zip.start_file(format!("{wrapper}/{path}"), SimpleFileOptions::default()).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn zip_entries(store: &Store, key: &str) -> BTreeMap<String, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(store.objects[key].clone())).unwrap();
    (0..archive.len())
        .map(|i| {
            let mut file = archive.by_index(i).unwrap();
            let mut body = String::new();
            std::io::Read::read_to_string(&mut file, &mut body).unwrap();
            (file.name().to_string(), body)
        })
        .collect()
}

#[tokio::test]
async fn publishes_a_folder_of_versioned_archives_like_the_real_packs() {
    let Some(seven_zip) = publisher::archive::find_seven_zip() else {
        eprintln!("7-Zip not installed, skipping");
        return;
    };
    let f = Fixture::new();
    let folder = f.tmp.path().join("archives");
    let wrap = |v: &str| format!("FMG Steel Logos {v}");
    write_archive(&folder, "FMG Steel Logos 2026.00.zip", &wrap("2026.00"), &[("Clubs/Normal/1.png", "base"), ("Clubs/Normal/2.png", "keep"), ("Clubs/Normal/config.xml", "c0")]);
    write_archive(&folder, "FMG Steel Logos 2026.02.zip", &wrap("2026.02"), &[("Clubs/Normal/1.png", "two"), ("Clubs/Normal/config.xml", "c2")]);
    write_archive(&folder, "FMG Steel Logos 2026.01.zip", &wrap("2026.01"), &[("Clubs/Normal/1.png", "one"), ("Women/Clubs/Normal/9.png", "w")]);

    let spec = PackSpec {
        id: "fmg-steel".into(),
        name: "FMG Steel".into(),
        target: Target::Graphics,
        min_db: None,
        group: Some("logos".into()),
        src: folder.clone(),
        cover: None,
        dest: Some("logos\\FMG Steel".into()),
    };
    let run = |spec: PackSpec| {
        let (r2, key, work, seven_zip) = (f.r2.clone(), f.key.clone(), f.work(), seven_zip.clone());
        async move { publish(&r2, &key, &work, &[spec], "2026.12.31", Some(&seven_zip), quiet(), cancel()).await.unwrap() }
    };

    // First upload: every archive overlaid in version order, version from the newest.
    run(spec.clone()).await;
    let package = f.manifest().packages[0].clone();
    assert_eq!(package.version, "2026.02");
    assert_eq!(package.chunks.len(), 1);
    let base = zip_entries(&f.store.lock().unwrap(), &package.chunks[0].key);
    assert_eq!(base["logos/FMG Steel/Clubs/Normal/1.png"], "two");
    assert_eq!(base["logos/FMG Steel/Clubs/Normal/2.png"], "keep");
    assert_eq!(base["logos/FMG Steel/Clubs/Normal/config.xml"], "c2");
    assert_eq!(base["logos/FMG Steel/Women/Clubs/Normal/9.png"], "w");
    assert!(base.keys().all(|k| !k.contains("2026.0")), "wrapper folder must be gone");

    // Nothing new: nothing extracted, nothing uploaded.
    f.writes();
    let report = run(spec.clone()).await;
    assert_eq!(report.unchanged, vec!["fmg-steel"]);
    assert!(f.writes().is_empty());

    // A new archive alone becomes the patch.
    write_archive(&folder, "FMG Steel Logos 2026.03.zip", &wrap("2026.03"), &[("Clubs/Normal/1.png", "three")]);
    let report = run(spec).await;
    assert_eq!(report.missing_files, 0);
    let package = f.manifest().packages[0].clone();
    assert_eq!(package.version, "2026.03");
    let patch = package.chunks.last().unwrap();
    assert!(patch.patch);
    let entries = zip_entries(&f.store.lock().unwrap(), &patch.key);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries["logos/FMG Steel/Clubs/Normal/1.png"], "three");
}

#[tokio::test]
async fn non_zip_archives_are_extracted_and_give_the_same_result() {
    let Some(seven_zip) = publisher::archive::find_seven_zip() else {
        return;
    };
    let f = Fixture::new();
    let folder = f.tmp.path().join("archives");
    let source = f.tmp.path().join("source/Pack 2026.01/Clubs");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("1.png"), "from 7z").unwrap();
    fs::create_dir_all(&folder).unwrap();
    write_archive(&folder, "Pack 2026.00.zip", "Pack 2026.00", &[("Clubs/1.png", "old"), ("Clubs/2.png", "keep")]);
    let status = std::process::Command::new(&seven_zip)
        .args(["a", "-bso0", "-bsp0"])
        .arg(folder.join("Pack 2026.01.7z"))
        .arg(source.parent().unwrap())
        .status()
        .unwrap();
    assert!(status.success());

    let spec = PackSpec {
        id: "pack".into(),
        name: "Pack".into(),
        target: Target::Graphics,
        min_db: None,
        group: None,
        src: folder,
        cover: None,
        dest: None,
    };
    publish(&f.r2, &f.key, &f.work(), &[spec], "2026.10.31", Some(&seven_zip), quiet(), cancel()).await.unwrap();
    let package = f.manifest().packages[0].clone();
    assert_eq!(package.version, "2026.01");
    let entries = zip_entries(&f.store.lock().unwrap(), &package.chunks[0].key);
    assert_eq!(entries["Clubs/1.png"], "from 7z");
    assert_eq!(entries["Clubs/2.png"], "keep");
}

#[tokio::test]
async fn database_packs_drop_the_instructions_file() {
    let Some(seven_zip) = publisher::archive::find_seven_zip() else {
        return;
    };
    let f = Fixture::new();
    let folder = f.tmp.path().join("rnf");
    write_archive(&folder, "Real Name Fix V2.zip", "Real Name Fix", &[("lnc/a.lnc", "x"), ("Installation Instructions.txt", "read me")]);
    let spec = PackSpec {
        id: "rnf".into(),
        name: "Real Name Fix".into(),
        target: Target::Database,
        min_db: Some(1),
        group: None,
        src: folder,
        cover: None,
        dest: None,
    };
    publish(&f.r2, &f.key, &f.work(), &[spec], "2026.10.31", Some(&seven_zip), quiet(), cancel()).await.unwrap();
    let package = f.manifest().packages[0].clone();
    let entries = zip_entries(&f.store.lock().unwrap(), &package.chunks[0].key);
    assert_eq!(entries.keys().collect::<Vec<_>>(), vec!["lnc/a.lnc"]);
}

#[tokio::test]
async fn renaming_is_published_without_a_new_version() {
    let f = Fixture::new();
    let mut spec = f.pack("faces", &[("a.png", "one")]);
    f.publish(&[spec.clone()], "2026.10.31").await;

    spec.name = "New name".into();
    f.publish(&[spec], "2026.11.01").await;
    let manifest = f.manifest();
    assert_eq!(manifest.packages[0].name, "New name");
    assert_eq!(manifest.packages[0].version, "2026.10.31");
}

#[tokio::test]
async fn large_file_goes_up_in_parts() {
    let f = Fixture::new();
    let path = f.tmp.path().join("large.bin");
    let data: Vec<u8> = (0..110 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    fs::write(&path, &data).unwrap();

    let sent = Arc::new(Mutex::new(0u64));
    let counter = sent.clone();
    let on_bytes = move |n: u64| *counter.lock().unwrap() += n;
    f.r2.upload_file(Path::new(&path), "packs/x/big.zip", data.len() as u64, &AtomicBool::new(false), &on_bytes)
        .await
        .unwrap();

    let store = f.store.lock().unwrap();
    assert_eq!(store.multipart_parts, 2);
    assert_eq!(store.objects["packs/x/big.zip"], data);
    assert_eq!(*sent.lock().unwrap(), data.len() as u64);
}

#[tokio::test]
async fn cancel_keeps_the_published_manifest() {
    let f = Fixture::new();
    f.publish(&[f.pack("faces", &[("a.png", "one")])], "2026.10.31").await;
    let before = f.manifest().packages[0].chunks[0].sha256.clone();

    let cancelled = Arc::new(AtomicBool::new(true));
    let spec = f.pack("faces", &[("a.png", "two")]);
    assert!(publish(&f.r2, &f.key, &f.work(), &[spec], "2026.11.01", None, quiet(), cancelled).await.is_err());
    assert_eq!(f.manifest().packages[0].chunks[0].sha256, before);
}

#[tokio::test]
async fn refuses_a_manifest_signed_with_another_key() {
    let f = Fixture::new();
    f.publish(&[f.pack("faces", &[("a.png", "one")])], "2026.10.31").await;
    let other = SigningKey::from_bytes(&[6u8; 32]);
    assert!(publish(&f.r2, &other, &f.work(), &[], "2026.11.01", None, quiet(), cancel()).await.is_err());
}

#[tokio::test]
async fn unpublish_then_prune_removes_files() {
    let f = Fixture::new();
    f.publish(&[f.pack("faces", &[("a.png", "one")]), f.pack("logos", &[("b.png", "two")])], "2026.10.31").await;

    unpublish(&f.r2, &f.key, "faces").await.unwrap();
    assert_eq!(f.manifest().packages.len(), 1);

    let recent = 1_767_225_600; // the mock reports 2026-01-01 for every object
    assert_eq!(prune(&f.r2, &f.key, recent).await.unwrap(), 0);

    let later = recent + 8 * 24 * 60 * 60;
    assert_eq!(prune(&f.r2, &f.key, later).await.unwrap(), 1);
    let store = f.store.lock().unwrap();
    assert!(store.objects.keys().all(|k| !k.starts_with("packs/faces/")));
    assert!(store.objects.keys().any(|k| k.starts_with("packs/logos/")));
}

#[tokio::test]
async fn explains_rejected_credentials() {
    let f = Fixture::new();
    f.store.lock().unwrap().reject_all = true;
    let error = fetch_manifest(&f.r2, &public_key_hex(&f.key)).await.unwrap_err().to_string();
    assert!(error.contains("Pengaturan"), "{error}");
}

#[tokio::test]
async fn reports_progress_through_every_phase() {
    let f = Fixture::new();
    let phases = Arc::new(Mutex::new(Vec::new()));
    let seen = phases.clone();
    let progress: ProgressFn = Arc::new(move |p: Progress| {
        seen.lock().unwrap().push(match p {
            Progress::Extracting { .. } => "extracting",
            Progress::Checking { .. } => "checking",
            Progress::Packing { .. } => "packing",
            Progress::Uploading { .. } => "uploading",
            Progress::Publishing => "publishing",
        })
    });
    let spec = f.pack("faces", &[("a.png", "one")]);
    publish(&f.r2, &f.key, &f.work(), &[spec], "2026.10.31", None, progress, cancel()).await.unwrap();
    let phases = phases.lock().unwrap();
    assert_eq!(phases.first(), Some(&"checking"));
    assert_eq!(phases.last(), Some(&"publishing"));
    assert!(phases.contains(&"packing"));
    assert!(phases.contains(&"uploading"));
}
