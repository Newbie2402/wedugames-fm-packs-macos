//! Turns a source folder into content-addressed chunks plus a signed manifest.
//!
//! The first upload of a pack becomes base chunks. After that, files that are
//! new or changed since the last upload go into patch chunks on top of them, so
//! customers download the size of the update and not the whole pack again.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signer, SigningKey};
use fmup_core::manifest::{Chunk, Cover, Manifest, Package, SignedManifest, Target, COVER_EXTENSIONS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime};

const TARGET_CHUNK_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COVER_BYTES: usize = 2 * 1024 * 1024;

pub struct PackOptions<'a> {
    pub src: &'a Path,
    pub id: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub target: Target,
    pub min_db: Option<u32>,
    pub group: Option<&'a str>,
    pub cover: Option<&'a Path>,
    /// Overrides the bucket count. Changing it moves files between chunks, so
    /// customers download the whole package once.
    pub buckets: Option<u64>,
    /// The published version. With `index` it makes this upload a patch;
    /// its bucket count and cover carry over either way.
    pub previous: Option<&'a Package>,
    pub index: Option<&'a FileIndex>,
    /// `src` holds only this update's files, so what it lacks is not missing.
    pub overlay: bool,
    /// Folder inside the install root the files go to, like `logos/FMG Steel`.
    pub prefix: Option<&'a str>,
    /// Build from these zips directly, ignoring `src`.
    pub direct: Option<&'a ZipPlan>,
    pub out: &'a Path,
}

/// What the pack's files looked like at the last upload.
pub type FileIndex = BTreeMap<String, IndexEntry>;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub size: u64,
    /// CRC-32, which zips already store, so comparing needs no decompression.
    pub crc: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Checking,
    Zipping,
}

pub struct PackOutput {
    pub package: Package,
    pub index: FileIndex,
    /// Files published earlier that are gone from the source folder. Customers
    /// keep them, since patches only add and replace.
    pub missing_files: u64,
}

type SourceFile = (String, PathBuf, u64);

pub fn load_key(path: &Path) -> Result<SigningKey> {
    let text = fs::read_to_string(path).with_context(|| format!("signing key tidak ada: {}", path.display()))?;
    let bytes: [u8; 32] = hex::decode(text.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("panjang signing key salah"))?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub fn public_key_hex(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

pub fn sign_manifest(manifest: &Manifest, key: &SigningKey) -> Result<SignedManifest> {
    manifest.validate()?;
    let payload = serde_json::to_string(manifest)?;
    let signature = hex::encode(key.sign(payload.as_bytes()).to_bytes());
    Ok(SignedManifest { payload, signature })
}

fn collect_files(src: &Path, prefix: Option<&str>) -> Result<Vec<SourceFile>> {
    let prefix = prefix.map(|p| p.replace('\\', "/")).map(|p| p.trim_matches('/').to_string());
    let prefix = prefix.filter(|p| !p.is_empty()).map(|p| format!("{p}/")).unwrap_or_default();
    let mut files = Vec::new();
    for entry in WalkDir::new(src).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(src)?;
        let name = rel
            .to_str()
            .with_context(|| format!("nama file bukan UTF-8: {}", rel.display()))?
            .replace('\\', "/");
        files.push((format!("{prefix}{name}"), entry.path().to_path_buf(), entry.metadata()?.len()));
    }
    files.sort();
    Ok(files)
}

fn bucket(name: &str, buckets: u64) -> u64 {
    // Lowercased because Windows paths are case-insensitive.
    let hash = Sha256::digest(name.to_lowercase().as_bytes());
    u64::from_be_bytes(hash[..8].try_into().unwrap()) % buckets
}

fn compression_for(name: &str) -> CompressionMethod {
    let compressed = [".png", ".jpg", ".jpeg", ".dds", ".zip", ".fm"];
    if compressed.iter().any(|ext| name.to_lowercase().ends_with(ext)) {
        CompressionMethod::Stored
    } else {
        CompressionMethod::Deflated
    }
}

fn crc_of(path: &Path) -> Result<u32> {
    let mut hasher = crc32fast::Hasher::new();
    let mut reader = File::open(path)?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(hasher.finalize());
        }
        hasher.update(&buf[..n]);
    }
}

