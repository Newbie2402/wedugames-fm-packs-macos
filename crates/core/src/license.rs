//! Activation against the WeduGames license Worker, the same one RTE4FM 27 uses (License.cs in FMRWG-Editor).
//! The WeduGames keygen writes the accounts; the Worker answers with a ticket signed for this computer. Unlike
//! RTE4FM there is no offline grace: the app asks the server every time it opens, and renews the ticket while it
//! runs, because the pack Worker takes a ticket for 24 hours only. The saved ticket just remembers the key.
//! Only the public key is in here.
//! Ticket text, as the Worker signs it: FMPACKS27|2|<hwid>|<expire or empty>|<key>|<issued yyyy-mm-ddThh:mm:ssZ>

use crate::state::Store;
use anyhow::{Context, Result};
use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const PRODUCT: &str = "fmpacks27";
const TAG: &str = "FMPACKS27";
const LICENSE_URL: &str = "https://rte4fm-license.wedugamez.workers.dev";
// P-256 public key of LICENSE_PRIVATE_KEY in the license Worker, as in License.cs.
const PUBLIC_X: &str = "T7if60C6qOsfo8Kva71V6skY3+Yon+ikBJVJ00pkqDE=";
const PUBLIC_Y: &str = "7g4MCAHWjhJiVPjG9wpV2fA09QZJDrnF0zZ5JHUG8r0=";
pub const UNREACHABLE: &str = "Tidak bisa menghubungi server license. Cek koneksi internet lalu coba lagi.";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Ticket {
    pub key: String,
    #[serde(default)]
    pub name: Option<String>,
    /// yyyy-mm-dd, empty for lifetime.
    #[serde(default)]
    pub expire: String,
    pub issued: String,
    pub sig: String,
}

/// What the license server said, for the register window.
#[derive(Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    /// False: the server could not be asked.
    pub reached: bool,
    pub ok: bool,
    pub code: String,
    pub message: String,
    pub can_transfer: bool,
    pub cooldown_days: u32,
}

/// Motherboard UUID, else the Windows MachineGuid; SHA-256, first 16 hex digits.
/// Same id as License.cs, so one WeduGames account counts this PC once across apps.
pub fn hwid() -> &'static str {
    static HWID: OnceLock<String> = OnceLock::new();
    HWID.get_or_init(|| {
        let raw = machine_uuid().filter(|u| u.trim().len() > 10).or_else(machine_guid).unwrap_or_default();
        hex::encode_upper(Sha256::digest(raw.trim().as_bytes()))[..16].to_string()
    })
}

#[cfg(windows)]
fn machine_uuid() -> Option<String> {
    #[derive(Deserialize)]
    #[serde(rename = "Win32_ComputerSystemProduct")]
    struct Product {
        #[serde(rename = "UUID")]
        uuid: Option<String>,
    }
    // Its own thread: the window's thread already set up COM in a mode WMI cannot share.
    std::thread::spawn(|| {
        let wmi = wmi::WMIConnection::new(wmi::COMLibrary::new().ok()?).ok()?;
        let rows: Vec<Product> = wmi.query().ok()?;
        rows.into_iter().next()?.uuid
    })
    .join()
    .ok()
    .flatten()
}

#[cfg(windows)]
fn machine_guid() -> Option<String> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY};
    winreg::RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(r"SOFTWARE\Microsoft\Cryptography", KEY_READ | KEY_WOW64_64KEY)
        .ok()?
        .get_value("MachineGuid")
        .ok()
}

#[cfg(target_os = "macos")]
fn machine_uuid() -> Option<String> {
    // macOS has no /etc/machine-id (that file is Linux-only). Read the hardware
    // UUID from IORegistry — stable across reinstalls, like the WMI UUID on
    // Windows — and fall back to the (deprecated but present) kern.uuid sysctl.
    let ioreg = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&ioreg.stdout);
    let uuid = text.lines().find_map(|line| {
        if !line.contains("IOPlatformUUID") {
            return None;
        }
        line.split('"').nth(3).map(str::trim).filter(|u| !u.is_empty()).map(str::to_string)
    });
    if let Some(uuid) = uuid {
        return Some(uuid);
    }
    let sysctl = std::process::Command::new("sysctl").args(["-n", "kern.uuid"]).output().ok()?;
    let out = String::from_utf8_lossy(&sysctl.stdout).trim().to_string();
    (!out.is_empty()).then_some(out)
}

#[cfg(target_os = "macos")]
fn machine_guid() -> Option<String> {
    None
}

