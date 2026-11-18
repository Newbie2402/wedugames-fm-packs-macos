//! Publishes packs: builds them, uploads what R2 lacks, then signs and writes
//! the manifest last, so customers never see a manifest whose files are missing.

pub mod archive;
pub mod r2;

use anyhow::{bail, Context, Result};
use archive::{archives_in, extract_over};
use ed25519_dalek::SigningKey;
use fmup_core::manifest::{Manifest, Package, SignedManifest, Target, SCHEMA};
use futures_util::{stream, TryStreamExt};
use packer::{
    build_package, plan_zips, public_key_hex, sign_manifest, write_cover, FileIndex, PackOptions, Stage, ZipPlan,
};
use r2::R2;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

const MANIFEST_KEY: &str = "manifest.json";
const PARALLEL_UPLOADS: usize = 3;
const PRUNE_AFTER_SECS: i64 = 7 * 24 * 60 * 60;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackSpec {
    pub id: String,
    pub name: String,
    pub target: Target,
    #[serde(default)]
    pub min_db: Option<u32>,
    #[serde(default)]
    pub group: Option<String>,
    /// A folder of downloaded archives, or one of already extracted files.
    pub src: PathBuf,
    #[serde(default)]
    pub cover: Option<PathBuf>,
    /// Where the pack goes inside `graphics`, like `logos/FMG Steel`.
    #[serde(default)]
    pub dest: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", tag = "phase")]
pub enum Progress {
    Extracting { name: String, archive: String, done: u64, total: u64 },
    Checking { name: String, done: u64, total: u64 },
    Packing { name: String, done: u64, total: u64 },
    Uploading { done: u64, total: u64 },
    Publishing,
}

pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub published: Vec<String>,
    pub unchanged: Vec<String>,
    pub uploaded_bytes: u64,
    /// Published files no longer in the source folders. Customers keep them.
    pub missing_files: u64,
    /// Files left out because Windows can't store their names.
    pub skipped_files: u64,
}

fn index_key(id: &str) -> String {
    format!("index/{id}.json")
}

/// What a pack's last upload contained. Lives on R2, so uploading works from
/// any PC and an archive already published is never processed twice.
#[derive(Serialize, Deserialize, Default, Clone)]
struct StoredIndex {
    archives: Vec<String>,
    files: FileIndex,
}

async fn fetch_index(r2: &R2, id: &str) -> Result<Option<StoredIndex>> {
    match r2.get_text(&index_key(id)).await? {
        Some(text) => Ok(Some(serde_json::from_str(&text).context("indeks pack di R2 rusak")?)),
        None => Ok(None),
    }
}

pub async fn fetch_manifest(r2: &R2, public_key: &str) -> Result<Manifest> {
    match r2.get_text(MANIFEST_KEY).await? {
        Some(text) => {
            let signed: SignedManifest = serde_json::from_str(&text).context("manifest di R2 rusak")?;
            signed.verify(public_key)
        }
        None => Ok(Manifest { schema: SCHEMA, packages: Vec::new() }),
    }
}

async fn write_manifest(r2: &R2, manifest: &Manifest, key: &SigningKey) -> Result<()> {
    let signed = sign_manifest(manifest, key)?;
    r2.put_text(MANIFEST_KEY, serde_json::to_string(&signed)?).await
}

/// True when customers would have nothing new to download.
fn same_content(a: &Package, b: &Package) -> bool {
    let shas = |p: &Package| p.chunks.iter().map(|c| c.sha256.clone()).collect::<HashSet<_>>();
    let cover = |p: &Package| p.cover.as_ref().map(|c| c.sha256.clone());
    shas(a) == shas(b) && cover(a) == cover(b)
}

/// `today`, or `today.2`, `today.3`... when that was already used.
pub fn next_version(previous: Option<&str>, today: &str) -> String {
    match previous {
        Some(v) if v == today => format!("{today}.2"),
        Some(v) => match v.strip_prefix(today).and_then(|r| r.strip_prefix('.')?.parse::<u32>().ok()) {
            Some(n) => format!("{today}.{}", n + 1),
            None => today.to_string(),
        },
        None => today.to_string(),
    }
}

