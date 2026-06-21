//! Networking utilities shared by client and server: length-prefixed message
//! framing over QUIC streams, certificate fingerprints, and the trust-on-
//! first-use (TOFU) `known_hosts` store.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use quinn::{RecvStream, SendStream};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

/// Largest frame we will read, as a guard against bad/hostile peers.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Write a bincode-serialized message with a 4-byte little-endian length prefix.
pub async fn write_msg<T: Serialize>(send: &mut SendStream, msg: &T) -> Result<()> {
    let bytes = bincode::serialize(msg)?;
    send.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

/// Read one length-prefixed bincode message.
pub async fn read_msg<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(anyhow!("frame too large: {len} bytes"));
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(bincode::deserialize(&buf)?)
}

/// SHA-256 of a certificate's DER encoding.
pub fn fingerprint(cert_der: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(cert_der);
    h.finalize().into()
}

/// Human-readable colon-separated hex, like OpenSSH prints.
pub fn fingerprint_hex(fp: &[u8; 32]) -> String {
    fp.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Path to the persistent `known_hosts` file under the user config dir.
fn known_hosts_path() -> Option<PathBuf> {
    let mut p = dirs::config_dir()?;
    p.push("survival-cubed");
    Some(p.join("known_hosts"))
}

/// Maps a host label (e.g. `"127.0.0.1:5000"`) to a trusted fingerprint.
#[derive(Default)]
pub struct KnownHosts {
    entries: HashMap<String, [u8; 32]>,
}

impl KnownHosts {
    /// Load from disk, returning an empty store if the file is absent.
    pub fn load() -> Self {
        let mut kh = KnownHosts::default();
        let Some(path) = known_hosts_path() else {
            return kh;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return kh;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((host, hex)) = line.split_once(char::is_whitespace) {
                if let Some(fp) = parse_fingerprint(hex.trim()) {
                    kh.entries.insert(host.to_string(), fp);
                }
            }
        }
        kh
    }

    pub fn get(&self, host: &str) -> Option<&[u8; 32]> {
        self.entries.get(host)
    }

    /// Record a trusted fingerprint and persist the store.
    pub fn add_and_save(&mut self, host: &str, fp: [u8; 32]) -> Result<()> {
        self.entries.insert(host.to_string(), fp);
        let path = known_hosts_path().ok_or_else(|| anyhow!("no config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = String::from("# survival-cubed known hosts (host fingerprint)\n");
        for (host, fp) in &self.entries {
            out.push_str(host);
            out.push(' ');
            out.push_str(&fingerprint_hex(fp));
            out.push('\n');
        }
        std::fs::write(&path, out)?;
        Ok(())
    }
}

pub fn parse_fingerprint(hex: &str) -> Option<[u8; 32]> {
    let bytes: Vec<&str> = hex.split(':').collect();
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in bytes.iter().enumerate() {
        out[i] = u8::from_str_radix(b, 16).ok()?;
    }
    Some(out)
}
