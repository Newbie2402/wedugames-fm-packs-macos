//! Publishes one pack from the command line, with FM Uploader's saved settings.
//!
//! `fmup-publish --check` lists what is live.
//! `fmup-publish --audit` measures the bucket: live files, files no pack uses, unfinished uploads.
//! `fmup-publish --abort-unfinished` deletes unfinished uploads over 6 hours old.
//! `fmup-publish --updates` checks each creator site (config/update-check.json) for a
//! newer version than we last published. Admin/seller only; touches no R2.
//! `fmup-publish --mark-seen <id> <version>` records that a version is published, so
//! `--updates` stops flagging it.
//! `fmup-publish --id <pack> [--src <folder>] --today 2026.10.31` publishes a
//! pack, optionally from a copy of its archives instead of its saved folder.

use anyhow::{bail, Context, Result};
use fmup_core::PUBLIC_KEY_HEX;
use packer::{load_key, public_key_hex};
use publisher::archive::find_seven_zip;
use publisher::r2::R2;
use publisher::{fetch_manifest, publish, unpublish, PackSpec, Progress, ProgressFn};
use aws_smithy_types::DateTime;
use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    account_id: String,
    access_key: String,
    secret_key: String,
    bucket: String,
    key_path: String,
    work_dir: String,
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|at| args.get(at + 1)).cloned()
}

/// One creator source `--updates` watches, from config/update-check.json.
#[derive(Deserialize)]
struct CheckEntry {
    id: String,
    label: String,
    url: String,
    pattern: String,
    seen: String,
}

#[derive(Deserialize)]
struct UpdateConfig {
    packs: Vec<CheckEntry>,
}

fn update_config_path(args: &[String]) -> PathBuf {
    option(args, "--config").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("config/update-check.json"))
}

/// Downloads a creator page as a browser would, so bot filters let it through.
fn fetch_page(url: &str) -> Result<String> {
    let agent = ureq::builder().timeout(Duration::from_secs(30)).build();
    let body = agent
        .get(url)
        .set(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        )
        .set("Accept-Encoding", "identity")
        .call()
        .with_context(|| format!("gagal buka {url}"))?
        .into_string()
        .context("respons tidak bisa dibaca")?;
    Ok(body)
}

/// Pulls every version the pattern finds and returns the newest, or None.
/// The version is the capture group named `v`, else the first group.
fn latest_version(page: &str, pattern: &str) -> Result<Option<String>> {
    let re = Regex::new(pattern).with_context(|| format!("pola tidak valid: {pattern}"))?;
    let mut best: Option<String> = None;
    for caps in re.captures_iter(page) {
        let found = caps.name("v").or_else(|| caps.get(1)).map(|m| m.as_str().to_string());
        if let Some(v) = found {
            if best.as_ref().map(|b| newer(&v, b)).unwrap_or(true) {
                best = Some(v);
            }
        }
    }
    Ok(best)
}

/// True when `a` is a strictly newer version than `b`. All-digit versions
/// compare as numbers (so 124 > 99), everything else as text (2027.02 > 2027.00).
fn newer(a: &str, b: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if digits(a) && digits(b) {
        a.parse::<u64>().unwrap_or(0) > b.parse::<u64>().unwrap_or(0)
    } else {
        a > b
    }
}

