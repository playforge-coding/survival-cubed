//! The server-side voice-chat relay: an in-process [MOQ](https://moq.dev) origin
//! that fans every connected player's Opus broadcast out to all the others.
//!
//! It is a thin wrapper over [`moq_native`]: one shared [`OriginProducer`] is
//! handed to every accepted session as both its publish and consume side, so a
//! broadcast announced by one client is immediately announced to all the others
//! (the standard MOQ relay shape). The relay neither decodes nor inspects the
//! audio; clients ([`crate::client::voice`]) do all encoding, decoding, and
//! playback. It speaks plain MOQ — no game protocol — and runs on its own QUIC
//! endpoint, separate from the game server's.
//!
//! Started only when the server owner opts in (see
//! [`crate::server::RunningServer::enable_voice`]). The relay mints its own
//! self-signed certificate; its fingerprint travels to clients in
//! [`crate::voice::VoiceInfo`] so they can pin it.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use moq_native::moq_net::{Origin, OriginProducer};

use crate::voice::VoiceInfo;

/// A running voice relay. Dropping it aborts the accept loop and drops the QUIC
/// endpoint, stopping the relay.
pub struct VoiceRelay {
    /// The background accept loop; aborted on drop.
    accept: tokio::task::JoinHandle<()>,
}

impl Drop for VoiceRelay {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

impl VoiceRelay {
    /// Bind a MOQ relay on `bind` and start accepting voice sessions. Must be
    /// called from within a tokio runtime (the server's). Returns the relay
    /// handle plus the [`VoiceInfo`] (actual port + certificate fingerprint) to
    /// advertise to clients.
    pub async fn start(bind: SocketAddr) -> Result<(VoiceRelay, VoiceInfo)> {
        // Generate a throwaway self-signed certificate for the relay. Clients pin
        // it by the fingerprint we report, so the hostname is irrelevant.
        let mut config = moq_native::ServerConfig::default();
        config.bind = Some(bind.to_string());
        config.tls.generate = vec!["localhost".to_string()];

        let server = config.init().context("starting voice relay endpoint")?;

        let port = server
            .local_addr()
            .context("voice relay local address")?
            .port();

        // moq-native reports each loaded/generated certificate's SHA-256 as hex;
        // the leaf's is what the client pins.
        let cert_hash = server
            .tls_info()
            .read()
            .expect("voice relay tls info lock poisoned")
            .fingerprints
            .first()
            .cloned()
            .context("voice relay produced no certificate fingerprint")?;

        // One shared origin: every session both publishes its broadcast into it
        // and consumes the others' from it, so the relay mirrors everyone to
        // everyone.
        let origin = Origin::random().produce();

        let accept = tokio::spawn(accept_loop(server, origin));

        log::info!("voice relay listening on port {port}");
        Ok((VoiceRelay { accept }, VoiceInfo { port, cert_hash }))
    }
}

/// Accept voice sessions until the server endpoint closes, wiring each into the
/// shared `origin` so broadcasts fan out between all participants.
async fn accept_loop(mut server: moq_native::Server, origin: OriginProducer) {
    while let Some(request) = server.accept().await {
        let origin = origin.clone();
        tokio::spawn(async move {
            // Publish *to* this peer everything in the shared origin, and route
            // whatever this peer publishes *into* the shared origin.
            let session = match request
                .with_publish(origin.consume())
                .with_consume(origin)
                .ok()
                .await
            {
                Ok(session) => session,
                Err(e) => {
                    log::debug!("voice session rejected: {e}");
                    return;
                }
            };
            // Hold the session until the peer disconnects; dropping it would tear
            // the connection down immediately.
            let _ = session.closed().await;
        });
    }
}