#[cfg(not(any(windows, target_os = "macos")))]
fn machine_uuid() -> Option<String> {
    None
}

#[cfg(not(any(windows, target_os = "macos")))]
fn machine_guid() -> Option<String> {
    std::fs::read_to_string("/etc/machine-id").ok()
}

/// Days since 1970-01-01 for a yyyy-mm-dd date.
fn days_of(date: &str) -> Option<i64> {
    let mut parts = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    Some(era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468)
}

fn today() -> i64 {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    (secs / 86_400) as i64
}

fn public_key() -> VerifyingKey {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut point = vec![4u8];
    point.extend(b64.decode(PUBLIC_X).expect("public key"));
    point.extend(b64.decode(PUBLIC_Y).expect("public key"));
    VerifyingKey::from_sec1_bytes(&point).expect("public key")
}

impl Ticket {
    fn text(&self, hwid: &str) -> String {
        format!("{TAG}|2|{hwid}|{}|{}|{}", self.expire, self.key, self.issued)
    }

    fn signed_by(&self, key: &VerifyingKey, hwid: &str) -> bool {
        let Ok(sig) = base64::engine::general_purpose::STANDARD.decode(&self.sig) else {
            return false;
        };
        let Ok(sig) = Signature::from_slice(&sig) else {
            return false;
        };
        key.verify(self.text(hwid).as_bytes(), &sig).is_ok()
    }

    /// Signed for this computer and not expired; the expiry day itself is already over, as on the server.
    fn valid_on(&self, key: &VerifyingKey, hwid: &str, day: i64) -> bool {
        self.signed_by(key, hwid) && (self.expire.is_empty() || days_of(&self.expire).is_some_and(|end| day < end))
    }

    pub fn valid(&self) -> bool {
        self.valid_on(&public_key(), hwid(), today())
    }

    /// What the pack Worker gets as the Bearer token: the ticket plus the computer it was signed for.
    pub fn token(&self) -> String {
        let body = serde_json::json!({
            "hwid": hwid(), "key": self.key, "expire": self.expire, "issued": self.issued, "sig": self.sig,
        });
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body.to_string())
    }
}

fn file(store: &Store) -> std::path::PathBuf {
    store.root.join("license.json")
}