fn reporter() -> ProgressFn {
    let last = Mutex::new(String::new());
    Arc::new(move |event: Progress| {
        let (phase, ratio) = match &event {
            Progress::Extracting { archive, done, total, .. } => (format!("extract {archive}"), *done as f64 / *total as f64),
            Progress::Checking { done, total, .. } => ("check".to_string(), *done as f64 / (*total).max(1) as f64),
            Progress::Packing { done, total, .. } => ("zip".to_string(), *done as f64 / (*total).max(1) as f64),
            Progress::Uploading { done, total } => ("upload".to_string(), *done as f64 / (*total).max(1) as f64),
            Progress::Publishing => ("publish".to_string(), 1.0),
        };
        let line = format!("{phase} {}%", (ratio * 10.0).floor() * 10.0);
        let mut last = last.lock().unwrap();
        if *last != line {
            println!("{line}");
            *last = line;
        }
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Checking creator sites and marking versions needs neither R2 nor the
    // signing key, so handle them before loading any of that.
    if args.iter().any(|a| a == "--updates") {
        let path = update_config_path(&args);
        let config: UpdateConfig = serde_json::from_slice(&std::fs::read(&path).with_context(|| format!("tidak bisa baca {}", path.display()))?)?;
        println!("mengecek {} sumber...", config.packs.len());
        let mut behind = 0;
        for entry in &config.packs {
            match fetch_page(&entry.url).and_then(|page| latest_version(&page, &entry.pattern)) {
                Ok(Some(latest)) if newer(&latest, &entry.seen) => {
                    behind += 1;
                    println!("  [BARU] {:<28} punya v{} (live v{})  -> --mark-seen {} {}", entry.label, latest, entry.seen, entry.id, latest);
                }
                Ok(Some(latest)) => println!("  [ok]   {:<28} v{}", entry.label, latest),
                Ok(None) => println!("  [?]    {:<28} versi tidak ketemu di halaman", entry.label),
                Err(e) => println!("  [err]  {:<28} {}", entry.label, e),
            }
        }
        println!("{behind} pack ada update baru");
        return Ok(());
    }

    if let Some(at) = args.iter().position(|a| a == "--mark-seen") {
        let id = args.get(at + 1).context("--mark-seen butuh <id>")?;
        let version = args.get(at + 2).context("--mark-seen butuh <version>")?;
        let path = update_config_path(&args);
        let mut doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let packs = doc.get_mut("packs").and_then(|p| p.as_array_mut()).context("config tanpa daftar packs")?;
        let entry = packs
            .iter_mut()
            .find(|p| p.get("id").and_then(|i| i.as_str()) == Some(id.as_str()))
            .with_context(|| format!("pack {id} tidak ada di config"))?;
        entry["seen"] = serde_json::Value::String(version.clone());
        std::fs::write(&path, serde_json::to_vec_pretty(&doc)?)?;
        println!("{id} ditandai sudah di versi {version}");
        return Ok(());
    }

    let root = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).context("LOCALAPPDATA kosong")?.join("FMUploader");
    let settings: Settings = serde_json::from_slice(&std::fs::read(root.join("settings.json"))?)?;

    let key = load_key(Path::new(settings.key_path.trim()))?;
    if public_key_hex(&key) != PUBLIC_KEY_HEX.trim() {
        bail!("signing.key ini bukan pasangan aplikasi customer");
    }
    let bucket = if settings.bucket.trim().is_empty() { "fm-updater" } else { settings.bucket.trim() };
    let r2 = R2::cloudflare(&settings.account_id, &settings.access_key, &settings.secret_key, bucket);

    if args.iter().any(|a| a == "--check") {
        let manifest = fetch_manifest(&r2, &public_key_hex(&key)).await?;
        println!("connected, {} packs live", manifest.packages.len());
        for package in &manifest.packages {
            let bytes: u64 = package.chunks.iter().map(|c| c.size).sum();
            println!("  {} v{} {:.2} GB, {} chunks", package.id, package.version, bytes as f64 / 1e9, package.chunks.len());
        }
        return Ok(());
    }

    if args.iter().any(|a| a == "--audit") {
        let manifest = fetch_manifest(&r2, &public_key_hex(&key)).await?;
        let mut owner = HashMap::new();
        for package in &manifest.packages {
            for chunk in &package.chunks {
                owner.insert(chunk.key.clone(), package.id.as_str());
            }
            if let Some(cover) = &package.cover {
                owner.insert(cover.key.clone(), package.id.as_str());
            }
        }
        let gb = |bytes: u64| bytes as f64 / 1e9;
        let (mut live, mut other, mut unused) = (0, 0, Vec::new());
        for object in r2.list("").await? {
            if owner.contains_key(&object.key) {
                live += object.size;
            } else if object.key.starts_with("packs/") || object.key.starts_with("covers/") {
                unused.push(object);
            } else {
                other += object.size;
            }
        }
        println!("live files: {:.2} GB", gb(live));
        println!("manifest and index: {:.4} GB", gb(other));
        println!("files no pack uses: {} files, {:.2} GB", unused.len(), gb(unused.iter().map(|o| o.size).sum()));
        for object in &unused {
            println!("  {} {:.2} GB modified {}", object.key, gb(object.size), DateTime::from_secs(object.modified));
        }
        let uploads = r2.open_uploads().await?;
        println!("unfinished uploads: {}, {:.2} GB", uploads.len(), gb(uploads.iter().map(|u| u.size).sum()));
        for upload in &uploads {
            println!("  {} {:.2} GB started {}", upload.key, gb(upload.size), DateTime::from_secs(upload.started));
        }
        return Ok(());
    }

    if args.iter().any(|a| a == "--abort-unfinished") {
        // Longer than any upload takes, so one running right now on another PC is left alone.
        let cutoff = DateTime::from(std::time::SystemTime::now()).secs() - 6 * 60 * 60;
        for upload in r2.open_uploads().await? {
            if upload.started < cutoff {
                r2.abort_upload(&upload.key, &upload.upload_id).await?;
                println!("aborted {} {:.2} GB", upload.key, upload.size as f64 / 1e9);
            }
        }
        return Ok(());
    }

    if let Some(id) = option(&args, "--unpublish") {
        unpublish(&r2, &key, &id).await?;
        println!("unpublished {id}");
        return Ok(());
    }

    let id = option(&args, "--id").context("--id belum diisi")?;
    let today = option(&args, "--today").context("--today belum diisi")?;
    let packs: Vec<PackSpec> = serde_json::from_slice(&std::fs::read(root.join("packs.json"))?)?;
    let mut spec = packs.into_iter().find(|p| p.id == id).with_context(|| format!("pack {id} tidak terdaftar"))?;
    if let Some(src) = option(&args, "--src") {
        spec.src = PathBuf::from(src);
    }
    let work = match settings.work_dir.trim() {
        "" => root.join("work"),
        dir => PathBuf::from(dir).join("FMUploaderWork"),
    };
    let report = publish(
        &r2,
        &key,
        &work,
        &[spec],
        &today,
        find_seven_zip().as_deref(),
        reporter(),
        Arc::new(AtomicBool::new(false)),
    )
    .await?;
    println!(
        "done: published {:?}, unchanged {:?}, uploaded {:.2} GB, skipped {} files",
        report.published,
        report.unchanged,
        report.uploaded_bytes as f64 / 1e9,
        report.skipped_files
    );
    Ok(())
}