fn sha256_of(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut reader = File::open(path)?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&buf[..n]);
    }
}

/// `patch_version` marks a patch chunk. It goes in the zip comment so a patch
/// that restores earlier content still gets a key of its own, and customers
/// apply it instead of assuming they already have it.
fn write_chunk(
    files: &[&SourceFile],
    id: &str,
    out: &Path,
    patch_version: Option<&str>,
    cancel: &AtomicBool,
    on_file: &mut dyn FnMut(),
) -> Result<Chunk> {
    let dir = out.join("objects").join("packs").join(id);
    fs::create_dir_all(&dir)?;
    let tmp = dir.join("chunk.tmp");

    let mut zip = zip::ZipWriter::new(BufWriter::new(File::create(&tmp)?));
    for (name, path, size) in files {
        if cancel.load(Ordering::Relaxed) {
            bail!("Dibatalkan");
        }
        let options = SimpleFileOptions::default()
            .compression_method(compression_for(name))
            .last_modified_time(DateTime::default())
            .unix_permissions(0o644)
            .large_file(*size >= u32::MAX as u64);
        zip.start_file(name.as_str(), options)?;
        io::copy(&mut BufReader::new(File::open(path)?), &mut zip)?;
        on_file();
    }
    if let Some(version) = patch_version {
        zip.set_comment(format!("patch {version}"));
    }
    zip.finish()?;
    let unpacked = files.iter().map(|f| f.2).sum();
    seal_chunk(&tmp, &dir, id, files.len() as u64, unpacked, patch_version.is_some())
}

fn seal_chunk(tmp: &Path, dir: &Path, id: &str, files: u64, unpacked: u64, patch: bool) -> Result<Chunk> {
    let sha256 = sha256_of(tmp)?;
    let size = fs::metadata(tmp)?.len();
    fs::rename(tmp, dir.join(format!("{sha256}.zip")))?;
    Ok(Chunk { key: format!("packs/{id}/{sha256}.zip"), size, sha256, files, unpacked, patch })
}

#[cfg(test)]
mod contents_tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn write_zip(path: &Path, large: bool) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        let options = SimpleFileOptions::default().large_file(large);
        zip.add_directory("Pack 2026.01/", SimpleFileOptions::default()).unwrap();
        zip.start_file("Pack 2026.01/Clubs/1.png", options).unwrap();
        zip.write_all(b"hello").unwrap();
        zip.start_file("Pack 2026.01/Women/caf\u{e9}.png", options.compression_method(CompressionMethod::Stored)).unwrap();
        zip.write_all(b"unicode name").unwrap();
        zip.finish().unwrap();
    }

    fn check(large: bool) {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("a.zip");
        write_zip(&path, large);
        let entries = read_contents(&path).unwrap().unwrap();
        assert_eq!(entries.len(), 2);

        let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        for entry in &entries {
            let index = zip.index_for_name(&entry.name).unwrap();
            let file = zip.by_index_raw(index).unwrap();
            assert_eq!(file.size(), entry.size);
            assert_eq!(file.crc32(), entry.crc);
        }
    }

    #[test]
    fn matches_the_zip_library() {
        check(false);
    }

    #[test]
    fn reads_entries_marked_zip64() {
        check(true);
    }

    #[test]
    fn reads_an_archive_with_a_comment() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("a.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("a.png", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"x").unwrap();
        zip.set_comment("patch 2026.11");
        zip.finish().unwrap();
        assert_eq!(read_contents(&path).unwrap().unwrap().len(), 1);
    }

    #[test]
    fn leaves_out_names_windows_cannot_store() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("a.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        for name in ["W/Clubs/1.png", "W/Check Out - https-::example.com:kits.png", "W/CON.png", "W/folder./2.png"] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(b"x").unwrap();
        }
        zip.finish().unwrap();

        let plan = plan_zips(&[path], false).unwrap().unwrap();
        assert_eq!(plan.skipped, 3);
        assert_eq!(plan.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Clubs/1.png"]);
    }

    #[test]
    fn refuses_a_path_escaping_the_pack() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("a.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("../evil.png", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        assert!(read_contents(&path).unwrap().is_none());
    }
}