fn field(v: &serde_json::Value, k: &str) -> String {
    match &v[k] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

async fn post(path: &str, identifier: &str, timeout: Duration) -> (Reply, Option<Ticket>) {
    let mut reply = Reply::default();
    let body = serde_json::json!({ "identifier": identifier, "hwid": hwid(), "product": PRODUCT });
    let sent = async {
        let http = reqwest::Client::builder().timeout(timeout).build()?;
        http.post(format!("{LICENSE_URL}{path}"))
            .header(reqwest::header::USER_AGENT, concat!("WedugamesFMPacks/", env!("CARGO_PKG_VERSION")))
            .json(&body)
            .send()
            .await?
            .json::<serde_json::Value>()
            .await
    };
    let Ok(v) = sent.await else {
        return (reply, None);
    };
    reply.reached = true;
    reply.ok = v["ok"] == true;
    reply.code = field(&v, "code");
    reply.message = field(&v, "message");
    reply.can_transfer = v["can_transfer"] == true;
    reply.cooldown_days = v["cooldown_days"].as_u64().unwrap_or(0) as u32;
    if !reply.ok {
        return (reply, None);
    }
    // Never take the server's word alone: the signature decides.
    let ticket: Option<Ticket> = serde_json::from_value(v).ok();
    match ticket.filter(Ticket::valid) {
        Some(t) => (reply, Some(t)),
        None => {
            reply.ok = false;
            reply.code = "BAD_TICKET".into();
            (reply, None)
        }
    }
}

fn save(store: &Store, ticket: &Ticket) -> Result<()> {
    std::fs::create_dir_all(&store.root)?;
    std::fs::write(file(store), serde_json::to_vec_pretty(ticket)?).context("Gagal menyimpan license")
}

/// Register this computer with a License Key, WhatsApp or email. `transfer` moves the license here
/// when the device quota is full.
pub async fn activate(store: &Store, identifier: &str, transfer: bool) -> (Reply, Option<Ticket>) {
    let path = if transfer { "/transfer" } else { "/verify" };
    let (reply, ticket) = post(path, identifier.trim(), Duration::from_secs(15)).await;
    if let Some(t) = &ticket {
        if let Err(e) = save(store, t) {
            return (Reply { message: format!("{e:#}"), ..reply }, None);
        }
    }
    (reply, ticket)
}

/// At start and while the app runs: a fresh ticket for the saved key, straight from the server.
/// No answer means no license, as there is no offline grace. A definite no forgets the key.
/// Err carries the reason to show, if there is one.
pub async fn load(store: &Store) -> std::result::Result<Ticket, Option<String>> {
    let saved: Option<Ticket> = std::fs::read(file(store)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    // Only its key matters: the server decides, and explains when the answer is no.
    let Some(saved) = saved else {
        return Err(None);
    };
    let (reply, fresh) = post("/verify", &saved.key, Duration::from_secs(10)).await;
    if let Some(t) = fresh {
        let _ = save(store, &t);
        return Ok(t);
    }
    if !reply.reached {
        return Err(Some(UNREACHABLE.into()));
    }
    if reply.code != "BAD_TICKET" {
        let _ = std::fs::remove_file(file(store));
    }
    Err(Some(if reply.message.is_empty() { "License tidak valid.".into() } else { reply.message }))
}

/// Forget the license on this computer.
pub fn sign_out(store: &Store) {
    let _ = std::fs::remove_file(file(store));
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{signature::Signer, SigningKey};

    fn signed(key: &SigningKey, hwid: &str, expire: &str) -> Ticket {
        let mut t = Ticket { key: "FMP-TEST".into(), name: None, expire: expire.into(), issued: "2026-10-31T21:05:00Z".into(), sig: String::new() };
        let sig: Signature = key.sign(t.text(hwid).as_bytes());
        t.sig = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        t
    }

    #[test]
    fn dates() {
        assert_eq!(days_of("1970-01-01"), Some(0));
        assert_eq!(days_of("2026-10-31"), Some(20_757));
        assert_eq!(days_of("2024-03-01").unwrap() - days_of("2024-02-28").unwrap(), 2);
        assert_eq!(days_of("2026-13-01"), None);
        assert_eq!(days_of("garbage"), None);
    }

    #[test]
    fn ticket_is_bound_to_computer_product_and_date() {
        let signer = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let key = *signer.verifying_key();
        let day = days_of("2026-10-31").unwrap();

        let lifetime = signed(&signer, "ABCDEF0123456789", "");
        assert!(lifetime.valid_on(&key, "ABCDEF0123456789", day));
        assert!(!lifetime.valid_on(&key, "0000000000000000", day), "other computer");

        let mut changed = lifetime.clone();
        changed.key = "FMP-OTHER".into();
        assert!(!changed.valid_on(&key, "ABCDEF0123456789", day), "edited ticket");

        let dated = signed(&signer, "ABCDEF0123456789", "2026-11-01");
        assert!(dated.valid_on(&key, "ABCDEF0123456789", day));
        assert!(!dated.valid_on(&key, "ABCDEF0123456789", day + 1), "expiry day is over");

        // An RTE4FM ticket for the same key and computer must not open FM Packs.
        let mut rte = lifetime.clone();
        let text = format!("RTE4FM27|2|ABCDEF0123456789||{}|{}", rte.key, rte.issued);
        let sig: Signature = signer.sign(text.as_bytes());
        rte.sig = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        assert!(!rte.valid_on(&key, "ABCDEF0123456789", day));
    }

    /// Signed by WebCrypto, as the license Worker signs, so the two formats agree.
    #[test]
    fn accepts_a_ticket_signed_like_the_worker() {
        let point = hex::decode("0493548fc6d5e6757ff57eb179121a33e5304276390f1e9b0d3b215ddf3d14252e0da527ed594aee45cc5a09990d3b7a1c23aea51fd9bb3ef1f7295b6e3bb000d7").unwrap();
        let key = VerifyingKey::from_sec1_bytes(&point).unwrap();
        let ticket = Ticket {
            key: "FMP-AAAA-BBBB".into(),
            name: None,
            expire: String::new(),
            issued: "2026-10-31T15:06:44Z".into(),
            sig: "ufVEcSzEIDAu/K5Y/TzXK8ev0iiUEDkJyjKCgzmtYZ7oqFfF8C3xzDHuszIVpgkOjwwB5ZbGm5I4Ek8KHtUlbg==".into(),
        };
        assert!(ticket.signed_by(&key, "43BAAD0BB81B9042"));
        assert!(!ticket.signed_by(&key, "43BAAD0BB81B9043"));
    }

    #[test]
    fn hwid_is_sixteen_upper_hex() {
        let id = hwid();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)));
    }
}