fn objects_of(package: &Package) -> Vec<(String, u64)> {
    let mut objects: Vec<_> = package.chunks.iter().map(|c| (c.key.clone(), c.size)).collect();
    objects.extend(package.cover.iter().map(|c| (c.key.clone(), c.size)));
    objects
}

async fn upload_missing(
    r2: &R2,
    work: &Path,
    objects: Vec<(String, u64)>,
    progress: &ProgressFn,
    cancel: &Arc<AtomicBool>,
) -> Result<u64> {
    let mut missing = Vec::new();
    for (key, size) in objects {
        if r2.size_of(&key).await? != Some(size) {
            let path = work.join("objects").join(&key);
            let local = tokio::fs::metadata(&path).await.map(|m| m.len()).ok();
            if local != Some(size) {
                bail!("{key} tidak ada di R2 maupun di folder kerja");
            }
            missing.push((key, path, size));
        }
    }

    let total: u64 = missing.iter().map(|m| m.2).sum();
    let done = Arc::new(AtomicU64::new(0));
    progress(Progress::Uploading { done: 0, total });
    stream::iter(missing.into_iter().map(Ok::<_, anyhow::Error>))
        .map_ok(|(key, path, size)| {
            let (r2, done, progress, cancel) = (r2.clone(), done.clone(), progress.clone(), cancel.clone());
            async move {
                if cancel.load(Ordering::Relaxed) {
                    bail!("Dibatalkan");
                }
                let on_bytes = move |n: u64| {
                    let done = done.fetch_add(n, Ordering::Relaxed) + n;
                    progress(Progress::Uploading { done, total });
                };
                r2.upload_file(&path, &key, size, &cancel, &on_bytes).await?;
                if r2.size_of(&key).await? != Some(size) {
                    bail!("Ukuran {key} di R2 tidak cocok setelah upload");
                }
                Ok(())
            }
        })
        .try_buffer_unordered(PARALLEL_UPLOADS)
        .try_collect::<Vec<()>>()
        .await?;
    Ok(total)
}

struct Prepared {
    staging: PathBuf,
    fresh: Vec<archive::Archive>,
    /// Set when the fresh archives can be copied without extracting them.
    direct: Option<ZipPlan>,
}

