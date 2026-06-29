//! Wire protocol shared between client and server.
//!
//! Messages are serialized with `bincode` and sent length-prefixed (see
//! [`crate::net`]). The protocol is intentionally tiny for now; it carries
//! block ids ([`BlockId`]) as the common currency between both sides.

use serde::{Deserialize, Serialize};

use crate::entity::{Entity, EntityId, EntityKind};
use crate::inventory::Slot;
use crate::world::Dimension;

/// Identifier of a block type. `0` is always air. See [`crate::block`].
pub type BlockId = u16;

/// A player-placed map marker. Its world position is the player's top-left
/// (matching [`crate::entity`] coordinates), and `color` is a stable RGB chosen
/// when the waypoint is created, so the on-screen dot keeps the same hue for the
/// life of the waypoint. Default markers (home, last death) are derived on the
/// client and never travel as `Waypoint`s.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Waypoint {
    pub x: f32,
    pub y: f32,
    pub color: [f32; 3],
}

/// Maximum characters allowed on a single line of a [`BlockText`] (sign or quest
/// board). Lines longer than this are truncated when written.
pub const TEXT_COLS: usize = 15;
/// Maximum lines in one [`BlockText`] note — both a sign's body and each note of a
/// quest board cap out here.
pub const TEXT_ROWS: usize = 5;
/// Maximum notes a quest board ([`BlockText::Quest`]) may hold.
pub const QUEST_MAX_NOTES: usize = 5;

/// Player-written text attached to a placed block, addressed by its world cell.
/// A [`sign`](crate::block::SIGN) carries a single note of up to [`TEXT_ROWS`]
/// lines; a [`quest board`](crate::block::QUEST_BOARD) carries up to
/// [`QUEST_MAX_NOTES`] such notes. Every line is capped at [`TEXT_COLS`]
/// characters. The server stores it per cell, syncs it to clients, and persists it
/// (see [`crate::server`] and [`crate::save`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BlockText {
    /// A sign's body: up to [`TEXT_ROWS`] lines.
    Sign(Vec<String>),
    /// A quest board's notes: up to [`QUEST_MAX_NOTES`] notes, each up to
    /// [`TEXT_ROWS`] lines.
    Quest(Vec<Vec<String>>),
}

/// Truncate one line to [`TEXT_COLS`] characters (counting Unicode scalar values).
fn clamp_line(line: &str) -> String {
    line.chars().take(TEXT_COLS).collect()
}

/// Clamp a note to [`TEXT_ROWS`] lines, each truncated to [`TEXT_COLS`] characters.
fn clamp_note(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .take(TEXT_ROWS)
        .map(|l| clamp_line(l))
        .collect()
}

impl BlockText {
    /// A copy clamped to the line/row/note limits, so untrusted client input can't
    /// store oversized text. The server clamps every write before storing it.
    pub fn sanitized(&self) -> BlockText {
        match self {
            BlockText::Sign(lines) => BlockText::Sign(clamp_note(lines)),
            BlockText::Quest(notes) => BlockText::Quest(
                notes
                    .iter()
                    .take(QUEST_MAX_NOTES)
                    .map(|n| clamp_note(n))
                    .collect(),
            ),
        }
    }

    /// Whether this holds no actual text (every line blank), so a cleared sign or
    /// board need not be stored or persisted.
    pub fn is_blank(&self) -> bool {
        let blank_note = |n: &Vec<String>| n.iter().all(|l| l.trim().is_empty());
        match self {
            BlockText::Sign(lines) => lines.iter().all(|l| l.trim().is_empty()),
            BlockText::Quest(notes) => notes.iter().all(blank_note),
        }
    }

    /// Whether this text belongs on the block currently at its cell: a
    /// [`Sign`](BlockText::Sign) only on a [`sign`](crate::block::SIGN), a
    /// [`Quest`](BlockText::Quest) only on a [`quest board`](crate::block::QUEST_BOARD).
    pub fn matches_block(&self, block: BlockId) -> bool {
        match self {
            BlockText::Sign(_) => crate::block::is_sign(block),
            BlockText::Quest(_) => crate::block::is_quest_board(block),
        }
    }
}

/// Maximum length (in characters) of a locked-chest password.
pub const PASSWORD_MAX_LEN: usize = 24;

