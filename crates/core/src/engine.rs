use crate::detect::GameInfo;
use crate::download::Client;
use crate::install::{apply_chunk, remove_files, verify_files};
use crate::manifest::{Chunk, Cover, Manifest, Package, Target};
use crate::state::{FileEntry, PackageState, Store};
use anyhow::{bail, Result};
use futures_util::{stream, TryStreamExt};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PARALLEL_DOWNLOADS: usize = 3;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    NotInstalled,
    UpToDate,
    UpdateAvailable,
    NeedsRepair,
    Incompatible,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RootAction {
    pub root: String,
    pub apply: Vec<Chunk>,
    /// Chunks installed here that the current version no longer has.
    pub stale: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackagePlan {
    pub id: String,
    pub name: String,
    pub target: Target,
    pub group: Option<String>,
    pub cover: Option<Cover>,
    pub total_bytes: u64,
    pub total_files: u64,
    pub latest_version: String,
    pub installed_version: Option<String>,
    pub status: Status,
    pub download_bytes: u64,
    pub unpacked_bytes: u64,
    pub actions: Vec<RootAction>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", tag = "phase")]
pub enum Progress {
    Downloading { done: u64, total: u64 },
    Installing { done: u64, total: u64 },
}

pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

fn roots_for(pkg: &Package, game: &GameInfo) -> Vec<PathBuf> {
    match pkg.target {
        Target::Graphics => vec![game.graphics_dir.clone()],
        Target::Database => {
            let min = pkg.min_db.unwrap_or(u32::MAX);
            game.db_versions.iter().filter(|v| **v >= min).map(|v| game.db_root(*v)).collect()
        }
    }
}

fn key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn root_is_empty(root: &Path) -> bool {
    match std::fs::read_dir(root) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => true,
    }
}

/// Instantaneous probe across up to 5 evenly distributed files in a chunk.
/// Detects missing/deleted packs or directories without expensive hash checks.
fn is_chunk_missing(root: &Path, files: &[FileEntry]) -> bool {
    if files.is_empty() || !root.exists() {
        return true;
    }
    let len = files.len();
    let sample_indices = [0, len / 4, len / 2, (3 * len) / 4, len.saturating_sub(1)];
    for &idx in &sample_indices {
        if idx < len && !root.join(&files[idx].path).exists() {
            return true;
        }
    }
    false
}

/// Chunks with a damaged file. Only the newest version of each path counts:
/// an older one was replaced by a patch on purpose.
fn broken_chunks(pkg: &Package, root: &Path, have: &BTreeSet<String>, store: &Store) -> Result<HashSet<String>> {
    if !root.exists() || root_is_empty(root) {
        return Ok(have.iter().cloned().collect());
    }
    let mut newest: HashMap<String, (usize, FileEntry)> = HashMap::new();
    let mut chunks_with_files = HashSet::new();
    for (index, chunk) in pkg.chunks.iter().enumerate().filter(|(_, c)| have.contains(&c.sha256)) {
        let files = store.load_files(&pkg.id, &chunk.sha256)?;
        if !files.is_empty() {
            chunks_with_files.insert(chunk.sha256.clone());
        }
        for file in files {
            newest.insert(file.path.clone(), (index, file));
        }
    }
    let mut by_chunk: HashMap<usize, Vec<FileEntry>> = HashMap::new();
    for (index, file) in newest.into_values() {
        by_chunk.entry(index).or_default().push(file);
    }
    let deep = pkg.target == Target::Database;
    let mut broken: HashSet<String> = by_chunk
        .into_iter()
        .filter(|(_, files)| files.is_empty() || !verify_files(root, &files, deep))
        .map(|(index, _)| pkg.chunks[index].sha256.clone())
        .collect();

    for sha in have {
        if !chunks_with_files.contains(sha) {
            broken.insert(sha.clone());
        }
    }

    Ok(broken)
}

/// `verify_content` checks installed files as well as the recorded state. It is
/// needed after a game patch, which can overwrite files without us noticing.
pub fn plan(manifest: &Manifest, game: &GameInfo, store: &Store, verify_content: bool) -> Result<Vec<PackagePlan>> {
    let meta = store.load_meta().ok();
    let game_patched = match (&game.build_id, meta.as_ref().and_then(|m| m.last_build_id.as_ref())) {
        (Some(curr), Some(last)) => curr != last,
        _ => false,
    };
    let mut plans = Vec::new();
    for pkg in &manifest.packages {
        let needs_content_check = verify_content || (game_patched && pkg.target == Target::Database);
        let state = store.load_package(&pkg.id)?;
        let roots = roots_for(pkg, game);
        let want: HashSet<&str> = pkg.chunks.iter().map(|c| c.sha256.as_str()).collect();
        let empty = BTreeSet::new();
        let mut actions = Vec::new();
        let mut repair = false;

        for root in &roots {
            let root_key = key(root);
            let have = state.as_ref().and_then(|s| s.roots.get(&root_key)).unwrap_or(&empty);
            let broken = if !have.is_empty() && (!root.exists() || root_is_empty(root)) {
                have.iter().cloned().collect()
            } else if needs_content_check {
                broken_chunks(pkg, root, have, store)?
            } else if !have.is_empty() {
                let mut quick_broken = HashSet::new();
                for sha in have {
                    let files = store.load_files(&pkg.id, sha)?;
                    if is_chunk_missing(root, &files) {
                        quick_broken.insert(sha.clone());
                    }
                }
                quick_broken
            } else {
                HashSet::new()
            };
            repair |= !broken.is_empty();

            // Writing a chunk again undoes the patches over it, so every patch
            // after the first chunk needing work is written again too.
            let needed = |c: &Chunk| !have.contains(&c.sha256) || broken.contains(&c.sha256);
            let first = pkg.chunks.iter().position(needed);
            let apply: Vec<Chunk> = pkg
                .chunks
                .iter()
                .enumerate()
                .filter(|(i, c)| needed(c) || (c.patch && first.is_some_and(|f| *i > f)))
                .map(|(_, c)| c.clone())
                .collect();

            let stale: Vec<String> = have.iter().filter(|s| !want.contains(s.as_str())).cloned().collect();
            if !apply.is_empty() || !stale.is_empty() {
                actions.push(RootAction { root: root_key, apply, stale });
            }
        }

        // A patch can drop a database folder we installed into.
        if let Some(state) = &state {
            let current: HashSet<String> = roots.iter().map(|r| key(r)).collect();
            for (root, chunks) in state.roots.iter().filter(|(r, _)| !current.contains(*r)) {
                actions.push(RootAction {
                    root: root.clone(),
                    apply: Vec::new(),
                    stale: chunks.iter().cloned().collect(),
                });
            }
        }

        let same_version = state.as_ref().is_some_and(|s| s.version == pkg.version);
        // A new database folder after a patch shows up here as chunks missing
        // from a root, with the package version unchanged.
        let status = if roots.is_empty() {
            Status::Incompatible
        } else if state.is_none() {
            Status::NotInstalled
        } else if actions.is_empty() {
            Status::UpToDate
        } else if same_version || repair {
            Status::NeedsRepair
        } else {
            Status::UpdateAvailable
        };

        let unique: HashMap<&str, &Chunk> =
            actions.iter().flat_map(|a| &a.apply).map(|c| (c.sha256.as_str(), c)).collect();
        plans.push(PackagePlan {
            id: pkg.id.clone(),
            name: pkg.name.clone(),
            target: pkg.target,
            group: pkg.group.clone(),
            cover: pkg.cover.clone(),
            total_bytes: pkg.chunks.iter().filter(|c| !c.patch).map(|c| c.unpacked).sum(),
            total_files: pkg.chunks.iter().filter(|c| !c.patch).map(|c| c.files).sum(),
            latest_version: pkg.version.clone(),
            installed_version: state.map(|s| s.version),
            status,
            download_bytes: unique.values().map(|c| c.size).sum(),
            unpacked_bytes: actions.iter().flat_map(|a| &a.apply).map(|c| c.unpacked).sum(),
            actions,
        });
    }
    Ok(plans)
}

/// Creates a plan that forces a full repair / reinstall of the specified package.
pub fn plan_repair(pkg: &Package, game: &GameInfo, store: &Store) -> Result<PackagePlan> {
    let state = store.load_package(&pkg.id)?;
    let roots = roots_for(pkg, game);
    let want: HashSet<&str> = pkg.chunks.iter().map(|c| c.sha256.as_str()).collect();
    let empty = BTreeSet::new();
    let mut actions = Vec::new();
    for root in &roots {
        let root_key = key(root);
        // Chunks installed here that the current version no longer has must be
        // removed, or the pack stays NeedsRepair after every repair.
        let have = state.as_ref().and_then(|s| s.roots.get(&root_key)).unwrap_or(&empty);
        let stale: Vec<String> = have.iter().filter(|s| !want.contains(s.as_str())).cloned().collect();
        actions.push(RootAction {
            root: root_key,
            apply: pkg.chunks.clone(),
            stale,
        });
    }
    // Chunks left in a root the pack no longer targets are stale too.
    if let Some(state) = &state {
        let current: HashSet<String> = roots.iter().map(|r| key(r)).collect();
        for (root, chunks) in state.roots.iter().filter(|(r, _)| !current.contains(*r)) {
            actions.push(RootAction { root: root.clone(), apply: Vec::new(), stale: chunks.iter().cloned().collect() });
        }
    }
    let unique: HashMap<&str, &Chunk> =
        actions.iter().flat_map(|a| &a.apply).map(|c| (c.sha256.as_str(), c)).collect();

    Ok(PackagePlan {
        id: pkg.id.clone(),
        name: pkg.name.clone(),
        target: pkg.target,
        group: pkg.group.clone(),
        cover: pkg.cover.clone(),
        total_bytes: pkg.chunks.iter().filter(|c| !c.patch).map(|c| c.unpacked).sum(),
        total_files: pkg.chunks.iter().filter(|c| !c.patch).map(|c| c.files).sum(),
        latest_version: pkg.version.clone(),
        installed_version: state.map(|s| s.version),
        status: Status::NeedsRepair,
        download_bytes: unique.values().map(|c| c.size).sum(),
        unpacked_bytes: actions.iter().flat_map(|a| &a.apply).map(|c| c.unpacked).sum(),
        actions,
    })
}

fn nearest_existing(path: &Path) -> Option<&Path> {
    path.ancestors().find(|p| p.exists())
}

fn check_space(path: &Path, needed: u64) -> Result<()> {
    let Some(existing) = nearest_existing(path) else {
        return Ok(());
    };
    let free = fs2::available_space(existing)?;
    if free < needed {
        bail!(
            "Ruang disk kurang di {}: butuh {:.1} GB, tersisa {:.1} GB",
            existing.display(),
            needed as f64 / 1e9,
            free as f64 / 1e9
        );
    }
    Ok(())
}

pub async fn execute(
    client: &Client,
    plans: &[PackagePlan],
    ids: &[String],
    game: &GameInfo,
    store: &Store,
    progress: ProgressFn,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let selected: Vec<&PackagePlan> = plans
        .iter()
        .filter(|p| ids.contains(&p.id) && p.status != Status::Incompatible)
        .collect();

    let mut groups = HashSet::new();
    for group in selected.iter().filter_map(|p| p.group.as_ref()) {
        if !groups.insert(group) {
            bail!("Pilih satu pack saja per kategori");
        }
    }
    // Swapped out only after every download succeeded, so a failed download
    // leaves the customer with their current pack.
    let replaced: Vec<String> = plans
        .iter()
        .filter(|p| p.installed_version.is_some() && !ids.contains(&p.id))
        .filter(|p| p.group.as_ref().is_some_and(|g| groups.contains(g)))
        .map(|p| p.id.clone())
        .collect();

    let mut chunks: HashMap<String, Chunk> = HashMap::new();
    for chunk in selected.iter().flat_map(|p| &p.actions).flat_map(|a| &a.apply) {
        chunks.insert(chunk.sha256.clone(), chunk.clone());
    }
    let download_total: u64 = chunks.values().map(|c| c.size).sum();
    check_space(&store.cache_dir(), download_total)?;
    for plan in &selected {
        if let Some(action) = plan.actions.first() {
            check_space(Path::new(&action.root), plan.unpacked_bytes)?;
        }
    }

    let done = Arc::new(AtomicU64::new(0));
    let reporter = {
        let (done, progress) = (done.clone(), progress.clone());
        tokio::spawn(async move {
            loop {
                progress(Progress::Downloading { done: done.load(Ordering::Relaxed), total: download_total });
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
    };
    let cache = store.cache_dir();
    let downloaded = stream::iter(chunks.values().cloned().map(Ok::<_, anyhow::Error>))
        .map_ok(|chunk| {
            let (client, cache, done, cancel) = (client.clone(), cache.clone(), done.clone(), cancel.clone());
            async move { client.fetch_chunk(&chunk, &cache, &done, &cancel).await.map(|_| ()) }
        })
        .try_buffer_unordered(PARALLEL_DOWNLOADS)
        .try_collect::<Vec<()>>()
        .await;
    reporter.abort();
    downloaded?;

    let owned_plans: Vec<PackagePlan> = selected.into_iter().cloned().collect();
    // Mark the game build reconciled only when no Database pack is left needing
    // work: a DB pack is handled if it is in this install or already clean.
    // Otherwise installing a Graphics pack after a Steam patch would advance the
    // build id and hide a Database pack (e.g. Real Name Fix) the patch broke.
    let db_reconciled = plans
        .iter()
        .filter(|p| p.target == Target::Database)
        .all(|p| ids.contains(&p.id) || matches!(p.status, Status::UpToDate | Status::NotInstalled | Status::Incompatible));
    let (build, store_copy) = (game.build_id.clone(), store.clone());
    tokio::task::spawn_blocking(move || -> Result<()> {
        let total: u64 = owned_plans.iter().flat_map(|p| &p.actions).flat_map(|a| &a.apply).map(|c| c.files).sum();
        let mut done = 0u64;
        for id in &replaced {
            uninstall(id, &store_copy)?;
        }
        let mut uses: HashMap<String, usize> = HashMap::new();
        for chunk in owned_plans.iter().flat_map(|p| &p.actions).flat_map(|a| &a.apply) {
            *uses.entry(chunk.sha256.clone()).or_default() += 1;
        }
        for plan in &owned_plans {
            apply_plan(plan, &store_copy, &cancel, &mut uses, &mut |n| {
                done += n;
                progress(Progress::Installing { done, total });
            })?;
        }
        if db_reconciled {
            let mut meta = store_copy.load_meta()?;
            meta.last_build_id = build;
            store_copy.save_meta(&meta)?;
        }
        Ok(())
    })
    .await?
}

fn apply_plan(
    plan: &PackagePlan,
    store: &Store,
    cancel: &AtomicBool,
    uses: &mut HashMap<String, usize>,
    on_files: &mut dyn FnMut(u64),
) -> Result<()> {
    let mut state = store.load_package(&plan.id)?.unwrap_or_default();
    // Recorded before any file is written: a crash leaves the root marked as
    // having the old chunks only, and the next run finishes the job.
    let keep_version = state.version.clone();

    for action in &plan.actions {
        let root = Path::new(&action.root);
        let backup = store.backup_dir(&plan.id, &action.root);

        let installed = state.roots.get(&action.root).cloned().unwrap_or_default();
        let mut owned: HashMap<String, Vec<u32>> = HashMap::new();
        let mut kept: HashSet<String> = HashSet::new();
        for sha in installed.iter().chain(action.apply.iter().map(|c| &c.sha256)) {
            for file in store.load_files(&plan.id, sha)? {
                if !action.stale.contains(sha) {
                    kept.insert(file.path.clone());
                }
                owned.entry(file.path).or_default().push(file.crc);
            }
        }

        let mut written: HashSet<String> = HashSet::new();
        for chunk in &action.apply {
            let zip = store.cache_dir().join(format!("{}.zip", chunk.sha256));
            let files = apply_chunk(&zip, root, &owned, &backup, cancel, || on_files(1))?;
            // A patch in this same run replaces what this chunk just wrote, and
            // that must not be mistaken for an original worth backing up.
            for file in &files {
                written.insert(file.path.clone());
                owned.entry(file.path.clone()).or_default().push(file.crc);
            }
            store.save_files(&plan.id, &chunk.sha256, &files)?;
            state.roots.entry(action.root.clone()).or_default().insert(chunk.sha256.clone());
            state.version = keep_version.clone();
            store.save_package(&plan.id, &state)?;

            // Each zip goes as soon as its files are in place, so disk use
            // stays near the pack's size instead of double it.
            if let Some(left) = uses.get_mut(&chunk.sha256) {
                *left -= 1;
                if *left == 0 {
                    let _ = std::fs::remove_file(&zip);
                }
            }
        }

        for sha in &action.stale {
            let leaving: Vec<_> = store
                .load_files(&plan.id, sha)?
                .into_iter()
                .filter(|f| !written.contains(&f.path) && !kept.contains(&f.path))
                .collect();
            remove_files(root, &leaving, &backup)?;
            if let Some(set) = state.roots.get_mut(&action.root) {
                set.remove(sha);
                if set.is_empty() {
                    state.roots.remove(&action.root);
                }
            }
            store.save_package(&plan.id, &state)?;
        }
    }

    state.version = plan.latest_version.clone();
    store.save_package(&plan.id, &state)?;
    store.prune_files(&plan.id, &state)
}

pub fn uninstall(id: &str, store: &Store) -> Result<()> {
    let Some(state): Option<PackageState> = store.load_package(id)? else {
        return Ok(());
    };
    for (root, chunks) in &state.roots {
        let backup = store.backup_dir(id, root);
        let mut files = Vec::new();
        for sha in chunks {
            files.extend(store.load_files(id, sha)?);
        }
        remove_files(Path::new(root), &files, &backup)?;
    }
    store.forget_package(id)
}