/// One file inside a source zip that makes it into the pack.
pub struct ZipFileRef {
    archive: usize,
    /// Position in the archive's table of contents.
    order: usize,
    /// Name inside the source zip.
    raw_name: String,
    /// Name in the pack, without the source's wrapper folder.
    name: String,
    size: u64,
    crc: u32,
}

/// Which file comes from which zip, once later versions have replaced earlier
/// ones. Worked out from the zips' tables of contents, without extracting.
pub struct ZipPlan {
    archives: Vec<PathBuf>,
    files: Vec<ZipFileRef>,
    /// Files left out because Windows can't store their names.
    pub skipped: u64,
}

/// Plans `paths`, oldest first. None when a zip uses something the customer
/// app can't read, so the caller extracts instead. `skip_root_text` drops
/// instruction files that would otherwise land in the game's folders.
pub fn plan_zips(paths: &[PathBuf], skip_root_text: bool) -> Result<Option<ZipPlan>> {
    let mut newest: BTreeMap<String, ZipFileRef> = BTreeMap::new();
    let mut skipped = 0;
    for (archive, path) in paths.iter().enumerate() {
        let Some(entries) = read_contents(path).with_context(|| format!("Arsip rusak: {}", path.display()))? else {
            return Ok(None);
        };
        // Most packs wrap everything in one folder named after the version.
        let first = entries.first().and_then(|e| e.name.split_once('/')).map(|(top, _)| format!("{top}/"));
        let wrapper = first.filter(|top| entries.iter().all(|e| e.name.starts_with(top.as_str())));
        for (order, item) in entries.into_iter().enumerate() {
            let name = match &wrapper {
                Some(top) => item.name[top.len()..].to_string(),
                None => item.name.clone(),
            };
            if skip_root_text && !name.contains('/') && name.to_lowercase().ends_with(".txt") {
                continue;
            }
            // Would stop the install on a customer's PC. So far these have only
            // been promo images, with a web address as their name.
            if !windows_safe(&name) {
                skipped += 1;
                continue;
            }
            // Lowercased: on Windows these are the same file.
            let reference = ZipFileRef { archive, order, raw_name: item.name, name: name.clone(), size: item.size, crc: item.crc };
            newest.insert(name.to_lowercase(), reference);
        }
    }
    let mut files: Vec<ZipFileRef> = newest.into_values().collect();
    // Archive order, so a hard disk reads each zip front to back.
    files.sort_by_key(|f| (f.archive, f.order));
    Ok(Some(ZipPlan { archives: paths.to_vec(), files, skipped }))
}

fn windows_safe(path: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1",
        "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    path.split('/').all(|part| {
        let stem = part.split('.').next().unwrap_or_default().to_lowercase();
        !part.is_empty()
            && !part.ends_with([' ', '.'])
            && !part.chars().any(|c| c.is_control() || "<>:\"|?*\\".contains(c))
            && !RESERVED.contains(&stem.as_str())
    })
}