/// A reference to one slot involved in a chest move: either a slot of the player's
/// own inventory or a slot of the open chest. Lets a single move message shuffle
/// items within the player's bag, within the chest, or between the two.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SlotRef {
    /// A slot of the player's own inventory (`0..TOTAL_SLOTS`).
    Player(u8),
    /// A slot of the currently-open chest (`0..CHEST_SLOTS`).
    Chest(u8),
}

/// Wire-protocol compatibility version. **Bump this on every incompatible change
/// to anything that crosses the wire** — adding/removing/reordering a
/// [`ClientMessage`] or [`ServerMessage`] variant, changing a variant's fields,
/// or altering a transported type like [`Entity`](crate::entity::Entity) or
/// [`Slot`].
///
/// Peers exchange this as a fixed 4-byte header before any bincode (see
/// [`crate::net::read_version`]), so a version-skewed client is rejected with a
/// clear "version mismatch" message instead of the cryptic bincode
/// `invalid value: integer N, expected variant index 0 <= i < K`
/// deserialization error that a mis-aligned enum tag produces.
pub const PROTOCOL_VERSION: u32 = 26;

/// ALPN protocol identifier negotiated during the QUIC/TLS handshake. The
/// trailing number is a coarse guard bumped only for changes deep enough to
/// affect the version handshake itself; ordinary wire changes are covered by
/// [`PROTOCOL_VERSION`]. Bumping it from `/0` to `/1` here also cleanly severs
/// this build from the older `/0` binaries that predate the handshake, so they
/// can no longer connect and reproduce the bug.
pub const ALPN: &[u8] = b"survival-cubed/1";

