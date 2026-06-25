//! Client-side networking: a background tokio thread driving a quinn
//! connection, plus the trust-on-first-use certificate verifier.
//!
//! The main (winit) thread talks to this thread over channels: it reads
//! [`NetEvent`]s and sends [`NetCommand`]s. When the server presents an
//! unknown certificate, the verifier emits [`NetEvent::TofuPrompt`] and blocks
//! its handshake until the UI sends back a decision.

use std::net::SocketAddr;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::entity::{Entity, EntityId, EntityKind};
use crate::inventory::Slot;
use crate::net::{KnownHosts, fingerprint, fingerprint_hex, read_msg, write_msg, write_version};
use crate::protocol::{ALPN, BlockId, ClientMessage, ServerMessage, Waypoint};
use crate::world::Dimension;

/// Events flowing from the network thread to the UI.
pub enum NetEvent {
    /// Server certificate is unknown; ask the user. Reply via `respond`.
    TofuPrompt {
        host: String,
        fingerprint: String,
        respond: Sender<bool>,
    },
    /// Handshake + welcome complete.
    Connected {
        entity_id: EntityId,
        spawn_x: f32,
        spawn_y: f32,
        /// Whether this client may enter creator mode on this server.
        creator_allowed: bool,
    },
    Chunk {
        dim: Dimension,
        cx: i32,
        cy: i32,
        blocks: Vec<BlockId>,
    },
    BlockUpdate {
        dim: Dimension,
        x: i32,
        y: i32,
        block: BlockId,
    },
    BlocksUpdate {
        dim: Dimension,
        cells: Vec<(i32, i32, BlockId)>,
    },
    /// Player-written text on the sign or quest board at cell `(x, y)`.
    BlockText {
        dim: Dimension,
        x: i32,
        y: i32,
        text: crate::protocol::BlockText,
    },
    /// Move into a new dimension at world pixel `(x, y)`: the client clears its
    /// world and entities and re-streams the new dimension.
    EnterDimension {
        dim: Dimension,
        x: f32,
        y: f32,
    },
    EntitySpawn {
        entity: Entity,
    },
    EntityMoved {
        id: EntityId,
        x: f32,
        y: f32,
        vx: f32,
        vy: f32,
    },
    EntityDespawn {
        id: EntityId,
    },
    EntityBoating {
        id: EntityId,
        on: bool,
    },
    EntityRiding {
        id: EntityId,
        horse: Option<EntityId>,
    },
    EntityDying {
        id: EntityId,
    },
    EntityLunging {
        id: EntityId,
    },
    EntityHealth {
        id: EntityId,
        health: i32,
        max_health: i32,
    },
    EntityHit {
        id: EntityId,
        vx: f32,
        vy: f32,
    },
    TimeOfDay {
        t: f32,
    },
    Respawn {
        x: f32,
        y: f32,
        died: bool,
    },
    /// Authoritative snapshot of this client's waypoints and home (respawn) point.
    Waypoints {
        list: Vec<Waypoint>,
        home: (f32, f32),
    },
    /// Authoritative snapshot of this client's inventory slots.
    Inventory {
        slots: Vec<Slot>,
    },
    /// A chat line to display, attributed to player `from`.
    Chat {
        from: String,
        text: String,
    },
    /// Begin (`Some`) or end (`None`) spectating another player's entity. Only
    /// received by an admin who issued `/spectate`.
    Spectate {
        target: Option<EntityId>,
    },
    /// Connection closed (or never established). `reason` is human-readable.
    Disconnected {
        reason: String,
    },
}