struct ContentsEntry {
    name: String,
    size: u64,
    crc: u32,
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    let raw = bytes.get(at..at + 2).context("arsip terpotong")?;
    Ok(u16::from_le_bytes(raw.try_into()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    let raw = bytes.get(at..at + 4).context("arsip terpotong")?;
    Ok(u32::from_le_bytes(raw.try_into()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Result<u64> {
    let raw = bytes.get(at..at + 8).context("arsip terpotong")?;
    Ok(u64::from_le_bytes(raw.try_into()?))
}

fn read_at(file: &mut File, offset: u64, length: usize) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0; length];
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

/// Offset and size of the table of contents at the end of a zip.
fn locate_contents(file: &mut File) -> Result<(u64, u64)> {
    const END: u32 = 0x0605_4b50;
    const ZIP64_LOCATOR: u32 = 0x0706_4b50;
    const ZIP64_END: u32 = 0x0606_4b50;
    let length = file.metadata()?.len();
    let tail_start = length.saturating_sub(66 * 1024);
    let tail = read_at(file, tail_start, (length - tail_start) as usize)?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&at| u32_at(&tail, at).is_ok_and(|sig| sig == END))
        .context("bukan arsip zip")?;

    let size = u32_at(&tail, end + 12)? as u64;
    let offset = u32_at(&tail, end + 16)? as u64;
    if size != u32::MAX as u64 && offset != u32::MAX as u64 && u16_at(&tail, end + 10)? != u16::MAX {
        return Ok((offset, size));
    }
    let locator = end.checked_sub(20).context("arsip terpotong")?;
    if u32_at(&tail, locator)? != ZIP64_LOCATOR {
        bail!("penanda zip64 tidak ditemukan");
    }
    let header = read_at(file, u64_at(&tail, locator + 8)?, 56)?;
    if u32_at(&header, 0)? != ZIP64_END {
        bail!("penanda zip64 rusak");
    }
    Ok((u64_at(&header, 48)?, u64_at(&header, 40)?))
}

/// The real size of an entry too big for the 32-bit field.
fn zip64_size(extra: &[u8]) -> Result<u64> {
    let mut at = 0;
    while at + 4 <= extra.len() {
        let (id, length) = (u16_at(extra, at)?, u16_at(extra, at + 2)? as usize);
        if id == 1 {
            return u64_at(extra, at + 4);
        }
        at += 4 + length;
    }
    bail!("ukuran zip64 tidak ditemukan")
}

/// Reads a zip's table of contents in one go. Going through the zip library
/// costs one disk seek per file, which on a hard disk takes minutes. None when
/// the zip has something the customer app can't handle.
fn read_contents(path: &Path) -> Result<Option<Vec<ContentsEntry>>> {
    const ENTRY: u32 = 0x0201_4b50;
    const FIXED: usize = 46;
    let mut file = File::open(path)?;
    let (offset, size) = locate_contents(&mut file)?;
    let table = read_at(&mut file, offset, size as usize)?;

    let mut entries = Vec::new();
    let mut at = 0;
    while at + FIXED <= table.len() && u32_at(&table, at)? == ENTRY {
        let flags = u16_at(&table, at + 8)?;
        let method = u16_at(&table, at + 10)?;
        let crc = u32_at(&table, at + 16)?;
        let small_size = u32_at(&table, at + 24)?;
        let name_length = u16_at(&table, at + 28)? as usize;
        let extra_length = u16_at(&table, at + 30)? as usize;
        let comment_length = u16_at(&table, at + 32)? as usize;
        let raw = table.get(at + FIXED..at + FIXED + name_length).context("arsip terpotong")?;
        let extra_start = at + FIXED + name_length;
        let extra = table.get(extra_start..extra_start + extra_length).context("arsip terpotong")?;
        at = extra_start + extra_length + comment_length;

        const ENCRYPTED: u16 = 1;
        const UTF8_NAME: u16 = 1 << 11;
        let Ok(name) = std::str::from_utf8(raw) else {
            return Ok(None);
        };
        // A non-ASCII name in the old encoding would be decoded differently.
        if flags & ENCRYPTED != 0 || (flags & UTF8_NAME == 0 && !raw.is_ascii()) {
            return Ok(None);
        }
        if name.ends_with('/') {
            continue;
        }
        if !matches!(method, 0 | 8) {
            return Ok(None);
        }
        let unsafe_path = name.starts_with('/') || name.contains('\\') || name.split('/').any(|part| part == "..");
        if unsafe_path {
            return Ok(None);
        }
        let size = if small_size == u32::MAX { zip64_size(extra)? } else { small_size as u64 };
        entries.push(ContentsEntry { name: name.to_string(), size, crc });
    }
    Ok(Some(entries))
}

fn prefixed(prefix: Option<&str>, name: &str) -> String {
    let prefix = prefix.map(|p| p.replace('\\', "/")).map(|p| p.trim_matches('/').to_string());
    match prefix.filter(|p| !p.is_empty()) {
        Some(prefix) => format!("{prefix}/{name}"),
        None => name.to_string(),
    }
}

/// Copies files between zips still compressed, so nothing is written to disk
/// one file at a time. That is where almost all extraction time goes.
fn write_direct_chunks(
    plan: &ZipPlan,
    files: &[&ZipFileRef],
    options: &PackOptions,
    patch_version: Option<&str>,
    cancel: &AtomicBool,
    on_file: &mut dyn FnMut(Stage, u64, u64),
) -> Result<Vec<Chunk>> {
    let mut sources = Vec::with_capacity(plan.archives.len());
    for path in &plan.archives {
        sources.push(zip::ZipArchive::new(BufReader::new(File::open(path)?))?);
    }
    let dir = options.out.join("objects").join("packs").join(options.id);
    fs::create_dir_all(&dir)?;
    let tmp = dir.join("chunk.tmp");

    let total = files.len() as u64;
    let mut chunks = Vec::new();
    let mut done = 0;
    for slice in slices_by_size(files, |f| f.size) {
        let mut zip = zip::ZipWriter::new(BufWriter::new(File::create(&tmp)?));
        for file in &slice {
            if cancel.load(Ordering::Relaxed) {
                bail!("Dibatalkan");
            }
            let archive = &mut sources[file.archive];
            let entry = archive.index_for_name(&file.raw_name).context("file hilang dari arsip")?;
            let source = archive.by_index_raw(entry)?;
            zip.raw_copy_file_rename(source, prefixed(options.prefix, &file.name))?;
            done += 1;
            on_file(Stage::Zipping, done, total);
        }
        if let Some(version) = patch_version {
            zip.set_comment(format!("patch {version}"));
        }
        zip.finish()?;
        let unpacked = slice.iter().map(|f| f.size).sum();
        chunks.push(seal_chunk(&tmp, &dir, options.id, slice.len() as u64, unpacked, patch_version.is_some())?);
    }
    Ok(chunks)
}

/// Splits `items`, in order, into runs of about one chunk each.
fn slices_by_size<'a, T>(items: &[&'a T], size: impl Fn(&T) -> u64) -> Vec<Vec<&'a T>> {
    let mut slices: Vec<Vec<&'a T>> = Vec::new();
    let mut bytes = 0;
    for item in items {
        if slices.is_empty() || bytes + size(item) > TARGET_CHUNK_BYTES {
            slices.push(Vec::new());
            bytes = 0;
        }
        slices.last_mut().unwrap().push(item);
        bytes += size(item);
    }
    slices
}

