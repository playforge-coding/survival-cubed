//! Optional UPnP-IGD port forwarding for internet hosts.
//!
//! A host that wants to be reachable from outside its LAN normally has to log
//! into the router and forward the game's UDP port by hand. When the host opts
//! in, this module asks the local gateway — over UPnP-IGD, via the `easy-upnp`
//! crate — to forward that port automatically.
//!
//! The game speaks QUIC over **UDP**, so the mapping is for UDP only.
//!
//! ## Security
//!
//! Opening a port punches a hole through the router's NAT and exposes the
//! server to the entire internet, not just the LAN. UPnP performs this with no
//! authentication: any program (or malware) on the network can request it, and
//! a buggy or hostile router firmware can mishandle the request. The game's own
//! password still guards who may *join*, but the listening socket itself becomes
//! publicly reachable and exposed to unsolicited traffic. This is why forwarding
//! is **opt-in** and the UI/CLI surface [`SECURITY_WARNING`] whenever it is
//! enabled.
//!
//! Everything here is strictly best-effort: a gateway that lacks UPnP, has it
//! disabled, or rejects the request only costs the convenience of automatic
//! forwarding — the server still runs and can be reached if the port is
//! forwarded manually.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use easy_upnp::{PortMappingProtocol, UpnpConfig, add_ports, delete_ports};

/// Plain-language warning shown wherever UPnP forwarding can be turned on, so a
/// host understands they are exposing the server to the public internet.
pub const SECURITY_WARNING: &str = "\
UPnP tells your router to forward this port to your computer, making the server \
reachable from the public internet — not just your LAN. Only enable it if you \
intend to host for people outside your network, and keep a strong world \
password. Disable it if you only play on the LAN.";

/// How long each mapping is leased from the router, in seconds. The lease is a
/// safety net: if the host crashes without cleaning up, the router drops the
/// mapping on its own once this elapses.
const LEASE_SECS: u32 = 3600;
/// How often the mapping is renewed, comfortably before [`LEASE_SECS`] expires
/// so a long-running host stays reachable.
const RENEW_SECS: u64 = 1800;

/// Build the mapping request for `port` (UDP, address auto-detected).
fn config(port: u16) -> UpnpConfig {
    UpnpConfig {
        address: None,
        port,
        protocol: PortMappingProtocol::UDP,
        duration: LEASE_SECS,
        comment: "Survival Cubed".to_string(),
    }
}

/// Ask the gateway to forward `port` (UDP), collapsing `easy-upnp`'s per-mapping
/// result iterator into a single result.
fn open_mapping(port: u16) -> Result<()> {
    for result in add_ports([config(port)]) {
        result.context("gateway rejected the UPnP port mapping")?;
    }
    Ok(())
}

/// Ask the gateway to drop the forwarding for `port`.
fn close_mapping(port: u16) -> Result<()> {
    for result in delete_ports([config(port)]) {
        result.context("gateway rejected the UPnP unmapping")?;
    }
    Ok(())
}

/// A live UPnP forwarding for one UDP port. While held, a background thread
/// keeps the router's mapping fresh; dropping it removes the mapping.
///
/// Gateway discovery can block for a few seconds, so all the network work runs
/// off the caller's thread — constructing this never stalls the UI.
pub struct PortForward {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PortForward {
    /// Begin forwarding `port` on the local gateway, renewing the lease until
    /// dropped. Best-effort: discovery/mapping failures are logged, never fatal.
    pub fn open(port: u16) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("upnp".into())
            .spawn(move || run(port, worker_stop))
            .ok();
        PortForward { port, stop, handle }
    }
}

impl Drop for PortForward {
    fn drop(&mut self) {
        // Signal the worker, then leave it to remove the mapping and exit on its
        // own. We deliberately don't join: tearing this down (e.g. the host
        // leaving the world) shouldn't block on a gateway round-trip, and the
        // lease expiry is a backstop if the process dies before removal lands.
        self.stop.store(true, Ordering::SeqCst);
        drop(self.handle.take());
        log::info!("UPnP: releasing forwarding for UDP port {}", self.port);
    }
}

/// Background worker: open the mapping, then renew it on an interval until
/// `stop` is set, and finally tear it down.
fn run(port: u16, stop: Arc<AtomicBool>) {
    match open_mapping(port) {
        Ok(()) => log::info!("UPnP: forwarded UDP port {port} on the gateway"),
        Err(e) => {
            log::warn!(
                "UPnP: could not forward port {port} ({e:#}); \
                 forward it manually to host over the internet"
            );
            return;
        }
    }

    // Sleep in short slices so a stop request is noticed promptly rather than
    // after a full renewal interval.
    while !stop.load(Ordering::SeqCst) {
        let mut waited = 0;
        while waited < RENEW_SECS && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(1));
            waited += 1;
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Err(e) = open_mapping(port) {
            log::warn!("UPnP: renewing port {port} failed ({e:#})");
        }
    }

    match close_mapping(port) {
        Ok(()) => log::info!("UPnP: removed forwarding for UDP port {port}"),
        Err(e) => log::warn!("UPnP: removing port {port} failed ({e:#})"),
    }
}