/// Commands flowing from the UI to the network thread.
pub enum NetCommand {
    SetBlock {
        x: i32,
        y: i32,
        block: BlockId,
        held: BlockId,
    },
    PlaceBlock {
        x: i32,
        y: i32,
        slot: u8,
    },
    /// Use the bucket in hotbar `slot` on cell `(x, y)`: an empty bucket scoops
    /// up water, a water bucket pours it out (server validates and resyncs).
    UseBucket {
        x: i32,
        y: i32,
        slot: u8,
    },
    /// Use the fire key in hotbar `slot` to warp between dimensions (server
    /// validates the slot, moves the player, and resyncs).
    UseFireKey {
        slot: u8,
    },
    /// Swing the door touching cell `(x, y)` open or shut (server flips both
    /// halves and resyncs).
    ToggleDoor {
        x: i32,
        y: i32,
    },
    MoveItem {
        from: u8,
        to: u8,
    },
    /// Drop inventory slot `slot` onto the ground at the player's feet. `all`
    /// drops the whole stack, otherwise a single item. `dir` is the player's
    /// facing (`-1.0` left, `+1.0` right) for the toss direction.
    DropItem {
        slot: u8,
        all: bool,
        dir: f32,
    },
    /// Craft the recipe at index `recipe` once (server validates materials).
    Craft {
        recipe: u16,
    },
    /// Smelt the forge recipe at index `recipe` up to `count` times, burning
    /// `fuel` (wood, coal, or bark).
    Smelt {
        recipe: u16,
        count: u32,
        fuel: BlockId,
    },
    /// Repair one worn tool of type `item` at a forge (consumes its material).
    Repair {
        item: BlockId,
    },
    /// Eat the food item in inventory `slot` (server adjusts health).
    Eat {
        slot: u8,
    },
    /// Feed one unit of `fuel` (wood, coal, or bark) to the campfire at cell `(x, y)`.
    FuelCampfire {
        x: i32,
        y: i32,
        fuel: BlockId,
    },
    /// Cook the campfire recipe at index `recipe` up to `count` times at the
    /// (lit) campfire at cell `(x, y)`.
    Cook {
        x: i32,
        y: i32,
        recipe: u16,
        count: u32,
    },
    /// Set the player's respawn point to the campfire at cell `(x, y)`.
    SetRespawn {
        x: i32,
        y: i32,
    },
    /// Drop a personal waypoint at world pixel `(x, y)` with `color`.
    AddWaypoint {
        x: f32,
        y: f32,
        color: [f32; 3],
    },
    /// Remove the personal waypoint nearest to world pixel `(x, y)`.
    RemoveWaypoint {
        x: f32,
        y: f32,
    },
    PlayerMove {
        x: f32,
        y: f32,
    },
    RequestChunk {
        dim: Dimension,
        cx: i32,
        cy: i32,
    },
    Attack {
        target: EntityId,
        held: BlockId,
    },
    FallDamage {
        amount: i32,
    },
    /// Set whether this player is riding a boat, so the server can share the
    /// riding pose with other clients.
    SetBoating {
        on: bool,
    },
    /// Mount the tamed horse with this id, or dismount (`None`), so the server can
    /// glue the horse beneath the rider and share the pose with other clients.
    SetRiding {
        horse: Option<EntityId>,
    },
    /// Creator mode: toggle this player's creator mode on or off.
    SetCreator {
        on: bool,
    },
    /// Creator mode: jump the world clock to time of day `t` in `[0, 1)`.
    SetTime {
        t: f32,
    },
    /// Creator mode: spawn a creature of `kind` at world pixel `(x, y)`.
    SpawnEntity {
        kind: EntityKind,
        x: f32,
        y: f32,
    },
    /// Creator mode: drop `count` of item `item` straight into the inventory.
    GiveItem {
        item: BlockId,
        count: u32,
    },
    /// Creator mode: place `block` at a world cell for free (infinite blocks).
    CreatorSetBlock {
        x: i32,
        y: i32,
        block: BlockId,
    },
    /// Creator mode: place many cells at once (stamping a saved structure).
    CreatorSetBlocks {
        cells: Vec<(i32, i32, BlockId)>,
    },
    /// Send a line of chat to the server for rebroadcast.
    Chat {
        text: String,
    },
    /// Write `text` onto the sign or quest board at cell `(x, y)` (server validates,
    /// clamps, stores, and rebroadcasts it).
    WriteBlockText {
        x: i32,
        y: i32,
        text: crate::protocol::BlockText,
    },
    Disconnect,
}

