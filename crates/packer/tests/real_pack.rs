//! Runs the direct zip copy on a real pack and installs the result like a
//! customer would, then compares it with the source archives.
//!
//! `cargo test -p packer --release --test real_pack -- --ignored --nocapture`
//! with FMUP_PACK set to a folder of zips and FMUP_WORK to a scratch folder.

use fmup_core::install::apply_chunk;
use fmup_core::manifest::Target;
use packer::{build_package, plan_zips, PackOptions, Stage};
use std::collections::HashMap;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[test]
#[ignore]
fn direct_copy_of_a_real_pack() {
    let pack = PathBuf::from(std::env::var("FMUP_PACK").expect("FMUP_PACK"));
    let work = PathBuf::from(std::env::var("FMUP_WORK").expect("FMUP_WORK"));
    let mut paths: Vec<PathBuf> =
        std::fs::read_dir(&pack).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_file()).collect();
    paths.sort();

    let clock = Instant::now();
    let plan = plan_zips(&paths, false).unwrap().expect("zips must be copyable directly");
    println!("planned {} archives in {:?}", paths.len(), clock.elapsed());

    let clock = Instant::now();
    let mut files = 0;
    let output = build_package(
        &PackOptions {
            src: &pack,
            id: "real-pack",
            name: "Real pack",
            version: "2026.11",
            target: Target::Graphics,
            min_db: None,
            group: None,
            cover: None,
            buckets: None,
            previous: None,
            index: None,
            overlay: true,
            prefix: Some("logos\\Real pack"),
            direct: Some(&plan),
            out: &work,
        },
        &AtomicBool::new(false),
        &mut |stage, done, _| {
            if stage == Stage::Zipping {
                files = done;
            }
        },
    )
    .unwrap();
    let bytes: u64 = output.package.chunks.iter().map(|c| c.size).sum();
    println!(
        "copied {files} files into {} chunks, {:.2} GB, in {:?}",
        output.package.chunks.len(),
        bytes as f64 / 1e9,
        clock.elapsed()
    );

    // Installing checks every file's CRC as it decompresses.
    let clock = Instant::now();
    let root = work.join("installed");
    let dir = work.join("objects/packs/real-pack");
    let mut installed = 0;
    for chunk in &output.package.chunks {
        let zip = dir.join(format!("{}.zip", chunk.sha256));
        let entries =
            apply_chunk(&zip, &root, &HashMap::new(), &work.join("backup"), &AtomicBool::new(false), || {}).unwrap();
        installed += entries.len();
    }
    println!("installed {installed} files with CRC checks in {:?}", clock.elapsed());
    assert_eq!(installed as u64, files);

    // Spot check: the newest archive's files must have won.
    let newest = paths.last().unwrap();
    let mut zip = zip::ZipArchive::new(BufReader::new(std::fs::File::open(newest).unwrap())).unwrap();
    let mut checked = 0;
    for i in 0..zip.len() {
        let item = zip.by_index_raw(i).unwrap();
        if item.is_dir() {
            continue;
        }
        let name = item.name().replace('\\', "/");
        let (_, rest) = name.split_once('/').unwrap();
        let installed = root.join("logos/Real pack").join(rest);
        assert_eq!(installed.metadata().unwrap().len(), item.size(), "{rest}");
        assert_eq!(fmup_core::install::crc_of(&installed).unwrap(), item.crc32(), "{rest}");
        checked += 1;
    }
    println!("newest archive: {checked} files match");
}
