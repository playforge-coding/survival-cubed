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

use crate::protocol::PROTOCOL_VERSION;

/// Largest frame we will read, as a guard against bad/hostile peers.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// QUIC application close code the server uses to reject a version-skewed peer.
/// (The accompanying close reason carries the human-readable explanation.)
pub const VERSION_MISMATCH_CLOSE: u32 = 1;

/// Write the fixed 4-byte little-endian [`PROTOCOL_VERSION`] header that opens
/// every connection. This framing is frozen across all versions, so even peers
/// that disagree on the bincode layout can still read it and detect the skew
/// instead of mis-decoding each other's messages.
pub async fn write_version(send: &mut SendStream) -> Result<()> {
    send.write_all(&PROTOCOL_VERSION.to_le_bytes()).await?;
    Ok(())
}

/// Read the peer's 4-byte protocol-version header (the counterpart to
/// [`write_version`]).
pub async fn read_version(recv: &mut RecvStream) -> Result<u32> {
    let mut v = [0u8; 4];
    recv.read_exact(&mut v).await?;
    Ok(u32::from_le_bytes(v))
}

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
    // The [`PROTOCOL_VERSION`] handshake already rejected version-skewed peers
    // before any message was read, so a decode failure here is a corrupt/truncated
    // frame on the wire, not a version mismatch — don't mislead the user about that.
    bincode::deserialize(&buf).map_err(|e| anyhow!("protocol error decoding message: {e}"))
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

// --- Saved login credentials --------------------------------------------

/// Path to the persistent saved-passwords file under the user config dir.
fn credentials_path() -> Option<PathBuf> {
    let mut p = dirs::config_dir()?;
    p.push("survival-cubed");
    Some(p.join("credentials"))
}

/// Remembers the password used for each `(server, name)` pair so a returning
/// player doesn't have to retype it every time. Stored locally in the user
/// config dir; this is a convenience cache, so passwords are kept in the clear
/// (like a browser's saved logins) rather than encrypted.
///
/// The on-disk format is one `key<TAB>password` line per entry, where the key is
/// `"<host label>\0<player name>"`. A tab separates key from value and neither
/// the server label nor the name can contain one, so parsing is unambiguous.
#[derive(Default)]
pub struct Credentials {
    entries: HashMap<String, String>,
}

impl Credentials {
    /// Build the lookup key for a `(server label, player name)` pair.
    fn key(host: &str, name: &str) -> String {
        format!("{host}\u{0}{name}")
    }

    /// Load from disk, returning an empty store if the file is absent.
    pub fn load() -> Self {
        let mut creds = Credentials::default();
        let Some(path) = credentials_path() else {
            return creds;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return creds;
        };
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, password)) = line.split_once('\t') {
                creds.entries.insert(key.to_string(), password.to_string());
            }
        }
        creds
    }

    /// The saved password for `(host, name)`, if any.
    pub fn get(&self, host: &str, name: &str) -> Option<&str> {
        self.entries.get(&Self::key(host, name)).map(String::as_str)
    }

    /// Whether any saved login exists for player `name` on *any* server. The menu
    /// uses this to hint that a returning player may leave the password blank (the
    /// stored credential is supplied for them), rather than always demanding one.
    pub fn has_any_for_name(&self, name: &str) -> bool {
        let suffix = format!("\u{0}{name}");
        self.entries.keys().any(|k| k.ends_with(&suffix))
    }

    /// Remember `password` for `(host, name)` and persist the store. A password
    /// equal to one already stored is a no-op (avoids a pointless rewrite).
    pub fn add_and_save(&mut self, host: &str, name: &str, password: &str) -> Result<()> {
        let key = Self::key(host, name);
        if self.entries.get(&key).map(String::as_str) == Some(password) {
            return Ok(());
        }
        self.entries.insert(key, password.to_string());
        self.save()
    }

    fn save(&self) -> Result<()> {
        let path = credentials_path().ok_or_else(|| anyhow!("no config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out =
            String::from("# survival-cubed saved logins (\"<server>\\0<name>\\t<password>\")\n");
        for (key, password) in &self.entries {
            // Skip any entry whose value would break the line-based format.
            if password.contains('\t') || password.contains('\n') || key.contains('\n') {
                continue;
            }
            out.push_str(key);
            out.push('\t');
            out.push_str(password);
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