/// The UI's handle to a network connection.
pub struct NetHandle {
    pub events: Receiver<NetEvent>,
    pub commands: tokio::sync::mpsc::UnboundedSender<NetCommand>,
}

/// Connect to `addr`. `host_label` keys the `known_hosts` store and is shown in
/// prompts. `player_name` is the display name announced in `Hello` (used to
/// restore saved state and to attribute chat). `password` authenticates that
/// name with the server (registering it on first join, or matching the stored
/// one thereafter). `trust`, if set, is a fingerprint to silently accept (used
/// by the embedded singleplayer server). `creator_token`, if set, is the
/// per-server admin secret presented in `Hello` to authorize the host (only the
/// host's own client holds it).
pub fn connect(
    addr: SocketAddr,
    host_label: String,
    player_name: String,
    password: String,
    trust: Option<[u8; 32]>,
    creator_token: Option<u64>,
) -> NetHandle {
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<NetEvent>();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<NetCommand>();

    let ev_for_thread = ev_tx.clone();
    std::thread::Builder::new()
        .name("game-net".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ev_for_thread.send(NetEvent::Disconnected {
                        reason: format!("runtime error: {e}"),
                    });
                    return;
                }
            };
            rt.block_on(async move {
                if let Err(e) = client_main(
                    addr,
                    host_label,
                    player_name,
                    password,
                    trust,
                    creator_token,
                    &ev_for_thread,
                    cmd_rx,
                )
                .await
                {
                    let _ = ev_for_thread.send(NetEvent::Disconnected {
                        reason: format!("{e:#}"),
                    });
                }
            });
        })
        .expect("spawn net thread");

    NetHandle {
        events: ev_rx,
        commands: cmd_tx,
    }
}

async fn client_main(
    addr: SocketAddr,
    host_label: String,
    player_name: String,
    password: String,
    trust: Option<[u8; 32]>,
    creator_token: Option<u64>,
    ev_tx: &Sender<NetEvent>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<NetCommand>,
) -> anyhow::Result<()> {
    let verifier = Arc::new(TofuVerifier::new(host_label, trust, ev_tx.clone()));

    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let qcc = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?;
    let client_config = quinn::ClientConfig::new(Arc::new(qcc));

    let mut endpoint = quinn::Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))?;
    endpoint.set_default_client_config(client_config);

    let connection = endpoint.connect(addr, "localhost")?.await?;
    let (mut send, mut recv) = connection.open_bi().await?;

    // Announce our wire version first; a server on a different version closes the
    // connection with an explanatory reason (surfaced below) rather than letting
    // us mis-decode its messages later.
    write_version(&mut send).await?;

    write_msg(
        &mut send,
        &ClientMessage::Hello {
            name: player_name,
            password,
            creator_token,
        },
    )
    .await?;

    loop {
        tokio::select! {
            msg = read_msg::<ServerMessage>(&mut recv) => {
                let msg = match msg {
                    Ok(msg) => msg,
                    // If the server closed the stream with a reason (e.g. a
                    // version mismatch), report that rather than the low-level
                    // read error it manifests as on our end.
                    Err(e) => match connection.close_reason() {
                        Some(quinn::ConnectionError::ApplicationClosed(close))
                            if !close.reason.is_empty() =>
                        {
                            return Err(anyhow::anyhow!(
                                "{}",
                                String::from_utf8_lossy(&close.reason)
                            ));
                        }
                        _ => return Err(e),
                    },
                };
                if dispatch(msg, ev_tx).is_break() {
                    break;
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(NetCommand::Disconnect) | None => {
                        let _ = send.finish();
                        connection.close(0u32.into(), b"bye");
                        break;
                    }
                    Some(cmd) => {
                        write_msg(&mut send, &to_client_message(cmd)).await?;
                    }
                }
            }
        }
    }

    let _ = ev_tx.send(NetEvent::Disconnected {
        reason: "connection closed".to_string(),
    });
    Ok(())
}