fn build_direct(
    plan: &ZipPlan,
    options: &PackOptions,
    cancel: &AtomicBool,
    on_file: &mut dyn FnMut(Stage, u64, u64),
) -> Result<PackOutput> {
    let current: FileIndex = plan
        .files
        .iter()
        .map(|f| (prefixed(options.prefix, &f.name), IndexEntry { size: f.size, crc: f.crc }))
        .collect();

    let earlier = options.previous.zip(options.index).filter(|(p, _)| !p.chunks.is_empty());
    let (chunks, index) = match earlier {
        Some((package, old)) => {
            let changed: Vec<&ZipFileRef> = plan
                .files
                .iter()
                .filter(|f| {
                    let name = prefixed(options.prefix, &f.name);
                    old.get(&name) != current.get(&name)
                })
                .collect();
            let mut chunks = package.chunks.clone();
            chunks.extend(write_direct_chunks(plan, &changed, options, Some(options.version), cancel, on_file)?);
            let mut index = old.clone();
            index.extend(current);
            (chunks, index)
        }
        None => {
            let all: Vec<&ZipFileRef> = plan.files.iter().collect();
            (write_direct_chunks(plan, &all, options, None, cancel, on_file)?, current)
        }
    };
    let buckets = chunks.len() as u64;
    Ok(PackOutput { package: assemble(options, chunks, buckets)?, index, missing_files: 0 })
}

fn assemble(options: &PackOptions, chunks: Vec<Chunk>, buckets: u64) -> Result<Package> {
    // Without a new image the pack keeps the one it already has.
    let cover = match options.cover {
        Some(path) => Some(write_cover(path, options.id, options.out)?),
        None => options.previous.and_then(|p| p.cover.clone()),
    };
    Ok(Package {
        id: options.id.to_string(),
        name: options.name.to_string(),
        version: options.version.to_string(),
        target: options.target,
        min_db: options.min_db,
        group: options.group.map(Into::into),
        cover,
        buckets,
        chunks,
    })
}

pub fn write_cover(path: &Path, id: &str, out: &Path) -> Result<Cover> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    if !COVER_EXTENSIONS.contains(&ext.as_str()) {
        bail!("cover harus png, jpg, atau webp: {}", path.display());
    }
    let bytes = fs::read(path).with_context(|| format!("cover tidak bisa dibaca: {}", path.display()))?;
    if bytes.len() > MAX_COVER_BYTES {
        bail!("cover lebih dari 2 MB: {}", path.display());
    }
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let dir = out.join("objects").join("covers").join(id);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(format!("{sha256}.{ext}")), &bytes)?;
    Ok(Cover { key: format!("covers/{id}/{sha256}.{ext}"), size: bytes.len() as u64, sha256 })
}

