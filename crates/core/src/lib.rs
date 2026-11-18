pub mod detect;
pub mod download;
pub mod engine;
pub mod install;
pub mod license;
pub mod manifest;
pub mod state;

/// Ed25519 public key that manifests must be signed with.
pub const PUBLIC_KEY_HEX: &str = include_str!("../../../config/public_key.hex");
/// Base URL of the Cloudflare Worker.
pub const API_URL: &str = include_str!("../../../config/api_url.txt");
