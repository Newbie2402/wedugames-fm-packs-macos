//! Command line access to the packer, used for creating the signing key.

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use packer::public_key_hex;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the signing key and print the public key for config/public_key.hex
    Keygen {
        #[arg(long, default_value = "keys/signing.key")]
        out: PathBuf,
    },
}

fn keygen(out: &Path) -> Result<()> {
    if out.exists() {
        bail!("{} sudah ada. Menimpanya membuat semua tool yang sudah beredar menolak update", out.display());
    }
    if let Some(dir) = out.parent() {
        fs::create_dir_all(dir)?;
    }
    let key = SigningKey::generate(&mut rand::rngs::OsRng);
    fs::write(out, hex::encode(key.to_bytes()))?;
    println!("{}", public_key_hex(&key));
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Keygen { out } => keygen(&out),
    }
}