/// Gets the archives not published yet ready to build, oldest first: planned
/// for a direct copy when they are all zips, else extracted into a staging
/// folder. None when `spec.src` is an already extracted folder.
fn prepare_archives(
    spec: &PackSpec,
    done: &[String],
    work: &Path,
    seven_zip: Option<&Path>,
    progress: &ProgressFn,
    cancel: &AtomicBool,
) -> Result<Option<Prepared>> {
    let archives = archives_in(&spec.src)?;
    if archives.is_empty() {
        return Ok(None);
    }
    let fresh: Vec<_> = archives.into_iter().filter(|a| !done.contains(&a.name)).collect();
    let staging = work.join("staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    if fresh.is_empty() {
        return Ok(Some(Prepared { staging, fresh, direct: None }));
    }
    let is_zip = |a: &archive::Archive| a.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    if fresh.iter().all(is_zip) {
        progress(Progress::Checking { name: spec.name.clone(), done: 0, total: 1 });
        let paths: Vec<PathBuf> = fresh.iter().map(|a| a.path.clone()).collect();
        if let Some(plan) = plan_zips(&paths, spec.target == Target::Database)? {
            return Ok(Some(Prepared { staging, fresh, direct: Some(plan) }));
        }
    }
    let seven_zip = seven_zip.context("7-Zip belum terpasang. Install dari 7-zip.org lalu coba lagi")?;
    let total = fresh.len() as u64;
    for (index, item) in fresh.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("Dibatalkan");
        }
        progress(Progress::Extracting {
            name: spec.name.clone(),
            archive: item.name.clone(),
            done: index as u64,
            total,
        });
        extract_over(seven_zip, item, &work.join("extracting"), &staging, cancel)?;
    }
    // Instructions in the archive would otherwise land in the game's folders.
    if spec.target == Target::Database {
        for entry in std::fs::read_dir(&staging)?.flatten() {
            if entry.path().extension().is_some_and(|e| e.eq_ignore_ascii_case("txt")) {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    Ok(Some(Prepared { staging, fresh, direct: None }))
}

/// `today` names the version of packs without a version in their archive names,
/// such as `2026.10.31`. `seven_zip` is needed for packs that come as archives.
#[allow(clippy::too_many_arguments)]
pub async fn publish(
    r2: &R2,
    key: &SigningKey,
    work: &Path,
    specs: &[PackSpec],
    today: &str,
    seven_zip: Option<&Path>,
    progress: ProgressFn,
    cancel: Arc<AtomicBool>,
) -> Result<Report> {
    let mut manifest = fetch_manifest(r2, &public_key_hex(key)).await?;
    let mut report = Report::default();
    let mut changed = Vec::new();
    let mut indexes = Vec::new();

    for spec in specs {
        let previous = manifest.packages.iter().find(|p| p.id == spec.id).cloned();
        let stored = match &previous {
            Some(_) => fetch_index(r2, &spec.id).await?,
            None => None,
        };
        let published_archives = stored.as_ref().map(|s| s.archives.clone()).unwrap_or_default();

        let (spec_arc, work_copy, progress_copy, cancel_copy, seven_zip_copy, done_copy) = (
            spec.clone(),
            work.to_path_buf(),
            progress.clone(),
            cancel.clone(),
            seven_zip.map(Path::to_path_buf),
            published_archives.clone(),
        );
        let prepared = tokio::task::spawn_blocking(move || {
            prepare_archives(&spec_arc, &done_copy, &work_copy, seven_zip_copy.as_deref(), &progress_copy, &cancel_copy)
        })
        .await??;

        // No new archive: only the pack's details can have changed.
        if let (Some(ready), Some(old)) = (&prepared, &previous) {
            if ready.fresh.is_empty() {
                let mut package = old.clone();
                package.name = spec.name.clone();
                package.group = spec.group.clone();
                package.min_db = spec.min_db;
                if let Some(cover) = &spec.cover {
                    package.cover = Some(write_cover(cover, &spec.id, work)?);
                }
                let same_cover = package.cover.as_ref().map(|c| &c.sha256) == old.cover.as_ref().map(|c| &c.sha256);
                if same_cover && package.name == old.name && package.group == old.group && package.min_db == old.min_db {
                    report.unchanged.push(spec.id.clone());
                    continue;
                }
                report.published.push(spec.id.clone());
                changed.extend(objects_of(&package));
                manifest.packages.retain(|p| p.id != spec.id);
                manifest.packages.push(package);
                continue;
            }
        }

        let label = prepared
            .as_ref()
            .and_then(|ready| ready.fresh.last())
            .and_then(|newest| newest.version.clone())
            .unwrap_or_else(|| today.to_string());
        let version = next_version(previous.as_ref().map(|p| p.version.as_str()), &label);
        let overlay = prepared.is_some();
        let source = prepared.as_ref().map(|ready| ready.staging.clone()).unwrap_or_else(|| spec.src.clone());
        let mut archives = published_archives;
        archives.extend(prepared.iter().flat_map(|ready| &ready.fresh).map(|a| a.name.clone()));
        let index = stored.map(|s| s.files);
        let direct = prepared.and_then(|ready| ready.direct);
        report.skipped_files += direct.as_ref().map_or(0, |plan| plan.skipped);

        let (spec_copy, work_copy, previous_copy, progress_copy, cancel_copy) =
            (spec.clone(), work.to_path_buf(), previous.clone(), progress.clone(), cancel.clone());
        let output = tokio::task::spawn_blocking(move || {
            let name = spec_copy.name.clone();
            build_package(
                &PackOptions {
                    src: &source,
                    id: &spec_copy.id,
                    name: &spec_copy.name,
                    version: &version,
                    target: spec_copy.target,
                    min_db: spec_copy.min_db,
                    group: spec_copy.group.as_deref(),
                    cover: spec_copy.cover.as_deref(),
                    buckets: None,
                    previous: previous_copy.as_ref(),
                    index: index.as_ref(),
                    overlay,
                    prefix: spec_copy.dest.as_deref(),
                    direct: direct.as_ref(),
                    out: &work_copy,
                },
                &cancel_copy,
                &mut |stage, done, total| {
                    if done % 100 == 0 || done == total {
                        let name = name.clone();
                        progress_copy(match stage {
                            Stage::Checking => Progress::Checking { name, done, total },
                            Stage::Zipping => Progress::Packing { name, done, total },
                        });
                    }
                },
            )
        })
        .await??;
        // Staged files are in zips now; freeing them keeps a big pack from
        // needing twice its size on disk during the upload.
        let _ = tokio::fs::remove_dir_all(work.join("staging")).await;

        report.missing_files += output.missing_files;
        let mut package = output.package;
        match &previous {
            Some(old) if same_content(old, &package) => {
                // A renamed pack must not look like an update to customers.
                package.version = old.version.clone();
                if old.name == package.name && old.group == package.group && old.min_db == package.min_db {
                    report.unchanged.push(spec.id.clone());
                    continue;
                }
            }
            _ => {}
        }
        report.published.push(spec.id.clone());
        changed.extend(objects_of(&package));
        indexes.push((spec.id.clone(), StoredIndex { archives, files: output.index }));
        manifest.packages.retain(|p| p.id != spec.id);
        manifest.packages.push(package);
    }

    if !report.published.is_empty() {
        report.uploaded_bytes = upload_missing(r2, work, changed, &progress, &cancel).await?;
        if cancel.load(Ordering::Relaxed) {
            bail!("Dibatalkan");
        }
        progress(Progress::Publishing);
        write_manifest(r2, &manifest, key).await?;
        // After the manifest: if this fails, the next upload only repeats some
        // files in its patch. The other way round would leave changes unsent.
        for (id, index) in &indexes {
            r2.put_text(&index_key(id), serde_json::to_string(index)?).await?;
        }
    }
    let _ = tokio::fs::remove_dir_all(work.join("objects")).await;
    Ok(report)
}

/// Takes a pack out of the customer app. Its files stay on R2 until `prune`.
pub async fn unpublish(r2: &R2, key: &SigningKey, id: &str) -> Result<()> {
    let mut manifest = fetch_manifest(r2, &public_key_hex(key)).await?;
    manifest.packages.retain(|p| p.id != id);
    write_manifest(r2, &manifest, key).await?;
    r2.delete(&index_key(id)).await
}

/// Deletes files no pack uses. Only old ones: a customer who loaded the
/// previous manifest may still be downloading the rest.
pub async fn prune(r2: &R2, key: &SigningKey, now_secs: i64) -> Result<usize> {
    let manifest = fetch_manifest(r2, &public_key_hex(key)).await?;
    let live: HashSet<String> = manifest.packages.iter().flat_map(objects_of).map(|o| o.0).collect();
    let mut stored = r2.list("packs/").await?;
    stored.extend(r2.list("covers/").await?);
    let dead: Vec<String> = stored
        .into_iter()
        .filter(|o| !live.contains(&o.key) && o.modified < now_secs - PRUNE_AFTER_SECS)
        .map(|o| o.key)
        .collect();
    let count = dead.len();
    stream::iter(dead.iter().map(Ok::<_, anyhow::Error>))
        .try_for_each_concurrent(PARALLEL_UPLOADS, |key| async move { r2.delete(key).await })
        .await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::next_version;

    #[test]
    fn versions_count_up_within_a_day() {
        assert_eq!(next_version(None, "2026.10.31"), "2026.10.31");
        assert_eq!(next_version(Some("2026.10.30"), "2026.10.31"), "2026.10.31");
        assert_eq!(next_version(Some("2026.10.31"), "2026.10.31"), "2026.10.31.2");
        assert_eq!(next_version(Some("2026.10.31.2"), "2026.10.31"), "2026.10.31.3");
    }
}