/// Sent from client to server over the single bidirectional stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// First message after the stream opens. The server authenticates this
    /// before admitting the player: `name` must not already be in use by another
    /// connected player, and `password` either registers a brand-new account (on
    /// first join under this name) or must match the one stored for an existing
    /// account. A failed check closes the connection with an explanatory reason.
    ///
    /// `creator_token` is the per-server admin secret: present (and matching) only
    /// for the client that created/hosted the server, which authorizes that
    /// connection as the server admin (admin commands, and creator mode even on a
    /// survival server). Remote joiners send `None` and are never admins.
    Hello {
        name: String,
        password: String,
        creator_token: Option<u64>,
    },
    /// Ask the server for the contents of a chunk in dimension `dim`. The server
    /// answers from whichever dimension the player currently occupies; `dim` lets a
    /// late reply that arrives after a dimension change be discarded by the client.
    RequestChunk { dim: Dimension, cx: i32, cy: i32 },
    /// Break the block at a world cell (it drops on the ground to be collected).
    /// `held` is the item the player is wielding ([`crate::block::AIR`] for bare
    /// hands); the server uses it to decide whether the broken block drops (e.g.
    /// stone needs a pickaxe).
    SetBlock {
        x: i32,
        y: i32,
        block: BlockId,
        held: BlockId,
    },
    /// Place the block from hotbar `slot` at a world cell. The server reads the
    /// block from that slot and consumes one, so the client can't place blocks
    /// it doesn't hold.
    PlaceBlock { x: i32, y: i32, slot: u8 },
    /// Use the bucket in hotbar `slot` on world cell `(x, y)`. The server reads
    /// the slot: an empty [`bucket`](crate::block::BUCKET) scoops up a
    /// [`water`](crate::block::WATER) cell (becoming a water bucket), and a
    /// [`water_bucket`](crate::block::WATER_BUCKET) pours its water into an empty
    /// cell (becoming empty again). Validated against the player's reach.
    UseBucket { x: i32, y: i32, slot: u8 },
    /// Use the fire key held in hotbar `slot`: the server checks the slot really
    /// holds a [`fire_key`](crate::block::FIRE_KEY) and, if so, warps the player to
    /// the *other* dimension (overworld ↔ underworld), landing them at that
    /// dimension's surface in their current column. The key is reusable and is not
    /// consumed. A no-op (with a resync) if the slot no longer holds the key.
    UseFireKey { slot: u8 },
    /// Use the arena key held in hotbar `slot`: the server checks the slot really
    /// holds an [`arena_key`](crate::block::ARENA_KEY) and, if so, warps the player
    /// into the [`crate::world::Dimension::Arena`] (or, if they are already in the
    /// arena, back to where they entered from). The key is reusable and is not
    /// consumed. A no-op (with a resync) if the slot no longer holds the key.
    UseArenaKey { slot: u8 },
    /// Swing the door touching world cell `(x, y)` open or shut. A door spans two
    /// cells; the server flips both halves between their closed
    /// ([`crate::block::DOOR`]/[`crate::block::DOOR_TOP`]) and open
    /// ([`crate::block::DOOR_OPEN`]/[`crate::block::DOOR_OPEN_TOP`]) states.
    /// Validated against the player's reach; a no-op if `(x, y)` is not a door.
    ToggleDoor { x: i32, y: i32 },
    /// Move/merge/swap the stack in inventory slot `from` onto slot `to`.
    MoveItem { from: u8, to: u8 },
    /// Drop the contents of inventory `slot` onto the ground at the player's feet
    /// so it can be discarded or picked up by another player. `all` drops the
    /// whole stack; otherwise a single item is dropped. `dir` is the player's
    /// facing (`-1.0` left, `+1.0` right) used to toss the drop clear of them.
    /// The dropped item keeps a tool's durability.
    DropItem { slot: u8, all: bool, dir: f32 },
    /// Craft [`RECIPES`](crate::recipe::RECIPES)`[recipe]` once: the server
    /// checks the player holds all inputs, consumes them, and grants the outputs.
    Craft { recipe: u16 },
    /// Smelt [`SMELT_RECIPES`](crate::recipe::SMELT_RECIPES)`[recipe]` up to
    /// `count` times at a forge, burning `fuel` (wood, coal, or bark — see
    /// [`forge_fuel_units`](crate::block::forge_fuel_units)). The server validates
    /// the raw material plus a charge of that fuel per repetition and stops early
    /// when either runs out.
    Smelt {
        recipe: u16,
        count: u32,
        fuel: BlockId,
    },
    /// Repair one worn tool of type `item` at a forge: the server restores some
    /// durability (see [`crate::block::repair_step`]) in exchange for one unit of
    /// the tool's [`repair_material`](crate::block::repair_material).
    Repair { item: BlockId },
    /// Eat the food item in inventory `slot`: the server consumes one and adjusts
    /// the player's health by its [`food_heal`](crate::block::food_heal) amount
    /// (raw meat *costs* health). No-op if the slot doesn't hold food.
    Eat { slot: u8 },
    /// Feed one unit of `fuel` (wood, coal, or bark) to the campfire at world cell
    /// `(x, y)`, lighting it and extending its burn time. The server validates the
    /// cell is a campfire and the player holds the fuel.
    FuelCampfire { x: i32, y: i32, fuel: BlockId },
    /// Cook [`COOK_RECIPES`](crate::recipe::COOK_RECIPES)`[recipe]` up to `count`
    /// times on the campfire at world cell `(x, y)`. The server requires that
    /// campfire to be lit and validates the inputs per repetition.
    Cook {
        x: i32,
        y: i32,
        recipe: u16,
        count: u32,
    },
    /// Mark the campfire at world cell `(x, y)` as this player's respawn point, so
    /// a later death returns them here instead of world spawn. The server validates
    /// the cell is a campfire before recording it. Sent when the player opens a
    /// campfire's GUI (i.e. interacts with it).
    SetRespawn { x: i32, y: i32 },
    /// Add a personal waypoint at world pixel `(x, y)` (the player's current
    /// position), drawn with `color`. The server stores it per-player and echoes
    /// the full list back via [`ServerMessage::Waypoints`].
    AddWaypoint { x: f32, y: f32, color: [f32; 3] },
    /// Remove the personal waypoint nearest to world pixel `(x, y)`. The server
    /// resyncs the list via [`ServerMessage::Waypoints`].
    RemoveWaypoint { x: f32, y: f32 },
    /// Report the owning player entity's position (pixels, world space) and
    /// current velocity (pixels/s). The server rebroadcasts the velocity in the
    /// resulting [`ServerMessage::EntityMoved`] so remote clients can flip the
    /// avatar to the right facing (from `vx`'s sign) and play its walk cycle.
    PlayerMove { x: f32, y: f32, vx: f32, vy: f32 },
    /// Set whether this player is riding a boat. The server records it on the
    /// player entity and resyncs that entity to everyone (via
    /// [`ServerMessage::EntitySpawn`]) so remote clients draw the rider in their
    /// boat. Riding itself is simulated client-side; this only shares the pose.
    SetBoating { on: bool },
    /// Mount the tamed [`horse`](crate::entity::EntityKind::Horse) with this id
    /// (`Some`), or dismount whatever horse the player is currently riding
    /// (`None`). The server validates that the horse exists in the player's
    /// dimension, is tamed by this player, and is within reach before honoring a
    /// mount; on success it records the ride on the player entity and shares the
    /// pose with every client (via [`ServerMessage::EntityRiding`]) so the rider is
    /// drawn on the combined horse sprite and the horse is glued beneath them.
    SetRiding { horse: Option<EntityId> },
    /// Begin remotely piloting one's own summoned white-dragon steed (`Some(dragon_id)`)
    /// or stop (`None`). The white dragon lets its summoner reach into its mind, so a
    /// player on foot can drive it like a second body — walking, flying, and breathing
    /// fire on command — while their own avatar stands frozen. The server validates the
    /// dragon exists in the player's dimension, is their own steed, and is not currently
    /// being ridden, then records the link on the player entity and echoes it back via
    /// [`ServerMessage::EntityControlled`]. A controlled steed runs no AI of its own and
    /// cannot stray more than [`crate::server::WHITE_DRAGON_CONTROL_RANGE`] of its pilot
    /// (the limit of the telepathic bond).
    SetControlling { dragon: Option<EntityId> },
    /// Per-frame movement intent for the white-dragon steed the sender is piloting (see
    /// [`Self::SetControlling`]): `dx`/`dy` are each `-1.0`, `0.0`, or `1.0` (left/right
    /// and up/down). The server flies the steed accordingly on its next tick, clamped to
    /// terrain and to the pilot's telepathic range. A no-op unless the sender is in fact
    /// controlling one of their own steeds.
    ControlDragon { dx: f32, dy: f32 },
    /// Melee-attack another entity (e.g. a slime). The server validates range
    /// before applying damage. `held` is the item the player is wielding
    /// ([`crate::block::AIR`] for bare hands); the server uses it to scale the
    /// damage (swords hit hardest, pickaxes far less). See
    /// [`crate::block::attack_damage`].
    Attack { target: EntityId, held: BlockId },
    /// Report fall damage the client computed from its own landing. The server
    /// is authoritative over the resulting health.
    FallDamage { amount: i32 },
    /// Toggle this player's creator mode on or off. Only honored from a connection
    /// the server has authorized for creator mode (see [`Self::Hello`]). A creator
    /// is ignored by hostile creatures.
    SetCreator { on: bool },
    /// Creator: jump the world clock to normalized time of day `t` in `[0, 1)`.
    /// The server adjusts its authoritative clock and rebroadcasts the time.
    SetTime { t: f32 },
    /// Creator: advance the world clock by one full day. Unlike [`Self::SetTime`] (which
    /// only sets the time *within* a day) this moves total in-world time forward a whole
    /// day/night cycle, so long-running countdowns — like the five days before
    /// [`crate::entity::EntityKind::Twinscale`] appears — can be fast-forwarded for testing.
    AdvanceDay,
    /// Creator: spawn a creature of `kind` at world pixel `(x, y)`. Player kinds
    /// are ignored by the server.
    SpawnEntity { kind: EntityKind, x: f32, y: f32 },
    /// Creator: set the block at a world cell directly, with no inventory cost or
    /// adjacency requirement (used by creator mode's infinite-block placement).
    CreatorSetBlock { x: i32, y: i32, block: BlockId },
    /// Creator: set many cells at once (used when stamping a saved structure).
    /// Each entry is `(x, y, block)`; the server applies and rebroadcasts them.
    CreatorSetBlocks { cells: Vec<(i32, i32, BlockId)> },
    /// Creator: drop `count` of item `item` straight into the player's inventory
    /// (the item-giver UI). The server validates `item` is a real id and stacks it
    /// in, then resyncs the inventory.
    GiveItem { item: BlockId, count: u32 },
    /// Send a line of chat. The server attributes it to this connection's
    /// player name and rebroadcasts it to everyone (see [`ServerMessage::Chat`]).
    Chat { text: String },
    /// Write `text` onto the sign or quest board at world cell `(x, y)`. The
    /// server validates the cell holds the matching block type and is within reach,
    /// clamps the text to the line/row/note limits, stores it, and rebroadcasts it
    /// via [`ServerMessage::BlockText`]. A blank write clears the cell's text.
    WriteBlockText { x: i32, y: i32, text: BlockText },
    /// Ask to open the chest at world cell `(x, y)`. `password` is sent for a
    /// locked chest (ignored for a plain one). The server validates the block, the
    /// player's reach, and — for a locked chest — the password (or a standing
    /// session unlock). It replies with [`ServerMessage::ChestContents`] on success
    /// or [`ServerMessage::ChestLocked`] when the password is missing or wrong.
    OpenChest {
        x: i32,
        y: i32,
        password: Option<String>,
    },
    /// Stop viewing whatever chest this player has open, so the server no longer
    /// streams that chest's content updates to them.
    CloseChest,
    /// Move a stack between the open chest at `(x, y)` and/or the player's own
    /// inventory (see [`SlotRef`]). The server validates the player is viewing that
    /// chest and applies the same merge/swap/relocate rules as an inventory move.
    MoveChestItem {
        x: i32,
        y: i32,
        from: SlotRef,
        to: SlotRef,
    },
    /// Reinforce the plain chest at `(x, y)` into a locked chest sealed with
    /// `password`. The server checks the cell is a chest, the player is in reach and
    /// holds enough [`gold`](crate::block::GOLD_INGOT), then consumes the gold,
    /// converts the block, and records the password. The chest keeps its contents.
    ReinforceChest { x: i32, y: i32, password: String },
    /// Cast the spellbook held in hotbar `slot` toward world pixel `(tx, ty)` (the
    /// player's cursor). The server checks the slot really holds a
    /// [`spellbook`](crate::block::is_spellbook) and that the player has at least its
    /// [`mana cost`](crate::block::spell_mana_cost); on success it spends that mana
    /// and looses the spell's effect (for the summoner spell, a friendly summoner
    /// fireball aimed at the cursor). The book is reusable and is never consumed. A
    /// no-op (with a mana resync) if the slot doesn't hold a spellbook or mana is short.
    CastSpell { slot: u8, tx: f32, ty: f32 },
    /// While riding a white-dragon steed, breathe a fireball toward world pixel
    /// `(tx, ty)` (the player's cursor). The server checks the sender really is riding
    /// one of their own [white dragons](crate::entity::EntityKind::WhiteDragon) and that
    /// its breath is off cooldown; on success it looses a friendly dragon fireball from
    /// the steed's maw at the cursor (damaging monsters where it strikes). A no-op
    /// otherwise — it costs no mana, only the steed's own breath cadence.
    DragonBreath { tx: f32, ty: f32 },
    /// Fire the musket held in hotbar `slot` toward world pixel `(tx, ty)` (the
    /// player's cursor). The server checks the slot really holds a
    /// [`musket`](crate::block::is_musket) and that the player carries at least one
    /// [`bullet`](crate::block::BULLET); on success it spends one bullet and looses a
    /// friendly bullet from the musket's muzzle at the cursor (damaging monsters where
    /// it strikes). A no-op (with an inventory resync) if the slot doesn't hold a musket
    /// or the player is out of bullets.
    FireMusket { slot: u8, tx: f32, ty: f32 },
    /// Spit a fireball from an empty hand toward world pixel `(tx, ty)` (the player's
    /// cursor) — the offensive half of the [dragon plate](crate::block::DRAGON_PLATE_SPELL)
    /// ward. The server checks the sender's ward is active and its breath cadence is
    /// ready; on success it looses a friendly dragon fireball from the player at the
    /// cursor (damaging monsters where it strikes). Costs no mana. The client cannot see
    /// the server-only ward, so it always sends this on an empty-handed use-click and the
    /// server simply ignores it when the player is unwarded or still on cadence.
    EmptyHandBreath { tx: f32, ty: f32 },
}