fn write_base(
    files: &[SourceFile],
    buckets: u64,
    options: &PackOptions,
    cancel: &AtomicBool,
    on_file: &mut dyn FnMut(Stage, u64, u64),
) -> Result<Vec<Chunk>> {
    let mut groups: Vec<Vec<&SourceFile>> = vec![Vec::new(); buckets as usize];
    for file in files {
        groups[bucket(&file.0, buckets) as usize].push(file);
    }
    let total = files.len() as u64;
    let mut done = 0;
    let mut chunks = Vec::new();
    for group in groups.iter().filter(|g| !g.is_empty()) {
        chunks.push(write_chunk(group, options.id, options.out, None, cancel, &mut || {
            done += 1;
            on_file(Stage::Zipping, done, total);
        })?);
    }
    Ok(chunks)
}

fn write_patch(
    changed: &[&SourceFile],
    options: &PackOptions,
    cancel: &AtomicBool,
    on_file: &mut dyn FnMut(Stage, u64, u64),
) -> Result<Vec<Chunk>> {
    let mut slices: Vec<Vec<&SourceFile>> = Vec::new();
    let mut slice_bytes = 0;
    for file in changed {
        if slices.is_empty() || slice_bytes + file.2 > TARGET_CHUNK_BYTES {
            slices.push(Vec::new());
            slice_bytes = 0;
        }
        slices.last_mut().unwrap().push(file);
        slice_bytes += file.2;
    }
    let total = changed.len() as u64;
    let mut done = 0;
    let mut chunks = Vec::new();
    for slice in &slices {
        chunks.push(write_chunk(slice, options.id, options.out, Some(options.version), cancel, &mut || {
            done += 1;
            on_file(Stage::Zipping, done, total);
        })?);
    }
    Ok(chunks)
}

/// Writes the chunks and cover under `out/objects` and describes them.
/// `on_file` receives (stage, files done, files total).
pub fn build_package(
    options: &PackOptions,
    cancel: &AtomicBool,
    on_file: &mut dyn FnMut(Stage, u64, u64),
) -> Result<PackOutput> {
    if let Some(plan) = options.direct {
        return build_direct(plan, options, cancel, on_file);
    }
    let files = collect_files(options.src, options.prefix)?;
    if files.is_empty() {
        bail!("{} kosong", options.src.display());
    }

    let file_count = files.len() as u64;
    let mut current = FileIndex::new();
    for (done, (name, path, size)) in files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("Dibatalkan");
        }
        current.insert(name.clone(), IndexEntry { size: *size, crc: crc_of(path)? });
        on_file(Stage::Checking, done as u64 + 1, file_count);
    }

    let previous = options.previous;
    let earlier = previous.zip(options.index).filter(|(p, _)| !p.chunks.is_empty());
    let (chunks, buckets, index, missing_files) = match earlier {
        Some((package, old)) => {
            let changed: Vec<&SourceFile> = files.iter().filter(|f| old.get(&f.0) != current.get(&f.0)).collect();
            let missing = match options.overlay {
                true => 0,
                false => old.keys().filter(|k| !current.contains_key(*k)).count() as u64,
            };
            let mut chunks = package.chunks.clone();
            chunks.extend(write_patch(&changed, options, cancel, on_file)?);
            // A file gone from the folder stays listed: customers still have it.
            let mut index = old.clone();
            index.extend(current);
            (chunks, package.buckets, index, missing)
        }
        None => {
            let total: u64 = files.iter().map(|f| f.2).sum();
            let buckets = options
                .buckets
                .or(previous.map(|p| p.buckets).filter(|n| *n > 0))
                .unwrap_or_else(|| total.div_ceil(TARGET_CHUNK_BYTES).max(1));
            (write_base(&files, buckets, options, cancel, on_file)?, buckets, current, 0)
        }
    };

    Ok(PackOutput { package: assemble(options, chunks, buckets)?, index, missing_files })
}
