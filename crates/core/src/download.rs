use crate::manifest::{Chunk, Cover, Manifest, SignedManifest};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::{header, StatusCode};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const RETRIES: u32 = 4;

#[derive(Clone)]
pub struct Client {
    base: String,
    /// Read on every request: the app renews it while a long download runs.
    license: Arc<RwLock<String>>,
    http: reqwest::Client,
}

/// Stops the retry loop: trying again cannot fix these.
#[derive(Debug)]
struct Fatal(String);

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fatal {}

fn auth_error(status: StatusCode) -> Option<Fatal> {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Some(Fatal("License key tidak valid atau sudah habis masa aktifnya".into()))
        }
        _ => None,
    }
}

fn sha256_of(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&buf[..n]);
    }
}

impl Client {
    pub fn new(base: &str, license: &str) -> Result<Self> {
        Self::shared(base, Arc::new(RwLock::new(license.trim().to_string())))
    }

    /// A client whose license token someone else keeps up to date.
    pub fn shared(base: &str, license: Arc<RwLock<String>>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { base: base.trim().trim_end_matches('/').to_string(), license, http })
    }

    fn token(&self) -> String {
        self.license.read().unwrap().clone()
    }

    pub async fn manifest(&self, public_key_hex: &str) -> Result<Manifest> {
        let response = self
            .http
            .get(format!("{}/manifest", self.base))
            .bearer_auth(self.token())
            .send()
            .await
            .context("Tidak bisa menghubungi server update")?;
        if let Some(e) = auth_error(response.status()) {
            bail!(e);
        }
        let signed: SignedManifest = response
            .error_for_status()
            .context("Server update error")?
            .json()
            .await
            .context("Respon server tidak valid")?;
        signed.verify(public_key_hex)
    }

    /// Returns the cover's path in `dir`, downloading it the first time.
    pub async fn fetch_cover(&self, cover: &Cover, dir: &Path) -> Result<PathBuf> {
        let name = cover.key.rsplit('/').next().unwrap_or(&cover.sha256);
        let dest = dir.join(name);
        if dest.exists() {
            return Ok(dest);
        }
        let response = self
            .http
            .get(format!("{}/objects/{}", self.base, cover.key))
            .bearer_auth(self.token())
            .send()
            .await?;
        if let Some(e) = auth_error(response.status()) {
            bail!(e);
        }
        let bytes = response.error_for_status()?.bytes().await?;
        if hex::encode(Sha256::digest(&bytes)) != cover.sha256 {
            bail!("Checksum cover tidak cocok: {}", cover.key);
        }
        tokio::fs::create_dir_all(dir).await?;
        tokio::fs::write(&dest, &bytes).await?;
        Ok(dest)
    }

    /// Downloads to `<sha>.zip` in `dir`, resuming any earlier `.part`.
    pub async fn fetch_chunk(
        &self,
        chunk: &Chunk,
        dir: &Path,
        done: &AtomicU64,
        cancel: &AtomicBool,
    ) -> Result<PathBuf> {
        tokio::fs::create_dir_all(dir).await?;
        let dest = dir.join(format!("{}.zip", chunk.sha256));
        let part = dir.join(format!("{}.part", chunk.sha256));

        if dest.exists() {
            let (path, want) = (dest.clone(), chunk.sha256.clone());
            if tokio::task::spawn_blocking(move || sha256_of(&path)).await?? == want {
                done.fetch_add(chunk.size, Ordering::Relaxed);
                return Ok(dest);
            }
            tokio::fs::remove_file(&dest).await?;
        }

        let mut last_error = anyhow!("download gagal");
        for attempt in 0..RETRIES {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
            }
            // Only the bytes received in this attempt count, so a failed one
            // can be taken back out of the shared progress total.
            let counted = AtomicU64::new(0);
            let result = self.try_fetch(chunk, &part, done, &counted, cancel).await;
            match result {
                Ok(()) => {
                    let (path, want) = (part.clone(), chunk.sha256.clone());
                    if tokio::task::spawn_blocking(move || sha256_of(&path)).await?? == want {
                        tokio::fs::rename(&part, &dest).await?;
                        return Ok(dest);
                    }
                    tokio::fs::remove_file(&part).await?;
                    done.fetch_sub(chunk.size, Ordering::Relaxed);
                    last_error = anyhow!("Checksum tidak cocok untuk {}", chunk.key);
                }
                Err(e) if e.downcast_ref::<Fatal>().is_some() => return Err(e),
                Err(e) => {
                    done.fetch_sub(counted.load(Ordering::Relaxed), Ordering::Relaxed);
                    last_error = e;
                }
            }
        }
        Err(last_error)
    }

    async fn try_fetch(
        &self,
        chunk: &Chunk,
        part: &Path,
        done: &AtomicU64,
        counted: &AtomicU64,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let have = tokio::fs::metadata(part).await.map(|m| m.len()).unwrap_or(0);
        let mut request = self
            .http
            .get(format!("{}/objects/{}", self.base, chunk.key))
            .bearer_auth(self.token());
        if have > 0 {
            request = request.header(header::RANGE, format!("bytes={have}-"));
        }
        let response = request.send().await?;
        if let Some(e) = auth_error(response.status()) {
            return Err(e.into());
        }
        if response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
            tokio::fs::remove_file(part).await?;
            bail!("resume ditolak server");
        }
        let response = response.error_for_status()?;
        let resumed = response.status() == StatusCode::PARTIAL_CONTENT;

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(part)
            .await?;
        if resumed {
            done.fetch_add(have, Ordering::Relaxed);
            counted.fetch_add(have, Ordering::Relaxed);
        }
        let mut stream = response.bytes_stream();
        while let Some(bytes) = stream.next().await {
            if cancel.load(Ordering::Relaxed) {
                file.flush().await?;
                return Err(Fatal("Dibatalkan".into()).into());
            }
            let bytes = bytes?;
            file.write_all(&bytes).await?;
            done.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            counted.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
        file.flush().await?;
        Ok(())
    }
}