/// Sent from server to client over the single bidirectional stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Response to `Hello`: identifies the client's own player entity and its
    /// spawn position (pixels).
    Welcome {
        entity_id: EntityId,
        spawn_x: f32,
        spawn_y: f32,
        /// Whether this connection may enter creator mode: always true for the
        /// admin (host), and true for everyone on a creator-type server.
        creator_allowed: bool,
        /// The optional voice-chat relay, when the server owner enabled it (see
        /// [`crate::voice`]). `None` means voice is off: the client shows no voice
        /// UI and never opens a relay connection.
        voice: Option<crate::voice::VoiceInfo>,
        /// The optional webcam-video relay, when the server owner enabled it. It is
        /// a separate toggle from `voice` but shares the same relay endpoint, so
        /// when both are on this carries the *same* port/cert as `voice`. `None`
        /// means webcam is off: no webcam UI and no relay connection for video.
        webcam: Option<crate::voice::VoiceInfo>,
    },
    /// Full contents of a chunk (row-major, `CHUNK_AREA` entries) in dimension
    /// `dim`. The client ignores chunks for a dimension it is no longer in.
    Chunk {
        dim: Dimension,
        cx: i32,
        cy: i32,
        blocks: Vec<BlockId>,
    },
    /// A single block changed somewhere in dimension `dim`. The client ignores
    /// updates for a dimension it is not currently in.
    BlockUpdate {
        dim: Dimension,
        x: i32,
        y: i32,
        block: BlockId,
    },
    /// Many cells changed at once in dimension `dim` (a stamped structure). The
    /// client applies each `(x, y, block)` it can, ignoring other dimensions.
    BlocksUpdate {
        dim: Dimension,
        cells: Vec<(i32, i32, BlockId)>,
    },
    /// Move the owning client into dimension `dim` at world pixel `(x, y)`: it
    /// clears its mirrored world and entities, switches dimension, and repositions
    /// its avatar. Sent when the player falls into the underworld or climbs back to
    /// the overworld (see [`crate::server`]'s dimension transitions).
    EnterDimension { dim: Dimension, x: f32, y: f32 },
    /// An entity appeared (or its full description is being (re)sent). The
    /// client never receives a spawn for its own player entity.
    EntitySpawn { entity: Entity },
    /// Lightweight position/velocity update for an existing entity.
    EntityMoved {
        id: EntityId,
        x: f32,
        y: f32,
        vx: f32,
        vy: f32,
    },
    /// An entity was removed from the world.
    EntityDespawn { id: EntityId },
    /// A player climbed into a boat (`on = true`) or stepped back out (`on =
    /// false`). Every other client draws (or stows) that player's boat. Riding is
    /// simulated on the rider's own client; this only shares the pose, so it carries
    /// no position. Sent on each toggle, and once per already-riding player when a
    /// client first receives that player's [`ServerMessage::EntitySpawn`] snapshot.
    EntityBoating { id: EntityId, on: bool },
    /// A player mounted a tamed horse (`horse = Some(horse_id)`) or dismounted
    /// (`horse = None`). Every client draws that player on the combined
    /// `player/horse` sprite while mounted and hides the now-ridden horse entity
    /// (which the server keeps glued beneath the rider). Riding is driven by the
    /// rider's own client; this shares the pose. Sent on each mount/dismount, and
    /// once per already-mounted player when a client first receives that player's
    /// [`ServerMessage::EntitySpawn`] snapshot. `id` is the rider's entity id.
    EntityRiding {
        id: EntityId,
        horse: Option<EntityId>,
    },
    /// A player began remotely piloting their white-dragon steed (`controller =
    /// Some(player_id)`) or stopped (`controller = None`). `id` is the steed's entity
    /// id. The piloting client adopts this as the authoritative "am I controlling?"
    /// state (its steed then flies from the player's input, server-authoritatively via
    /// [`ServerMessage::EntityMoved`]); other clients need do nothing with it. Sent in
    /// response to a [`ClientMessage::SetControlling`] request that passed validation.
    EntityControlled {
        id: EntityId,
        controller: Option<EntityId>,
    },
    /// A zombie has been caught by daylight and begun its death animation. The
    /// client plays the crumble animation for [`crate::entity::ZOMBIE_DEATH_TIME`]
    /// seconds; an [`ServerMessage::EntityDespawn`] for the same id follows once
    /// it finishes.
    EntityDying { id: EntityId },
    /// An entity has begun a telegraphed wind-up melee attack — a snake's lunge or
    /// an orc's slam. Every client plays that kind's strike animation for its
    /// attack duration ([`crate::entity::SNAKE_LUNGE_TIME`] /
    /// [`crate::entity::ORC_SLAM_TIME`]); the attacker's motion and the blow itself
    /// arrive as ordinary [`ServerMessage::EntityMoved`]/[`ServerMessage::EntityHit`]
    /// updates.
    EntityLunging { id: EntityId },
    /// An entity's health changed (damage, healing, or an initial value). Sent
    /// to every client, including the owner of a player entity (whose avatar is
    /// otherwise never mirrored).
    EntityHealth {
        id: EntityId,
        health: i32,
        max_health: i32,
    },
    /// An entity just took a hit. Every client flashes that entity red; the
    /// owning client of a player avatar also applies the knockback velocity
    /// `(vx, vy)` (px/s) to its locally-simulated motion. Server-simulated
    /// creatures are already knocked back on the server, so for them the
    /// velocity is informational only.
    EntityHit { id: EntityId, vx: f32, vy: f32 },
    /// Current normalized time of day in `[0, 1)` (see [`crate::daylight`]).
    /// Broadcast periodically; clients advance it locally in between.
    TimeOfDay { t: f32 },
    /// Instruct the owning client to move its player avatar back to a spawn
    /// point. Health is restored via a separate `EntityHealth`. `died` is `true`
    /// when this is a death respawn (the client drops a "last death" waypoint at
    /// the spot it was standing) and `false` for a reconnect teleport to the
    /// player's saved position.
    Respawn { x: f32, y: f32, died: bool },
    /// Authoritative snapshot of the owning player's personal waypoints plus the
    /// current home (respawn) point — the dimension it lives in and its world
    /// pixels. Sent on join and after any waypoint or respawn-point change. Only
    /// ever sent to the list's owner. The client only draws the home marker while
    /// in its dimension, so it doesn't haunt the other planes.
    Waypoints {
        list: Vec<Waypoint>,
        home: (Dimension, f32, f32),
    },
    /// Authoritative snapshot of the owning player's inventory slots (hotbar
    /// first, then storage). Sent on join and after any change (pickup,
    /// placement, slot move). Only ever sent to the inventory's owner.
    Inventory { slots: Vec<Slot> },
    /// The owning player's current `mana` and its `max`. Mana is the magic resource
    /// won by slaying monsters and spent casting spellbooks (see
    /// [`crate::block::SUMMONER_SPELL`]). Sent on join and after any change (a kill
    /// reward or a cast). Only ever sent to the player it belongs to.
    Mana { mana: i32, max: i32 },
    /// A chat line to display, attributed to player `from`. Broadcast to every
    /// client (including the original sender, so they see their own message).
    /// Admin command feedback and ban announcements arrive on this same channel,
    /// attributed to a `Server` pseudo-sender.
    Chat { from: String, text: String },
    /// The player-written text on the sign or quest board at world cell `(x, y)`
    /// in dimension `dim`. Sent when a chunk holding a written text block loads, and
    /// whenever the text changes. A cleared block sends a blank
    /// [`BlockText`]. The client ignores updates for a dimension it is not in.
    BlockText {
        dim: Dimension,
        x: i32,
        y: i32,
        text: BlockText,
    },
    /// The contents of the chest at world cell `(x, y)` — its
    /// [`CHEST_SLOTS`](crate::inventory::CHEST_SLOTS) slots. Sent to a player when
    /// they successfully open a chest (the client then shows the chest window) and
    /// again to every viewer whenever its contents change.
    ChestContents { x: i32, y: i32, slots: Vec<Slot> },
    /// The locked chest at `(x, y)` refused to open: the password was missing or
    /// wrong. The client prompts for (or re-prompts for) the password.
    ChestLocked { x: i32, y: i32 },
    /// Begin or end spectating another player. `Some(id)` locks the receiving
    /// (admin) client's camera onto the entity with that id — which the server has
    /// already moved the admin alongside so it streams in — and freezes the admin's
    /// own avatar; `None` releases the camera back to the admin's avatar. Only ever
    /// sent to an admin who issued `/spectate`. See [`crate::server`].
    Spectate { target: Option<EntityId> },
}
