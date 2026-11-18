use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Target {
    /// `Documents\Sports Interactive\Football Manager 27\graphics`
    Graphics,
    /// `<game>\shared\data\database\db\<version>` for every version >= `min_db`
    Database,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    pub key: String,
    pub size: u64,
    pub sha256: String,
    pub files: u64,
    pub unpacked: u64,
    /// Holds newer versions of files from earlier chunks, so it must be applied
    /// after them. Base chunks never share a file and can go in any order.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub patch: bool,
}

/// Card image shown in the app.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Cover {
    pub key: String,
    pub size: u64,
    pub sha256: String,
}

pub const COVER_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    pub target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_db: Option<u32>,
    /// Packs in one group are alternatives (e.g. `faces`, `logos`, `kits-3d`):
    /// only one can be installed, since they fill the same folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<Cover>,
    /// Hash buckets files are split into. Can exceed `chunks.len()`, since an
    /// empty bucket produces no chunk, and must stay stable across versions.
    #[serde(default)]
    pub buckets: u64,
    pub chunks: Vec<Chunk>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema: u32,
    pub packages: Vec<Package>,
}

/// What is stored in R2. `payload` is signed as raw bytes so the signature does
/// not depend on JSON formatting.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignedManifest {
    pub payload: String,
    pub signature: String,
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        if self.schema > SCHEMA {
            bail!("Versi tool ini terlalu lama, silakan download versi terbaru");
        }
        for p in &self.packages {
            let id_ok = !p.id.is_empty()
                && p.id.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'));
            if !id_ok {
                bail!("id paket tidak valid: {}", p.id);
            }
            if p.target == Target::Database && p.min_db.is_none() {
                bail!("paket database {} wajib punya minDb", p.id);
            }
            if let Some(cover) = &p.cover {
                let valid = is_hex(&cover.sha256, 64)
                    && COVER_EXTENSIONS
                        .iter()
                        .any(|ext| cover.key == format!("covers/{}/{}.{ext}", p.id, cover.sha256));
                if !valid {
                    bail!("cover tidak valid di paket {}", p.id);
                }
            }
            for c in &p.chunks {
                if !is_hex(&c.sha256, 64) || c.key != format!("packs/{}/{}.zip", p.id, c.sha256) {
                    bail!("chunk tidak valid di paket {}", p.id);
                }
            }
        }
        Ok(())
    }
}

impl SignedManifest {
    pub fn verify(&self, public_key_hex: &str) -> Result<Manifest> {
        let key: [u8; 32] = hex::decode(public_key_hex.trim())
            .context("public key rusak")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("panjang public key salah"))?;
        let key = VerifyingKey::from_bytes(&key).context("public key rusak")?;
        let sig: [u8; 64] = hex::decode(self.signature.trim())
            .context("signature rusak")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("panjang signature salah"))?;
        key.verify(self.payload.as_bytes(), &Signature::from_bytes(&sig))
            .context("Signature manifest tidak valid, update ditolak")?;
        let manifest: Manifest = serde_json::from_str(&self.payload).context("manifest rusak")?;
        manifest.validate()?;
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed(payload: &str) -> (SignedManifest, String) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let signature = hex::encode(key.sign(payload.as_bytes()).to_bytes());
        let public = hex::encode(key.verifying_key().to_bytes());
        (SignedManifest { payload: payload.into(), signature }, public)
    }

    #[test]
    fn accepts_valid_signature() {
        let (m, public) = signed(r#"{"schema":1,"packages":[]}"#);
        assert!(m.verify(&public).is_ok());
    }

    #[test]
    fn rejects_tampered_payload() {
        let (mut m, public) = signed(r#"{"schema":1,"packages":[]}"#);
        m.payload = r#"{"schema":1,"packages":[{}]}"#.into();
        assert!(m.verify(&public).is_err());
    }

    #[test]
    fn rejects_path_like_package_id() {
        let p = r#"{"schema":1,"packages":[{"id":"../x","name":"x","version":"1","target":"graphics","chunks":[]}]}"#;
        let (m, public) = signed(p);
        assert!(m.verify(&public).is_err());
    }
}