fn to_client_message(cmd: NetCommand) -> ClientMessage {
    match cmd {
        NetCommand::SetBlock { x, y, block, held } => ClientMessage::SetBlock { x, y, block, held },
        NetCommand::PlaceBlock { x, y, slot } => ClientMessage::PlaceBlock { x, y, slot },
        NetCommand::UseBucket { x, y, slot } => ClientMessage::UseBucket { x, y, slot },
        NetCommand::UseFireKey { slot } => ClientMessage::UseFireKey { slot },
        NetCommand::ToggleDoor { x, y } => ClientMessage::ToggleDoor { x, y },
        NetCommand::MoveItem { from, to } => ClientMessage::MoveItem { from, to },
        NetCommand::DropItem { slot, all, dir } => ClientMessage::DropItem { slot, all, dir },
        NetCommand::Craft { recipe } => ClientMessage::Craft { recipe },
        NetCommand::Smelt {
            recipe,
            count,
            fuel,
        } => ClientMessage::Smelt {
            recipe,
            count,
            fuel,
        },
        NetCommand::PlayerMove { x, y } => ClientMessage::PlayerMove { x, y },
        NetCommand::RequestChunk { dim, cx, cy } => ClientMessage::RequestChunk { dim, cx, cy },
        NetCommand::Attack { target, held } => ClientMessage::Attack { target, held },
        NetCommand::Repair { item } => ClientMessage::Repair { item },
        NetCommand::Eat { slot } => ClientMessage::Eat { slot },
        NetCommand::FuelCampfire { x, y, fuel } => ClientMessage::FuelCampfire { x, y, fuel },
        NetCommand::Cook {
            x,
            y,
            recipe,
            count,
        } => ClientMessage::Cook {
            x,
            y,
            recipe,
            count,
        },
        NetCommand::SetRespawn { x, y } => ClientMessage::SetRespawn { x, y },
        NetCommand::AddWaypoint { x, y, color } => ClientMessage::AddWaypoint { x, y, color },
        NetCommand::RemoveWaypoint { x, y } => ClientMessage::RemoveWaypoint { x, y },
        NetCommand::FallDamage { amount } => ClientMessage::FallDamage { amount },
        NetCommand::SetBoating { on } => ClientMessage::SetBoating { on },
        NetCommand::SetRiding { horse } => ClientMessage::SetRiding { horse },
        NetCommand::SetCreator { on } => ClientMessage::SetCreator { on },
        NetCommand::SetTime { t } => ClientMessage::SetTime { t },
        NetCommand::SpawnEntity { kind, x, y } => ClientMessage::SpawnEntity { kind, x, y },
        NetCommand::GiveItem { item, count } => ClientMessage::GiveItem { item, count },
        NetCommand::CreatorSetBlock { x, y, block } => {
            ClientMessage::CreatorSetBlock { x, y, block }
        }
        NetCommand::CreatorSetBlocks { cells } => ClientMessage::CreatorSetBlocks { cells },
        NetCommand::Chat { text } => ClientMessage::Chat { text },
        NetCommand::WriteBlockText { x, y, text } => ClientMessage::WriteBlockText { x, y, text },
        NetCommand::Disconnect => unreachable!("handled before conversion"),
    }
}

