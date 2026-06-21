//! LAN service discovery over mDNS / DNS-SD (via the `mdns-sd` crate).
//!
//! A host advertises a `_survival-cubed._udp.local.` service carrying its game
//! port and certificate fingerprint; clients browse for it on the menu screen
//! and can join without typing an address. The advertised fingerprint is used to
//! pre-trust the certificate, so a LAN join skips the TOFU prompt (see
//! [`crate::client::net`]).
//!
//! Discovery is strictly best-effort: a missing or firewalled mDNS stack only
//! costs the convenience of auto-listing, never the ability to host or to join
//! by typing an address.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::Mutex;

use crate::net::{fingerprint_hex, parse_fingerprint};

/// DNS-SD service type advertised and browsed by the game.
const SERVICE_TYPE: &str = "_survival-cubed._udp.local.";
/// TXT-record key carrying the server's hex certificate fingerprint.
const FP_KEY: &str = "fp";

/// A live mDNS advertisement of this host's server. Dropping it unregisters the
/// service and stops the daemon, so the host disappears from other clients'
/// menus promptly.
pub struct LanAdvertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for LanAdvertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Advertise a server listening on `port` under the friendly `instance` name,
/// publishing `fingerprint` so discovering clients can pre-trust the cert.
pub fn advertise(port: u16, instance: &str, fingerprint: &[u8; 32]) -> Result<LanAdvertiser> {
    let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;

    // The host name only needs to be unique-ish on the link; with addr-auto the
    // daemon fills in this machine's interface addresses for us.
    let host_name = format!("survival-cubed-{port}.local.");
    let mut props = HashMap::new();
    props.insert(FP_KEY.to_string(), fingerprint_hex(fingerprint));

    let info = ServiceInfo::new(SERVICE_TYPE, instance, &host_name, "", port, props)
        .context("building mDNS service info")?
        .enable_addr_auto();
    let fullname = info.get_fullname().to_string();

    daemon.register(info).context("registering mDNS service")?;
    Ok(LanAdvertiser { daemon, fullname })
}

/// A server seen on the LAN. `fingerprint` is present when the host advertised
/// one (it always does), letting the client connect without a TOFU prompt.
#[derive(Clone)]
pub struct DiscoveredServer {
    pub name: String,
    pub addr: SocketAddr,
    pub fingerprint: Option<[u8; 32]>,
    /// Fully-qualified DNS-SD name, used to match add/remove events.
    fullname: String,
}

/// Continuously browses the LAN for game servers on a background thread,
/// maintaining a snapshot the UI polls each frame. Dropping it stops browsing.
pub struct LanBrowser {
    daemon: ServiceDaemon,
    servers: Arc<Mutex<Vec<DiscoveredServer>>>,
}

impl Drop for LanBrowser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}

/// Start browsing for LAN servers. The returned handle keeps the search alive.
pub fn browse() -> Result<LanBrowser> {
    let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .context("starting mDNS browse")?;

    let servers: Arc<Mutex<Vec<DiscoveredServer>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = servers.clone();
    std::thread::Builder::new()
        .name("mdns-browse".into())
        .spawn(move || {
            // Ends when the daemon is shut down (on `LanBrowser` drop), which
            // closes the channel.
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        if let Some(server) = to_server(&info) {
                            let mut list = sink.lock();
                            list.retain(|s| s.fullname != server.fullname);
                            list.push(server);
                        }
                    }
                    ServiceEvent::ServiceRemoved(_ty, fullname) => {
                        sink.lock().retain(|s| s.fullname != fullname);
                    }
                    _ => {}
                }
            }
        })
        .context("spawning mDNS browse thread")?;

    Ok(LanBrowser { daemon, servers })
}

impl LanBrowser {
    /// A snapshot of the servers currently visible on the LAN.
    pub fn servers(&self) -> Vec<DiscoveredServer> {
        self.servers.lock().clone()
    }
}

/// Turn a resolved mDNS service into a joinable server, or `None` if it
/// advertised no reachable IPv4 address (our endpoints bind v4).
fn to_server(info: &ResolvedService) -> Option<DiscoveredServer> {
    let ip = info.get_addresses_v4().into_iter().next()?;
    Some(DiscoveredServer {
        name: instance_label(info.get_fullname()),
        addr: SocketAddr::new(ip.into(), info.get_port()),
        fingerprint: info
            .get_property_val_str(FP_KEY)
            .and_then(parse_fingerprint),
        fullname: info.get_fullname().to_string(),
    })
}

/// Strip the service-type suffix from a fullname to recover the friendly
/// instance label the host registered.
fn instance_label(fullname: &str) -> String {
    fullname
        .strip_suffix(SERVICE_TYPE)
        .map(|s| s.trim_end_matches('.'))
        .unwrap_or(fullname)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercises real multicast on the loopback/LAN, so it is opt-in
    // (`cargo test -- --ignored`) to keep the default suite hermetic.
    #[test]
    #[ignore = "requires live mDNS multicast"]
    fn advertise_is_discoverable() {
        let fp = [0xABu8; 32];
        let _ad = advertise(5099, "Test World", &fp).expect("advertise");
        let browser = browse().expect("browse");
        let mut found = None;
        for _ in 0..50 {
            let servers = browser.servers();
            if let Some(s) = servers.into_iter().find(|s| s.addr.port() == 5099) {
                found = Some(s);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let s = found.expect("discovered the advertised server");
        assert_eq!(s.name, "Test World");
        assert_eq!(s.fingerprint, Some(fp));
    }
}