fn dispatch(msg: ServerMessage, ev_tx: &Sender<NetEvent>) -> std::ops::ControlFlow<()> {
    let ev = match msg {
        ServerMessage::Welcome {
            entity_id,
            spawn_x,
            spawn_y,
            creator_allowed,
        } => NetEvent::Connected {
            entity_id,
            spawn_x,
            spawn_y,
            creator_allowed,
        },
        ServerMessage::Chunk {
            dim,
            cx,
            cy,
            blocks,
        } => NetEvent::Chunk {
            dim,
            cx,
            cy,
            blocks,
        },
        ServerMessage::BlockUpdate { dim, x, y, block } => {
            NetEvent::BlockUpdate { dim, x, y, block }
        }
        ServerMessage::BlocksUpdate { dim, cells } => NetEvent::BlocksUpdate { dim, cells },
        ServerMessage::BlockText { dim, x, y, text } => NetEvent::BlockText { dim, x, y, text },
        ServerMessage::EnterDimension { dim, x, y } => NetEvent::EnterDimension { dim, x, y },
        ServerMessage::EntitySpawn { entity } => NetEvent::EntitySpawn { entity },
        ServerMessage::EntityMoved { id, x, y, vx, vy } => {
            NetEvent::EntityMoved { id, x, y, vx, vy }
        }
        ServerMessage::EntityDespawn { id } => NetEvent::EntityDespawn { id },
        ServerMessage::EntityBoating { id, on } => NetEvent::EntityBoating { id, on },
        ServerMessage::EntityRiding { id, horse } => NetEvent::EntityRiding { id, horse },
        ServerMessage::EntityDying { id } => NetEvent::EntityDying { id },
        ServerMessage::EntityLunging { id } => NetEvent::EntityLunging { id },
        ServerMessage::EntityHealth {
            id,
            health,
            max_health,
        } => NetEvent::EntityHealth {
            id,
            health,
            max_health,
        },
        ServerMessage::EntityHit { id, vx, vy } => NetEvent::EntityHit { id, vx, vy },
        ServerMessage::TimeOfDay { t } => NetEvent::TimeOfDay { t },
        ServerMessage::Respawn { x, y, died } => NetEvent::Respawn { x, y, died },
        ServerMessage::Waypoints { list, home } => NetEvent::Waypoints { list, home },
        ServerMessage::Inventory { slots } => NetEvent::Inventory { slots },
        ServerMessage::Chat { from, text } => NetEvent::Chat { from, text },
        ServerMessage::Spectate { target } => NetEvent::Spectate { target },
    };
    if ev_tx.send(ev).is_err() {
        std::ops::ControlFlow::Break(())
    } else {
        std::ops::ControlFlow::Continue(())
    }
}

// --- TOFU certificate verifier -------------------------------------------

struct TofuVerifier {
    host: String,
    trust: Option<[u8; 32]>,
    known: Mutex<KnownHosts>,
    prompt_tx: Sender<NetEvent>,
    algs: WebPkiSupportedAlgorithms,
}

impl TofuVerifier {
    fn new(host: String, trust: Option<[u8; 32]>, prompt_tx: Sender<NetEvent>) -> Self {
        TofuVerifier {
            host,
            trust,
            known: Mutex::new(KnownHosts::load()),
            prompt_tx,
            algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        }
    }
}

impl std::fmt::Debug for TofuVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TofuVerifier")
            .field("host", &self.host)
            .finish()
    }
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity.as_ref());

        // Embedded / pre-trusted fingerprint: accept silently.
        if let Some(trusted) = self.trust {
            if trusted == fp {
                return Ok(ServerCertVerified::assertion());
            }
        }

        // Previously accepted host: compare against the pinned fingerprint.
        if let Some(known) = self.known.lock().get(&self.host).copied() {
            return if known == fp {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(rustls::Error::General(format!(
                    "REMOTE HOST CERTIFICATE CHANGED for {} (possible MITM); refusing to connect",
                    self.host
                )))
            };
        }

        // Unknown host: ask the user (SSH-style TOFU prompt).
        let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
        let _ = self.prompt_tx.send(NetEvent::TofuPrompt {
            host: self.host.clone(),
            fingerprint: fingerprint_hex(&fp),
            respond: resp_tx,
        });
        match resp_rx.recv() {
            Ok(true) => {
                let _ = self.known.lock().add_and_save(&self.host, fp);
                Ok(ServerCertVerified::assertion())
            }
            _ => Err(rustls::Error::General(
                "certificate rejected by user".to_string(),
            )),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}
