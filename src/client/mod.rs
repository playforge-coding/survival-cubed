//! Client application: window + event loop (winit), egui UI, input, player
//! physics, and the bridge to the networking thread.

mod audio;
mod net;
mod render;
mod screenshot;
mod sprite;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use glam::Vec2;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::block::{AIR, BlockRegistry, CHARRED_ROCK, DIRT, FIRE, GRASS, LEAVES, LOG, STONE};
use crate::daylight;
use crate::discovery::{DiscoveredServer, LanBrowser};
use crate::entity::{Entities, EntityId, EntityKind, PLAYER_MAX_HEALTH};
use crate::inventory::{HOTBAR_SLOTS, Inventory, STORAGE_SLOTS, Slot};
use crate::net::Credentials;
use crate::protocol::{BlockId, Waypoint};
use crate::server::{self, RunningServer};
use crate::structure::Structure;
use crate::world::{CHUNK_SIZE, Dimension, TILE_SIZE, WORLD_HEIGHT, World, to_chunk};

/// Which creator-mode interaction the left/right mouse buttons drive.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CreatorTool {
    /// Mine (LMB) and place the infinite block (RMB), as in normal creator play.
    Build,
    /// Drag out a rectangular region to capture as a structure.
    Select,
}

use net::{NetCommand, NetEvent, NetHandle, connect};
use render::{Atlas, CameraUniform, CapturedFrame, EguiFrame, Gfx, TileInstance, UvRect};

// --- Tunables ------------------------------------------------------------

const GRAVITY: f32 = 1400.0;
const MOVE_SPEED: f32 = 150.0;
const JUMP_VELOCITY: f32 = -440.0;
/// Base vertical speed (px/s) of the player while flying in creator mode, before
/// the scroll-wheel speed multiplier is applied.
const FLY_SPEED: f32 = 240.0;
/// Bounds on the creator fly-speed multiplier adjusted with the scroll wheel.
const FLY_MULT_MIN: f32 = 1.0;
const FLY_MULT_MAX: f32 = 8.0;
/// How much one scroll-wheel notch changes the creator fly-speed multiplier.
const FLY_MULT_STEP: f32 = 0.5;
/// Vertical speed (px/s) of the player while climbing a ladder.
const CLIMB_SPEED: f32 = 90.0;
/// Vertical speed (px/s) of the player paddling up or diving down in water. Kept
/// gentle so a swimmer can't launch themselves up out of the surface and skip
/// along the top — escaping water means wading to a shallow shore, not skimming.
const SWIM_SPEED: f32 = 55.0;
/// Rate (px/s) at which an idle swimmer sinks: the player is denser than water and
/// steadily goes under unless actively paddling up. Crossing open water on foot is
/// a slow, sinking slog — that's what a boat is for.
const WATER_SINK_SPEED: f32 = 110.0;
/// Reduced gravity (px/s²) felt while submerged, easing the vertical speed toward
/// a steady sink instead of a free fall.
const WATER_GRAVITY: f32 = 320.0;
/// Fraction of horizontal speed kept while swimming — water drags movement hard
/// (down to a slow crawl), so wading across a lake is far slower than riding a boat
/// over it.
const WATER_DRAG: f32 = 0.22;
/// How far (px) above the water a swimmer still counts as "in water". Bobbing at
/// the surface stays a slow swim rather than briefly popping into open air at full
/// land speed, which is what let players skim across the top.
const WATER_SURFACE_CLING: f32 = 8.0;
/// How far (px) a swimmer's head may poke above the waterline. A swimmer treads
/// water here and can rise no further — they can't launch out of the surface and
/// skim across open water; to leave the water they wade to a shallow shore.
const SWIM_SURFACE_POKE: f32 = 4.0;
/// Multiplier on walking speed while boating *on water*: a boat glides across the
/// surface noticeably faster than walking, and far faster than swimming.
const BOAT_SPEED_MULT: f32 = 1.45;
/// Multiplier on walking speed while boating *on land*: a beached boat is dead
/// weight you shuffle along slowly until you reach water — never a fast land ride.
const BOAT_LAND_DRAG: f32 = 0.45;
/// How firmly a boat is pulled toward its floating waterline each second: the
/// proportional gain easing the rider's vertical speed to settle on the surface.
const BOAT_FLOAT_STIFFNESS: f32 = 9.0;
/// Cap (px/s) on the vertical speed of that settling, so a boat dropped into deep
/// water rises to the surface briskly without snapping.
const BOAT_FLOAT_MAX_SPEED: f32 = 220.0;
/// Multiplier on walking speed while riding a horse: a gallop carries the rider
/// noticeably faster than they can run on foot. Gravity and jumping are otherwise
/// unchanged — a mounted player still arcs and lands like normal.
const HORSE_RIDE_SPEED_MULT: f32 = 1.7;
// The local player is just a (special) entity; reuse its shared size.
const PLAYER_W: f32 = crate::entity::PLAYER_SIZE.0;
const PLAYER_H: f32 = crate::entity::PLAYER_SIZE.1;
const ZOOM: f32 = 3.0;
/// How often (seconds) a held mouse button places a block or swings a melee hit.
const ACTION_COOLDOWN: f32 = 0.12;
/// Extra chunks loaded beyond the screen edges.
const CHUNK_MARGIN: i32 = 1;
/// Falls shorter than this (in tiles) are harmless.
const SAFE_FALL_TILES: f32 = 10.0;
/// Hit points lost per tile fallen beyond [`SAFE_FALL_TILES`].
const FALL_DAMAGE_PER_TILE: f32 = 1.0;
/// Max gap (px between AABBs) at which the player can melee a creature.
const PLAYER_ATTACK_REACH: f32 = 80.0;
/// Seconds an entity tints red after taking a hit.
const HIT_FLASH_TIME: f32 = 0.25;
/// Exponential decay rate (per second) of the player's horizontal knockback, so
/// a shove fades over roughly a quarter second.
const KNOCKBACK_DAMP: f32 = 9.0;
/// How close (world px) the player must get to the death marker before it clears.
const WAYPOINT_REACHED_DIST: f32 = 24.0;

/// Validate a user-entered world name, returning the trimmed name if it is safe
/// to use as a directory. Restricting to letters, numbers, spaces, '-' and '_'
/// keeps names readable and rules out path separators and traversal.
fn sanitize_world_name(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if s.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
    {
        Some(s.to_string())
    } else {
        None
    }
}

/// Resolve an item-giver query to a [`BlockId`]. The query is either a numeric
/// id (`"22"`) or a registry name (`"cooked_meat"`, also accepting spaces or
/// dashes in place of underscores, case-insensitively). Returns `None` for an
/// empty, out-of-range, or unknown query, and never resolves to air.
fn resolve_item(registry: &BlockRegistry, query: &str) -> Option<BlockId> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if let Ok(id) = q.parse::<BlockId>() {
        return ((id as usize) < registry.len() && id != AIR).then_some(id);
    }
    let norm = q.to_ascii_lowercase().replace([' ', '-'], "_");
    registry
        .iter()
        .find(|d| d.id != AIR && d.name == norm)
        .map(|d| d.id)
}

/// Entry point: build the app and run the winit event loop.
pub fn run() -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[derive(PartialEq)]
enum Screen {
    Menu,
    Connecting,
    InGame,
}

struct PendingTofu {
    host: String,
    fingerprint: String,
    respond: crossbeam_channel::Sender<bool>,
}

#[derive(Default)]
struct Input {
    left: bool,
    right: bool,
    jump: bool,
    /// Held while descending in creator-mode flight (S / ↓). Ignored on the ground.
    down: bool,
    mouse: (f32, f32),
    breaking: bool,
    placing: bool,
}

struct GameState {
    /// Id of this client's own player entity. That entity is the "special" one:
    /// it is simulated locally (below) rather than mirrored from the server, so
    /// it is deliberately *not* stored in `entities`.
    entity_id: EntityId,
    pos: Vec2,
    vel: Vec2,
    on_ground: bool,
    /// Which dimension the player is currently in. Chosen by the server (which
    /// owns transitions); the client mirrors it so it requests and ignores chunks
    /// and block updates for the right dimension, and tints the underworld dark.
    dim: Dimension,
    world: World,
    /// All other entities (remote players and server creatures), mirrored from
    /// the server.
    entities: Entities,
    /// Facing per remote entity (`true` = right), remembered while it is idle.
    facing: HashMap<EntityId, bool>,
    /// Facing of this client's own player avatar.
    player_facing: bool,
    /// Whether the player is currently riding a boat. A client-side movement mode
    /// toggled by right-clicking a held boat: while on, the player glides across
    /// water on the surface instead of swimming (see [`step_physics`]).
    boating: bool,
    /// The tamed horse this player is currently riding, if any (its entity id).
    /// Set authoritatively from the server's [`NetEvent::EntityRiding`] echo after
    /// right-clicking a tamed horse to mount; while `Some`, the player gallops faster
    /// (see [`step_physics`]) and is drawn on the combined `player/horse` sprite with
    /// the mounted horse entity hidden. Right-clicking again dismounts.
    riding: Option<EntityId>,
    requested: HashSet<(i32, i32)>,
    /// Index of the selected hotbar slot (`0..HOTBAR_SLOTS`); its block is the
    /// one placed on right-click.
    selected_slot: usize,
    /// Slot inventory, authoritative on the server and mirrored here. Placing a
    /// block requires (and spends) one from the selected hotbar slot.
    inventory: Inventory,
    /// Whether the full inventory management screen is open.
    inventory_open: bool,
    /// Slot picked as the source of a pending move on the inventory screen.
    move_from: Option<usize>,
    /// Whether the forge smelting GUI is open, and which forge cell it belongs
    /// to (opened by right-clicking a forge block).
    forge_open: bool,
    forge_cell: Option<(i32, i32)>,
    /// Which fuel the forge GUI currently burns per smelt (wood, coal, or bark).
    forge_fuel: BlockId,
    /// Whether the campfire GUI is open, and which campfire cell it belongs to
    /// (opened by right-clicking a campfire block).
    campfire_open: bool,
    campfire_cell: Option<(i32, i32)>,
    action_timer: f32,
    move_send_timer: f32,
    last_sent: Vec2,
    /// Local player health, authoritative on the server but mirrored here for
    /// the HUD.
    health: i32,
    max_health: i32,
    /// Normalized time of day in `[0, 1)`; advanced locally and corrected by the
    /// server (see [`crate::daylight`]).
    time_of_day: f32,
    /// Cell currently being mined and how long it has been held, driving the
    /// breaking delay and its overlay.
    break_target: Option<(i32, i32)>,
    break_progress: f32,
    /// Highest point (smallest `y`) reached since leaving the ground, used to
    /// measure fall distance for fall damage.
    air_min_y: f32,
    /// Seconds left on the local player's red "just got hit" flash (counted down
    /// each frame). Remote entities track this on their own [`Entity::hit_flash`].
    hit_flash: f32,
    /// Horizontal knockback velocity (px/s) added on top of input-driven motion
    /// and decayed each frame. Kept separate because [`step_physics`] recomputes
    /// `vel.x` from input every step, which would otherwise wipe out the shove.
    knockback_x: f32,
    /// Whether the server permits this client to enter creator mode (true for the
    /// admin/host, and for everyone on a creator-type server). Gates whether the
    /// creator-tools window is offered at all.
    creator_allowed: bool,
    /// Whether creator mode is currently active. Gates the creator abilities
    /// (flight, infinite blocks) and exempts the player from monster attacks.
    creator: bool,
    /// Creator mode: whether the player is flying (gravity off, free vertical move).
    fly: bool,
    /// Creator mode: scroll-wheel speed multiplier applied to flight movement.
    fly_speed_mult: f32,
    /// Creator mode: the block placed for free by infinite-block placement (RMB).
    creator_block: BlockId,
    /// Creator mode: which tool the mouse buttons drive (build vs select).
    creator_tool: CreatorTool,
    /// Creator mode: the two world-cell corners of the in-progress structure
    /// selection (the second tracks the cursor while the drag is held).
    sel_a: Option<(i32, i32)>,
    sel_b: Option<(i32, i32)>,
    /// Previous-frame mouse button states, for rising-edge detection in the
    /// creator selection/paste tools (where a click should fire once, not repeat).
    sel_prev_lmb: bool,
    sel_prev_rmb: bool,
    /// Creator mode: a loaded structure awaiting placement. While set, the next
    /// left-click stamps it (top-left at the cursor); right-click cancels.
    pending_paste: Option<Structure>,
    /// When spectating another player (admin `/spectate`), the id of the entity
    /// being watched: the camera follows it and the local avatar is frozen until
    /// it clears (a second `/spectate`, or the target leaving). `None` in normal
    /// play. Set by [`ServerMessage::Spectate`](crate::protocol::ServerMessage).
    spectating: Option<EntityId>,
    /// Received chat lines, oldest first, capped at [`MAX_CHAT_LOG`].
    chat_log: Vec<ChatLine>,
    /// Whether the chat input box is open (capturing keyboard input).
    chat_open: bool,
    /// Current chat input text, preserved across frames while typing.
    chat_input: String,
    /// Set when chat opens so the input box grabs keyboard focus next frame.
    chat_focus: bool,
    /// Personal waypoints this player has dropped, mirrored from the server (which
    /// persists them). Each carries the stable random colour drawn for its dot.
    waypoints: Vec<Waypoint>,
    /// Home (respawn) waypoint in world pixels — the player's last campfire, or
    /// world spawn before they've used one. Mirrored from the server.
    home: Option<Vec2>,
    /// Where the player last died, in world pixels. Shown as a death marker until
    /// they walk back to it (then it clears). Derived locally from death respawns.
    death: Option<Vec2>,
}

/// One received chat line: who said it and what they said.
struct ChatLine {
    from: String,
    text: String,
}

impl GameState {
    fn new(entity_id: EntityId, spawn: Vec2) -> Self {
        GameState {
            entity_id,
            pos: spawn,
            vel: Vec2::ZERO,
            on_ground: false,
            dim: Dimension::Overworld,
            world: World::new(),
            entities: Entities::new(),
            facing: HashMap::new(),
            player_facing: true,
            boating: false,
            riding: None,
            requested: HashSet::new(),
            selected_slot: 0,
            inventory: Inventory::new(),
            inventory_open: false,
            move_from: None,
            forge_open: false,
            forge_cell: None,
            forge_fuel: crate::block::WOOD,
            campfire_open: false,
            campfire_cell: None,
            action_timer: 0.0,
            move_send_timer: 0.0,
            last_sent: spawn,
            health: PLAYER_MAX_HEALTH,
            max_health: PLAYER_MAX_HEALTH,
            time_of_day: 0.0,
            break_target: None,
            break_progress: 0.0,
            air_min_y: spawn.y,
            hit_flash: 0.0,
            knockback_x: 0.0,
            creator_allowed: false,
            creator: false,
            fly: false,
            fly_speed_mult: FLY_MULT_MIN,
            creator_block: STONE,
            creator_tool: CreatorTool::Build,
            sel_a: None,
            sel_b: None,
            sel_prev_lmb: false,
            sel_prev_rmb: false,
            pending_paste: None,
            spectating: None,
            chat_log: Vec::new(),
            chat_open: false,
            chat_input: String::new(),
            chat_focus: false,
            waypoints: Vec::new(),
            // Until the server's first sync arrives, treat world spawn as home.
            home: Some(spawn),
            death: None,
        }
    }
}

/// Most chat lines kept in the scrollback before old ones are dropped.
const MAX_CHAT_LOG: usize = 100;
/// How many of the most recent chat lines are drawn in the overlay.
const CHAT_VISIBLE_LINES: usize = 8;
/// Longest chat line the input box accepts, matching the server's relay cap.
const MAX_CHAT_INPUT_LEN: usize = 256;
/// Entity sprite sheets exposed as `:name:` chat icons, alongside block/item
/// names. (Dropped-item and zombie-death sheets are intentionally excluded.)
const CHAT_ENTITY_ICONS: &[&str] = &["player", "slime", "chicken", "goat", "horse", "zombie"];

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,

    registry: Arc<BlockRegistry>,
    atlas: Atlas,
    /// The block/item texture atlas uploaded to egui, so inventory and hotbar
    /// slots can draw the real sprites. Built lazily on the first UI frame (it
    /// needs the egui context), then reused.
    block_tex: Option<egui::TextureHandle>,

    screen: Screen,
    status: String,
    /// Display name announced to servers; attributes this client's chat and keys
    /// its saved state on the server.
    name_input: String,
    /// Password typed in the menu to authenticate the chosen name. May be left
    /// blank when a password is already remembered for the target server (see
    /// [`Self::credentials`]).
    password_input: String,
    /// Saved passwords per `(server, name)`, so a returning player needn't retype
    /// one each time. Loaded once at startup and updated on a successful join.
    credentials: Credentials,
    /// Credential awaiting confirmation that the connection was accepted, as
    /// `(server label, name, password)`. Saved to [`Self::credentials`] on
    /// `Connected` and discarded if the connection is refused (e.g. wrong
    /// password), so only working passwords are ever remembered.
    pending_credential: Option<(String, String, String)>,
    address_input: String,
    port_input: String,
    /// Name typed in the "New world" form.
    world_name_input: String,
    /// Seed typed in the "New world" form; empty means pick one at random.
    seed_input: String,
    /// Whether launching a world should also host it on the LAN.
    host_enabled: bool,
    /// Whether the "New world" form should create a creator-type server (every
    /// player may enter creator mode) rather than a survival one. Only meaningful
    /// when creating a brand-new world; ignored when loading an existing save.
    host_creator_world: bool,
    /// Item id or name typed into the creator-tools item giver.
    give_item_input: String,
    /// How many of the item the creator-tools item giver hands over per click.
    give_item_count: u32,
    /// Name typed into the creator-tools "save structure" field.
    structure_name_input: String,
    /// Cached list of saved structure names shown in the creator tools, refreshed
    /// after a save/delete (and when creator mode is first entered) rather than
    /// re-reading the directory every frame.
    structure_list: Vec<String>,
    /// Last result line from a structure save/load/delete, shown in the creator
    /// tools window.
    structure_status: String,

    net: Option<NetHandle>,
    server: Option<RunningServer>,
    pending_tofu: Option<PendingTofu>,
    /// Name of a saved world awaiting delete confirmation, set when the menu's
    /// "Delete" button is clicked and cleared once confirmed or cancelled.
    pending_delete: Option<String>,
    game: Option<GameState>,
    /// Background mDNS browser feeding the menu's LAN server list, if discovery
    /// could be started.
    lan: Option<LanBrowser>,
    /// Background music player, if an audio device was available at startup.
    music: Option<audio::Music>,

    input: Input,
    last_frame: Instant,
    /// Seconds elapsed, used to drive sprite animation.
    anim_time: f32,
    /// Set when F2 is pressed; the next rendered frame is captured (without the
    /// HUD) and saved, then this is cleared.
    screenshot_requested: bool,
}

impl App {
    fn new() -> Self {
        let registry = Arc::new(BlockRegistry::new());
        let atlas = Atlas::build(&registry);
        App {
            window: None,
            gfx: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            registry,
            atlas,
            block_tex: None,
            screen: Screen::Menu,
            status: String::new(),
            name_input: "player".to_string(),
            password_input: String::new(),
            credentials: Credentials::load(),
            pending_credential: None,
            address_input: "127.0.0.1:5000".to_string(),
            port_input: "5000".to_string(),
            world_name_input: "world".to_string(),
            seed_input: String::new(),
            host_enabled: false,
            host_creator_world: false,
            give_item_input: String::new(),
            give_item_count: 1,
            structure_name_input: String::new(),
            structure_list: Vec::new(),
            structure_status: String::new(),
            net: None,
            server: None,
            pending_tofu: None,
            pending_delete: None,
            game: None,
            lan: match crate::discovery::browse() {
                Ok(b) => Some(b),
                Err(e) => {
                    log::warn!("LAN discovery unavailable: {e:#}");
                    None
                }
            },
            music: audio::Music::new(),
            input: Input::default(),
            last_frame: Instant::now(),
            anim_time: 0.0,
            screenshot_requested: false,
        }
    }

    // --- Session lifecycle ---

    fn seed() -> i32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i32)
            .unwrap_or(1337)
    }

    /// Turn the user's seed text into a generator seed. Empty input picks a
    /// time-based seed; a plain integer is used as-is; any other text is hashed
    /// (FNV-1a) so memorable word-seeds work like other block games.
    fn parse_seed(input: &str) -> i32 {
        let s = input.trim();
        if s.is_empty() {
            return Self::seed();
        }
        if let Ok(n) = s.parse::<i32>() {
            return n;
        }
        let mut hash: u32 = 0x811c_9dc5;
        for b in s.bytes() {
            hash ^= b as u32;
            hash = hash.wrapping_mul(0x0100_0193);
        }
        hash as i32
    }

    /// Create and launch a brand-new world from the "New world" form, validating
    /// the name and rejecting collisions with an existing save.
    fn create_world(&mut self) {
        let name = match sanitize_world_name(&self.world_name_input) {
            Some(n) => n,
            None => {
                self.status =
                    "World name must be letters, numbers, spaces, '-' or '_'.".to_string();
                return;
            }
        };
        if crate::save::world_exists(&name) {
            self.status = format!("A world named '{name}' already exists.");
            return;
        }
        let seed = Self::parse_seed(&self.seed_input);
        self.launch_world(name, seed);
    }

    /// Launch a world locally, or host it on the LAN if the "Host on LAN" toggle
    /// is set. For an existing world the saved seed wins; `seed` only applies to
    /// a fresh one.
    fn launch_world(&mut self, name: String, seed: i32) {
        if self.host_enabled {
            let port: u16 = self.port_input.trim().parse().unwrap_or(5000);
            self.start_host_world(name, seed, port);
        } else {
            self.start_world(name, seed);
        }
    }

    /// Start an embedded server for a singleplayer world and connect to it.
    fn start_world(&mut self, name: String, seed: i32) {
        let Some(password) = self.resolve_password(&name) else {
            return;
        };
        let save_dir = crate::save::world_dir(&name);
        match server::start_server(
            server::local_bind(),
            seed,
            save_dir,
            self.host_creator_world,
        ) {
            Ok(srv) => {
                let handle = connect(
                    srv.addr,
                    name.clone(),
                    self.player_name(),
                    password,
                    Some(srv.fingerprint),
                    Some(srv.creator_token),
                );
                self.server = Some(srv);
                self.net = Some(handle);
                self.screen = Screen::Connecting;
                self.status = format!("Starting world '{name}'...");
            }
            Err(e) => self.status = format!("Failed to start server: {e:#}"),
        }
    }

    /// Start a LAN-advertised server for `name` on `port` and connect to it.
    fn start_host_world(&mut self, name: String, seed: i32, port: u16) {
        let Some(password) = self.resolve_password(&name) else {
            return;
        };
        let save_dir = crate::save::world_dir(&name);
        match server::start_server(
            server::host_bind(port),
            seed,
            save_dir,
            self.host_creator_world,
        ) {
            Ok(mut srv) => {
                srv.advertise(&format!("Survival Cubed: {name} :{port}"));
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                let handle = connect(
                    addr,
                    name.clone(),
                    self.player_name(),
                    password,
                    Some(srv.fingerprint),
                    Some(srv.creator_token),
                );
                self.server = Some(srv);
                self.net = Some(handle);
                self.screen = Screen::Connecting;
                self.status = format!("Hosting '{name}' on port {port}...");
            }
            Err(e) => self.status = format!("Failed to host: {e:#}"),
        }
    }

    /// The trimmed display name to announce, falling back to "player" when the
    /// field is left blank.
    fn player_name(&self) -> String {
        let n = self.name_input.trim();
        if n.is_empty() {
            "player".to_string()
        } else {
            n.to_string()
        }
    }

    /// Resolve the password to authenticate with `host`: the one typed in the
    /// menu, or — if that's blank — any previously saved for this `(host, name)`.
    /// Returns `None` (and shows a status message) when neither exists, since
    /// every server requires a password. On success the chosen credential is
    /// stashed in [`Self::pending_credential`] to be remembered once the server
    /// accepts the connection.
    fn resolve_password(&mut self, host: &str) -> Option<String> {
        let name = self.player_name();
        let typed = self.password_input.trim();
        let password = if !typed.is_empty() {
            typed.to_string()
        } else if let Some(saved) = self.credentials.get(host, &name) {
            saved.to_string()
        } else {
            self.status = "Enter a password to join.".to_string();
            return None;
        };
        self.pending_credential = Some((host.to_string(), name, password.clone()));
        Some(password)
    }

    fn start_connect(&mut self) {
        let label = self.address_input.trim().to_string();
        let addr = match label.parse::<std::net::SocketAddr>() {
            Ok(a) => a,
            Err(_) => {
                self.status = format!("Invalid address: {label}");
                return;
            }
        };
        let Some(password) = self.resolve_password(&label) else {
            return;
        };
        let handle = connect(
            addr,
            label.clone(),
            self.player_name(),
            password,
            None,
            None,
        );
        self.net = Some(handle);
        self.screen = Screen::Connecting;
        self.status = format!("Connecting to {label}...");
    }

    /// Join a server discovered on the LAN. Its advertised fingerprint is passed
    /// as a pre-trusted cert, so a LAN join needs no TOFU prompt.
    fn start_join_lan(&mut self, server: DiscoveredServer) {
        let label = server.addr.to_string();
        let Some(password) = self.resolve_password(&label) else {
            return;
        };
        let handle = connect(
            server.addr,
            label,
            self.player_name(),
            password,
            server.fingerprint,
            None,
        );
        self.net = Some(handle);
        self.screen = Screen::Connecting;
        self.status = format!("Connecting to {}...", server.name);
    }

    fn leave(&mut self) {
        if let Some(net) = &self.net {
            let _ = net.commands.send(NetCommand::Disconnect);
        }
        self.net = None;
        self.server = None; // dropping closes the embedded server
        self.game = None;
        self.pending_tofu = None;
        self.pending_credential = None;
        if let Some(m) = &mut self.music {
            m.stop();
        }
        self.input = Input::default();
        self.screen = Screen::Menu;
    }

    // --- Networking ---

    fn poll_net(&mut self) {
        let events: Vec<NetEvent> = match &self.net {
            Some(net) => net.events.try_iter().collect(),
            None => return,
        };
        for ev in events {
            self.handle_net_event(ev);
        }
    }

    fn handle_net_event(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::TofuPrompt {
                host,
                fingerprint,
                respond,
            } => {
                self.pending_tofu = Some(PendingTofu {
                    host,
                    fingerprint,
                    respond,
                });
            }
            NetEvent::Connected {
                entity_id,
                spawn_x,
                spawn_y,
                creator_allowed,
            } => {
                // The login was accepted: remember the password for this server so
                // it needn't be retyped next time.
                if let Some((host, name, password)) = self.pending_credential.take()
                    && !password.is_empty()
                    && let Err(e) = self.credentials.add_and_save(&host, &name, &password)
                {
                    log::warn!("could not save login: {e:#}");
                }
                let mut game = GameState::new(entity_id, Vec2::new(spawn_x, spawn_y));
                // Whether creator mode is offered is decided by the server: the
                // admin/host always may, and so may everyone on a creator server.
                // Creator mode itself starts off; the player toggles it in-game.
                game.creator_allowed = creator_allowed;
                let dim = game.dim;
                self.game = Some(game);
                self.screen = Screen::InGame;
                self.status.clear();
                // Players spawn in the overworld; start its music. A following
                // EnterDimension would switch it if the server moves them.
                if let Some(m) = &mut self.music {
                    m.play_for(dim);
                }
            }
            NetEvent::Chunk {
                dim,
                cx,
                cy,
                blocks,
            } => {
                if let Some(g) = &mut self.game {
                    // Drop chunks for a dimension we've since left (a reply that
                    // raced a transition).
                    if dim == g.dim {
                        g.world
                            .insert_chunk((cx, cy), crate::world::Chunk::from_vec(blocks));
                    }
                }
            }
            NetEvent::BlockUpdate { dim, x, y, block } => {
                if let Some(g) = &mut self.game {
                    if dim == g.dim {
                        g.world.set_block(x, y, block);
                    }
                }
            }
            NetEvent::BlocksUpdate { dim, cells } => {
                if let Some(g) = &mut self.game {
                    if dim == g.dim {
                        for (x, y, block) in cells {
                            g.world.set_block(x, y, block);
                        }
                    }
                }
            }
            NetEvent::EnterDimension { dim, x, y } => {
                if let Some(g) = &mut self.game {
                    // Switch dimensions: drop the old world and entities and start
                    // streaming the new one, then reposition the avatar.
                    g.dim = dim;
                    g.world = World::new();
                    g.entities = Entities::new();
                    g.facing.clear();
                    g.requested.clear();
                    g.break_target = None;
                    g.break_progress = 0.0;
                    // Close any block GUIs tied to a cell in the world we just left.
                    g.forge_open = false;
                    g.forge_cell = None;
                    g.campfire_open = false;
                    g.campfire_cell = None;
                    g.pos = Vec2::new(x, y);
                    g.vel = Vec2::ZERO;
                    g.knockback_x = 0.0;
                    g.air_min_y = y;
                    g.last_sent = g.pos;
                    // You always arrive in a new dimension on foot, never afloat or
                    // mounted; the server clears both rider states to match (see
                    // enter_dimension), leaving any horse behind in the old dimension.
                    g.boating = false;
                    g.riding = None;
                }
                // Swap to the new dimension's (randomly chosen) music.
                if let Some(m) = &mut self.music {
                    m.play_for(dim);
                }
            }
            NetEvent::EntitySpawn { entity } => {
                if let Some(g) = &mut self.game {
                    // The server never spawns us our own avatar, but guard anyway.
                    if entity.id != g.entity_id {
                        g.entities.insert(entity);
                    }
                }
            }
            NetEvent::EntityMoved { id, x, y, vx, vy } => {
                if let Some(g) = &mut self.game {
                    if id != g.entity_id {
                        if let Some(e) = g.entities.get_mut(id) {
                            e.x = x;
                            e.y = y;
                            e.vx = vx;
                            e.vy = vy;
                        }
                        if vx != 0.0 {
                            g.facing.insert(id, vx > 0.0);
                        }
                    }
                }
            }
            NetEvent::EntityBoating { id, on } => {
                if let Some(g) = &mut self.game
                    && id != g.entity_id
                    && let Some(e) = g.entities.get_mut(id)
                {
                    // Mirror a remote player's boat pose; their rendering picks the
                    // boat sprite from this flag (their own motion still arrives via
                    // EntityMoved).
                    e.boating = on;
                }
            }
            NetEvent::EntityRiding { id, horse } => {
                if let Some(g) = &mut self.game {
                    if id == g.entity_id {
                        // The server confirmed our own mount/dismount; adopt it as the
                        // authoritative local riding state.
                        g.riding = horse;
                    } else if let Some(e) = g.entities.get_mut(id) {
                        // Mirror a remote player's mount pose; their rendering draws
                        // them on the combined horse sprite from this, and the ridden
                        // horse entity is hidden (their own motion still arrives via
                        // EntityMoved).
                        e.riding = horse;
                    }
                }
            }
            NetEvent::EntityDespawn { id } => {
                if let Some(g) = &mut self.game {
                    // If the horse we were riding vanished, step off so we don't keep
                    // galloping on a ghost.
                    if g.riding == Some(id) {
                        g.riding = None;
                    }
                    g.entities.remove(id);
                    g.facing.remove(&id);
                    // If the player we were spectating just left, drop back to our
                    // own avatar rather than tracking a now-absent entity.
                    if g.spectating == Some(id) {
                        g.spectating = None;
                        g.chat_log.push(ChatLine {
                            from: "Server".to_string(),
                            text: "Spectated player left.".to_string(),
                        });
                    }
                }
            }
            NetEvent::EntityDying { id } => {
                if let Some(g) = &mut self.game {
                    if let Some(e) = g.entities.get_mut(id) {
                        // Kick off this kind's death animation (a zombie's daylight
                        // crumble, a snake's writhe); it plays out locally until the
                        // server's despawn for this id arrives.
                        if let Some(t) = e.kind.death_time() {
                            e.dying = t;
                            e.vx = 0.0;
                        }
                    }
                }
            }
            NetEvent::EntityLunging { id } => {
                if let Some(g) = &mut self.game {
                    if let Some(e) = g.entities.get_mut(id) {
                        // Kick off the strike animation; it plays out locally over
                        // the lunge while the server drives the snake's spring.
                        e.lunge = crate::entity::SNAKE_LUNGE_TIME;
                    }
                }
            }
            NetEvent::EntityHealth {
                id,
                health,
                max_health,
            } => {
                if let Some(g) = &mut self.game {
                    if id == g.entity_id {
                        g.health = health;
                        g.max_health = max_health;
                    } else if let Some(e) = g.entities.get_mut(id) {
                        e.health = health;
                        e.max_health = max_health;
                    }
                }
            }
            NetEvent::EntityHit { id, vx, vy } => {
                if let Some(g) = &mut self.game {
                    if id == g.entity_id {
                        // Our own avatar: the server can't move us, so apply the
                        // knockback to local motion and flash ourselves red. The
                        // horizontal shove rides on a separate decaying channel
                        // (vel.x is rewritten from input each step); the vertical
                        // pop can go straight onto vel.y, which gravity carries.
                        g.knockback_x += vx;
                        g.vel.y += vy;
                        g.hit_flash = HIT_FLASH_TIME;
                    } else if let Some(e) = g.entities.get_mut(id) {
                        // Remote entity: it's already being knocked back by the
                        // server (its EntityMoved follows), so just flash it.
                        e.hit_flash = HIT_FLASH_TIME;
                    }
                }
            }
            NetEvent::TimeOfDay { t } => {
                if let Some(g) = &mut self.game {
                    g.time_of_day = t;
                }
            }
            NetEvent::Respawn { x, y, died } => {
                if let Some(g) = &mut self.game {
                    // A death respawn drops a marker at the spot we fell, so we can
                    // find our way back to whatever killed (or dropped) us.
                    if died {
                        g.death = Some(g.pos);
                    }
                    g.pos = Vec2::new(x, y);
                    g.vel = Vec2::ZERO;
                    g.knockback_x = 0.0;
                    g.air_min_y = y;
                    g.last_sent = g.pos;
                }
            }
            NetEvent::Waypoints { list, home } => {
                if let Some(g) = &mut self.game {
                    g.waypoints = list;
                    g.home = Some(Vec2::new(home.0, home.1));
                }
            }
            NetEvent::Inventory { slots } => {
                if let Some(g) = &mut self.game {
                    g.inventory = Inventory::from_slots(slots);
                }
            }
            NetEvent::Chat { from, text } => {
                if let Some(g) = &mut self.game {
                    g.chat_log.push(ChatLine { from, text });
                    // Trim the oldest lines once the scrollback is full.
                    if g.chat_log.len() > MAX_CHAT_LOG {
                        let cut = g.chat_log.len() - MAX_CHAT_LOG;
                        g.chat_log.drain(..cut);
                    }
                }
            }
            NetEvent::Spectate { target } => {
                if let Some(g) = &mut self.game {
                    g.spectating = target;
                }
            }
            NetEvent::Disconnected { reason } => {
                self.status = format!("Disconnected: {reason}");
                self.net = None;
                self.server = None;
                self.game = None;
                if let Some(m) = &mut self.music {
                    m.stop();
                }
                self.pending_tofu = None;
                // The connection was refused or dropped: don't remember a password
                // that may have been rejected.
                self.pending_credential = None;
                self.screen = Screen::Menu;
            }
        }
    }

    // --- Per-frame update ---

    fn update(&mut self, dt: f32) {
        if self.game.is_none() {
            return;
        }
        let reg = &self.registry;
        let input = &self.input;
        let game = self.game.as_mut().unwrap();

        // Advance the day/night clock locally; the server corrects it via
        // TimeOfDay messages.
        game.time_of_day = (game.time_of_day + dt / daylight::DAY_LENGTH_SECS).rem_euclid(1.0);

        // Count down red hit-flash timers (own avatar and every remote entity).
        game.hit_flash = (game.hit_flash - dt).max(0.0);
        for e in game.entities.values_mut() {
            e.hit_flash = (e.hit_flash - dt).max(0.0);
            // Advance any in-progress death animation (zombies crumbling at dawn).
            if e.dying > 0.0 {
                e.dying = (e.dying - dt).max(0.0);
            }
            // Advance any in-progress snake strike animation.
            if e.lunge > 0.0 {
                e.lunge = (e.lunge - dt).max(0.0);
            }
        }

        // While spectating, the camera follows the watched player and our own
        // avatar is frozen: skip movement, world interaction and position updates,
        // but keep streaming chunks around the spectated view.
        if game.spectating.is_some() {
            request_chunks(game, self.gfx.as_ref(), self.net.as_ref());
            return;
        }

        // A boat that's been dropped, traded, or otherwise lost can no longer be
        // ridden — step out of it so the player doesn't glide on without one, and
        // tell the server so other clients stow our boat too.
        if game.boating && game.inventory.count(crate::block::BOAT) == 0 {
            game.boating = false;
            if let Some(net) = &self.net {
                let _ = net.commands.send(NetCommand::SetBoating { on: false });
            }
        }

        let fall_damage = step_physics(game, reg, input, dt);
        if let (Some(amount), Some(net)) = (fall_damage, &self.net) {
            let _ = net.commands.send(NetCommand::FallDamage { amount });
        }
        request_chunks(game, self.gfx.as_ref(), self.net.as_ref());
        handle_block_actions(game, reg, input, self.gfx.as_ref(), self.net.as_ref(), dt);

        // The death marker is a one-shot: it clears once the player gets back to
        // the spot they fell.
        if let Some(d) = game.death
            && game.pos.distance(d) < WAYPOINT_REACHED_DIST
        {
            game.death = None;
        }

        // Throttle position updates to the server.
        game.move_send_timer -= dt;
        if game.move_send_timer <= 0.0 && game.pos.distance(game.last_sent) > 0.25 {
            if let Some(net) = &self.net {
                let _ = net.commands.send(NetCommand::PlayerMove {
                    x: game.pos.x,
                    y: game.pos.y,
                });
            }
            game.last_sent = game.pos;
            game.move_send_timer = 0.05;
        }
    }

    // --- egui UI ---

    fn build_ui(&mut self, ui: &mut egui::Ui) {
        // Upload the block/item atlas to egui on the first frame so slots can
        // draw real sprites (the wgpu atlas can't be sampled from egui directly).
        if self.block_tex.is_none() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [self.atlas.width as usize, self.atlas.height as usize],
                &self.atlas.pixels,
            );
            self.block_tex = Some(ui.ctx().load_texture(
                "block_atlas",
                image,
                egui::TextureOptions::NEAREST,
            ));
        }

        // TOFU prompt takes priority and is shown as a modal-ish window.
        if self.pending_tofu.is_some() {
            self.tofu_window(ui);
        }

        // World delete confirmation, shown over the menu.
        if self.pending_delete.is_some() {
            self.delete_confirmation_window(ui);
        }

        match self.screen {
            Screen::Menu => self.menu_ui(ui),
            Screen::Connecting => {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(120.0);
                        ui.heading("Survival Cubed");
                        ui.add_space(20.0);
                        ui.label(&self.status);
                        ui.spinner();
                        if ui.button("Cancel").clicked() {
                            self.leave();
                        }
                    });
                });
            }
            Screen::InGame => {
                self.hud_ui(ui);
                // The HUD's "Leave" button drops `self.game` mid-frame and sends
                // us back to the menu; bail out instead of unwrapping a now-None
                // game in the panels below (which would crash the client).
                if self.game.is_none() {
                    return;
                }
                self.waypoint_overlay(ui);
                self.selection_overlay(ui);
                self.spectate_overlay(ui);
                self.chat_ui(ui);
                if self.game.as_ref().is_some_and(|g| g.inventory_open) {
                    self.inventory_window(ui);
                }
                if self.game.as_ref().is_some_and(|g| g.forge_open) {
                    self.forge_window(ui);
                }
                if self.game.as_ref().is_some_and(|g| g.campfire_open) {
                    self.campfire_window(ui);
                }
                if self.game.as_ref().is_some_and(|g| g.creator_allowed) {
                    self.creator_window(ui);
                }
            }
        }
    }

    fn menu_ui(&mut self, ui: &mut egui::Ui) {
        // Snapshot the LAN list and saved worlds up front so the closures below
        // can freely borrow `self` to launch a join or load.
        let lan_servers = self.lan.as_ref().map(|b| b.servers()).unwrap_or_default();
        let worlds = crate::save::list_worlds();
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.heading("Survival Cubed");
                ui.add_space(8.0);
                ui.label("A multiplayer-first 2D block game.");
                ui.add_space(24.0);

                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.name_input)
                                .desired_width(160.0)
                                .hint_text("player"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Password:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.password_input)
                                .desired_width(160.0)
                                .password(true)
                                .hint_text("required"),
                        );
                    });
                    ui.weak("Set on first join; remembered after that.");
                });
                ui.add_space(8.0);

                ui.group(|ui| {
                    ui.label("New world");
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.world_name_input)
                                .desired_width(160.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Seed:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.seed_input)
                                .desired_width(160.0)
                                .hint_text("random"),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Mode:");
                        ui.radio_value(&mut self.host_creator_world, false, "Survival");
                        ui.radio_value(&mut self.host_creator_world, true, "Creator");
                    });
                    if self.host_creator_world {
                        ui.weak("Everyone may enter creator mode.");
                    } else {
                        ui.weak("Only the host may enter creator mode.");
                    }
                    ui.checkbox(&mut self.host_enabled, "Host on LAN");
                    if self.host_enabled {
                        ui.horizontal(|ui| {
                            ui.label("Port:");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.port_input)
                                    .desired_width(80.0),
                            );
                        });
                    }
                    let create_label = if self.host_enabled {
                        "Create & host"
                    } else {
                        "Create world"
                    };
                    if ui.button(create_label).clicked() {
                        self.create_world();
                    }
                });
                ui.add_space(8.0);

                ui.group(|ui| {
                    ui.label("Your worlds");
                    if worlds.is_empty() {
                        ui.weak("No saved worlds yet.");
                    } else {
                        let play_label = if self.host_enabled { "Host" } else { "Play" };
                        for world in &worlds {
                            ui.horizontal(|ui| {
                                if ui.button(play_label).clicked() {
                                    self.launch_world(world.name.clone(), world.seed);
                                }
                                if ui.button("Delete").clicked() {
                                    self.pending_delete = Some(world.name.clone());
                                }
                                ui.label(format!("{}  (seed {})", world.name, world.seed));
                            });
                        }
                    }
                });
                ui.add_space(16.0);

                ui.group(|ui| {
                    ui.label("Join a server");
                    ui.horizontal(|ui| {
                        ui.label("Address:");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.address_input)
                                .desired_width(180.0),
                        );
                        if ui.button("Connect").clicked() {
                            self.start_connect();
                        }
                    });
                });
                ui.add_space(8.0);

                ui.group(|ui| {
                    ui.label("LAN games");
                    if self.lan.is_none() {
                        ui.weak("Discovery unavailable.");
                    } else if lan_servers.is_empty() {
                        ui.weak("Searching for nearby games...");
                    } else {
                        for server in &lan_servers {
                            ui.horizontal(|ui| {
                                if ui.button("Join").clicked() {
                                    self.start_join_lan(server.clone());
                                }
                                ui.label(format!("{}  ({})", server.name, server.addr));
                            });
                        }
                    }
                });

                ui.add_space(20.0);
                if !self.status.is_empty() {
                    ui.colored_label(egui::Color32::LIGHT_RED, &self.status);
                }
            });
        });
    }

    fn tofu_window(&mut self, ui: &mut egui::Ui) {
        let Some(tofu) = &self.pending_tofu else {
            return;
        };
        let host = tofu.host.clone();
        let fingerprint = tofu.fingerprint.clone();
        let mut decision: Option<bool> = None;

        egui::Window::new("Unknown server certificate")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "The authenticity of host '{host}' can't be established."
                ));
                ui.add_space(4.0);
                ui.label("SHA-256 certificate fingerprint:");
                ui.monospace(&fingerprint);
                ui.add_space(8.0);
                ui.label("Are you sure you want to continue connecting?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Accept & connect").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Decline").clicked() {
                        decision = Some(false);
                    }
                });
            });

        if let Some(accept) = decision {
            if let Some(tofu) = self.pending_tofu.take() {
                let _ = tofu.respond.send(accept);
            }
            if !accept {
                self.status = "Connection declined.".to_string();
            }
        }
    }

    fn delete_confirmation_window(&mut self, ui: &mut egui::Ui) {
        let Some(name) = self.pending_delete.clone() else {
            return;
        };
        // None = no decision yet, Some(true) = delete, Some(false) = cancel.
        let mut decision: Option<bool> = None;

        egui::Window::new("Delete world")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label(format!("Permanently delete the world '{name}'?"));
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::LIGHT_RED, "This cannot be undone.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Delete").clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });

        match decision {
            Some(true) => {
                self.pending_delete = None;
                match crate::save::delete_world(&name) {
                    Ok(()) => self.status = format!("Deleted world '{name}'."),
                    Err(e) => self.status = format!("Failed to delete '{name}': {e}"),
                }
            }
            Some(false) => self.pending_delete = None,
            None => {}
        }
    }

    fn hud_ui(&mut self, ui: &mut egui::Ui) {
        let (selected_slot, other_players, pos, health, max_health, time_of_day, hotbar) = {
            let g = self.game.as_ref().unwrap();
            let hotbar: Vec<Slot> = g.inventory.slots()[..HOTBAR_SLOTS].to_vec();
            (
                g.selected_slot,
                g.entities.player_count(),
                g.pos,
                g.health,
                g.max_health,
                g.time_of_day,
                hotbar,
            )
        };
        let night = daylight::is_night(time_of_day);
        let registry = self.registry.clone();
        let atlas = &self.atlas;
        let tex = self.block_tex.as_ref().unwrap().id();
        let mut leave = false;
        let mut open_inventory = false;
        let mut select: Option<usize> = None;

        egui::Panel::top("hud").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Survival Cubed").strong());
                ui.separator();
                health_bar(ui, health, max_health);
                ui.separator();
                ui.label(if night { "🌙 Night" } else { "☀ Day" });
                ui.separator();
                ui.label("Move: A/D · Jump/Climb: Space · Down: S · Mine: LMB · Place: RMB");
                ui.separator();
                ui.label("[1–9] Select · [E] Inventory · [F] Eat · [M] Mark · [N] Unmark");
                ui.separator();
                ui.label(format!("Players online: {}", other_players + 1));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Leave").clicked() {
                        leave = true;
                    }
                    if ui.button("Inventory").clicked() {
                        open_inventory = true;
                    }
                    ui.label(format!("({:.0}, {:.0})", pos.x, pos.y));
                });
            });
        });

        // Hotbar lives along the bottom of the screen, like a real game.
        egui::Panel::bottom("hotbar").show_inside(ui, |ui| {
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    for (i, slot) in hotbar.iter().enumerate() {
                        let resp = slot_widget(
                            ui,
                            &registry,
                            atlas,
                            tex,
                            *slot,
                            Some(i),
                            false,
                            i == selected_slot,
                        );
                        if resp.clicked() {
                            select = Some(i);
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });

        if leave {
            self.leave();
        }
        if let Some(g) = self.game.as_mut() {
            if let Some(i) = select {
                g.selected_slot = i;
            }
            if open_inventory {
                g.inventory_open = true;
            }
        }
    }

    /// Draw the waypoint markers around the edge of the screen. Each marker points
    /// at its world target: when the target is off-screen the marker is pinned to
    /// the nearest screen edge (with a distance readout in tiles); when on-screen
    /// it sits at the target. Default markers (home, last death) carry an icon;
    /// personal waypoints are a dot in their stored colour.
    fn waypoint_overlay(&self, ui: &mut egui::Ui) {
        let Some(g) = &self.game else {
            return;
        };
        let ctx = ui.ctx();
        let screen = ctx.content_rect();
        // Points per world pixel: the world is drawn at `ZOOM` physical pixels per
        // world pixel, and egui works in points (physical / pixels_per_point).
        let scale = ZOOM / ctx.pixels_per_point();
        let player_center = g.pos + Vec2::new(PLAYER_W * 0.5, PLAYER_H * 0.5);
        let center_pt = screen.center();
        // Keep markers clear of the top HUD bar and the bottom hotbar.
        let bounds = egui::Rect::from_min_max(
            egui::pos2(screen.min.x + 16.0, screen.min.y + 34.0),
            egui::pos2(screen.max.x - 16.0, screen.max.y - 56.0),
        );
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("waypoint_overlay"),
        ));

        // Collect markers: (world top-left, disc colour, optional icon glyph).
        let mut markers: Vec<(Vec2, egui::Color32, Option<&str>)> = Vec::new();
        if let Some(h) = g.home {
            markers.push((h, egui::Color32::from_rgb(80, 200, 120), Some("🏠")));
        }
        if let Some(d) = g.death {
            markers.push((d, egui::Color32::from_rgb(210, 60, 60), Some("💀")));
        }
        for w in &g.waypoints {
            let c = egui::Color32::from_rgb(
                (w.color[0].clamp(0.0, 1.0) * 255.0) as u8,
                (w.color[1].clamp(0.0, 1.0) * 255.0) as u8,
                (w.color[2].clamp(0.0, 1.0) * 255.0) as u8,
            );
            markers.push((Vec2::new(w.x, w.y), c, None));
        }

        for (world_pos, color, icon) in markers {
            let target_center = world_pos + Vec2::new(PLAYER_W * 0.5, PLAYER_H * 0.5);
            let delta = target_center - player_center;
            let want = center_pt + egui::vec2(delta.x * scale, delta.y * scale);
            // Skip markers sitting on the player to avoid a dot under the avatar.
            if delta.length() < 4.0 {
                continue;
            }
            let on_screen = bounds.contains(want);
            let pos = if on_screen {
                want
            } else {
                clamp_to_rect_edge(bounds, want)
            };
            let r = 7.0;
            painter.circle_filled(pos, r, color);
            painter.circle_stroke(
                pos,
                r,
                egui::Stroke::new(1.5, egui::Color32::from_black_alpha(160)),
            );
            if let Some(glyph) = icon {
                painter.text(
                    pos,
                    egui::Align2::CENTER_CENTER,
                    glyph,
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );
            }
            // Off-screen markers show how far away (in tiles) the target is, placed
            // just inside the dot toward the screen centre.
            if !on_screen {
                let inward = (center_pt - pos).normalized();
                let tiles = (delta.length() / TILE_SIZE).round() as i32;
                painter.text(
                    pos + inward * (r + 7.0),
                    egui::Align2::CENTER_CENTER,
                    format!("{tiles}"),
                    egui::FontId::proportional(11.0),
                    egui::Color32::from_white_alpha(200),
                );
            }
        }
    }

    /// Draw the creator structure-tool overlays in world space: the active
    /// selection rectangle, and — while a structure is loaded for pasting — its
    /// footprint outlined at the cursor. No-op outside creator mode.
    fn selection_overlay(&self, ui: &mut egui::Ui) {
        let Some(g) = &self.game else { return };
        if !g.creator {
            return;
        }
        let ctx = ui.ctx();
        let screen = ctx.content_rect();
        let scale = ZOOM / ctx.pixels_per_point();
        let player_center = g.pos + Vec2::new(PLAYER_W * 0.5, PLAYER_H * 0.5);
        let center_pt = screen.center();
        // World pixel -> screen point, matching the camera the tiles are drawn with.
        let to_screen = |wx: f32, wy: f32| {
            center_pt
                + egui::vec2(
                    (wx - player_center.x) * scale,
                    (wy - player_center.y) * scale,
                )
        };
        let cell_rect = |x0: i32, y0: i32, x1: i32, y1: i32| {
            egui::Rect::from_two_pos(
                to_screen(x0 as f32 * TILE_SIZE, y0 as f32 * TILE_SIZE),
                to_screen((x1 + 1) as f32 * TILE_SIZE, (y1 + 1) as f32 * TILE_SIZE),
            )
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("selection_overlay"),
        ));

        // The committed/in-progress selection box.
        if let Some((x0, y0, x1, y1)) = selection_bounds(g) {
            let rect = cell_rect(x0, y0, x1, y1);
            painter.rect_filled(
                rect,
                0,
                egui::Color32::from_rgba_unmultiplied(120, 200, 255, 40),
            );
            painter.rect_stroke(
                rect,
                0,
                egui::Stroke::new(2.0, egui::Color32::from_rgb(120, 200, 255)),
                egui::StrokeKind::Inside,
            );
        }

        // Paste preview: the loaded structure's footprint at the cursor cell.
        if let (Some(s), Some(gfx)) = (&g.pending_paste, self.gfx.as_ref()) {
            let view_w = gfx.size.width.max(1) as f32 / ZOOM;
            let view_h = gfx.size.height.max(1) as f32 / ZOOM;
            let offset = player_center - Vec2::new(view_w * 0.5, view_h * 0.5);
            let world = offset + Vec2::new(self.input.mouse.0 / ZOOM, self.input.mouse.1 / ZOOM);
            let tx = (world.x / TILE_SIZE).floor() as i32;
            let ty = (world.y / TILE_SIZE).floor() as i32;
            let rect = cell_rect(tx, ty, tx + s.width as i32 - 1, ty + s.height as i32 - 1);
            painter.rect_stroke(
                rect,
                0,
                egui::Stroke::new(2.0, egui::Color32::from_rgb(140, 240, 160)),
                egui::StrokeKind::Inside,
            );
        }
    }

    /// Chat overlay: a scrollback of recent lines pinned to the bottom-left, plus
    /// an input box that appears (and grabs focus) while chatting. Sender names
    /// and `:icon:` tokens in each line are drawn with their real sprites.
    /// A banner across the top while spectating another player (admin
    /// `/spectate`), naming the watched player and how to stop.
    fn spectate_overlay(&mut self, ui: &mut egui::Ui) {
        let Some(g) = self.game.as_ref() else { return };
        let Some(tid) = g.spectating else { return };
        let who = g
            .entities
            .get(tid)
            .and_then(|e| match &e.kind {
                EntityKind::Player { name } if !name.is_empty() => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("Player {tid}"));
        egui::Area::new(egui::Id::new("spectate_banner"))
            .anchor(egui::Align2::CENTER_TOP, [0.0, 12.0])
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE
                    .fill(egui::Color32::from_black_alpha(160))
                    .inner_margin(egui::Margin::symmetric(10, 5))
                    .corner_radius(egui::CornerRadius::same(4))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "👁 Spectating {who}  —  /spectate to stop"
                            ))
                            .color(egui::Color32::WHITE),
                        );
                    });
            });
    }

    fn chat_ui(&mut self, ui: &mut egui::Ui) {
        // Only draw the panel when there's something to show or the box is open,
        // so an idle game has a clean screen.
        let (chat_open, has_log) = {
            let g = self.game.as_ref().unwrap();
            (g.chat_open, !g.chat_log.is_empty())
        };
        if !chat_open && !has_log {
            return;
        }

        // Snapshot what the closure needs so it never borrows `self.game`.
        let recent: Vec<(String, String)> = {
            let g = self.game.as_ref().unwrap();
            let start = g.chat_log.len().saturating_sub(CHAT_VISIBLE_LINES);
            g.chat_log[start..]
                .iter()
                .map(|l| (l.from.clone(), l.text.clone()))
                .collect()
        };
        let mut input = self.game.as_ref().unwrap().chat_input.clone();
        let want_focus = self.game.as_ref().unwrap().chat_focus;

        let registry = self.registry.clone();
        let atlas = &self.atlas;
        let tex = self.block_tex.as_ref().unwrap().id();

        let mut submit: Option<String> = None;
        let mut close = false;
        let mut focused = false;

        egui::Area::new(egui::Id::new("chat"))
            .anchor(egui::Align2::LEFT_BOTTOM, [8.0, -64.0])
            .show(ui.ctx(), |ui| {
                ui.set_max_width(440.0);
                egui::Frame::NONE
                    .fill(egui::Color32::from_black_alpha(150))
                    .inner_margin(egui::Margin::same(6))
                    .corner_radius(egui::CornerRadius::same(4))
                    .show(ui, |ui| {
                        for (from, text) in &recent {
                            render_chat_line(ui, &registry, atlas, tex, from, text);
                        }
                        if chat_open {
                            ui.add_space(2.0);
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut input)
                                    .desired_width(428.0)
                                    .hint_text("Say something… ( :stone: :zombie: for icons )")
                                    .char_limit(MAX_CHAT_INPUT_LEN),
                            );
                            if want_focus {
                                resp.request_focus();
                            }
                            focused = resp.has_focus();
                            // Enter while focused sends; losing focus any other way
                            // (Escape, clicking out) just closes the box.
                            if resp.lost_focus() {
                                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if enter && !input.trim().is_empty() {
                                    submit = Some(input.clone());
                                }
                                close = true;
                            }
                        }
                    });
            });

        if let Some(text) = &submit
            && let Some(net) = &self.net
        {
            let _ = net.commands.send(NetCommand::Chat { text: text.clone() });
        }
        if let Some(g) = self.game.as_mut() {
            g.chat_input = input;
            // Clear focus request once the box has actually taken focus.
            if focused {
                g.chat_focus = false;
            }
            if submit.is_some() || close {
                g.chat_open = false;
                g.chat_focus = false;
                g.chat_input.clear();
            }
        }
    }

    /// The full inventory management screen: storage grid plus the hotbar row,
    /// with click-to-move slot management. Shown over the HUD when toggled.
    fn inventory_window(&mut self, ui: &mut egui::Ui) {
        let (slots, move_from, selected_slot, facing) = {
            let g = self.game.as_ref().unwrap();
            (
                g.inventory.to_slots(),
                g.move_from,
                g.selected_slot,
                g.player_facing,
            )
        };
        let registry = self.registry.clone();
        let atlas = &self.atlas;
        let tex = self.block_tex.as_ref().unwrap().id();
        let inventory = self.game.as_ref().unwrap().inventory.clone();
        let mut clicked: Option<usize> = None;
        let mut dropped: Option<usize> = None;
        let mut craft: Option<u16> = None;
        let mut close = false;

        egui::Window::new("Inventory")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label("Storage");
                for row in 0..(STORAGE_SLOTS / 9) {
                    ui.horizontal(|ui| {
                        for col in 0..9 {
                            let idx = HOTBAR_SLOTS + row * 9 + col;
                            let resp = slot_widget(
                                ui,
                                &registry,
                                atlas,
                                tex,
                                slots.get(idx).copied().flatten(),
                                None,
                                move_from == Some(idx),
                                false,
                            );
                            if resp.clicked() {
                                clicked = Some(idx);
                            }
                            if resp.secondary_clicked() {
                                dropped = Some(idx);
                            }
                        }
                    });
                }
                ui.add_space(8.0);
                ui.label("Hotbar");
                ui.horizontal(|ui| {
                    for idx in 0..HOTBAR_SLOTS {
                        let resp = slot_widget(
                            ui,
                            &registry,
                            atlas,
                            tex,
                            slots.get(idx).copied().flatten(),
                            Some(idx),
                            move_from == Some(idx),
                            idx == selected_slot,
                        );
                        if resp.clicked() {
                            clicked = Some(idx);
                        }
                        if resp.secondary_clicked() {
                            dropped = Some(idx);
                        }
                    }
                });
                ui.add_space(6.0);
                ui.weak("Click a slot, then another, to move/stack · click it again to cancel");
                ui.weak("Right-click a slot to drop the whole stack on the ground");

                ui.separator();
                ui.label("Crafting");
                for (idx, recipe) in crate::recipe::RECIPES.iter().enumerate() {
                    let can = recipe.craftable(&inventory);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(can, egui::Button::new(recipe.name))
                            .on_hover_text(recipe_tooltip(&registry, recipe))
                            .clicked()
                        {
                            craft = Some(idx as u16);
                        }
                        ui.weak(recipe_summary(&registry, recipe));
                    });
                }

                ui.add_space(4.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });

        // Resolve a slot click into a pending-move selection or a completed move.
        if let Some(idx) = clicked {
            let mut move_cmd: Option<(u8, u8)> = None;
            if let Some(g) = self.game.as_mut() {
                match g.move_from {
                    None => {
                        if g.inventory.get(idx).is_some() {
                            g.move_from = Some(idx);
                        }
                    }
                    Some(from) if from == idx => g.move_from = None,
                    Some(from) => {
                        // Optimistic local move; the server confirms with a snapshot.
                        g.inventory.move_stack(from, idx);
                        g.move_from = None;
                        move_cmd = Some((from as u8, idx as u8));
                    }
                }
            }
            if let (Some((from, to)), Some(net)) = (move_cmd, &self.net) {
                let _ = net.commands.send(NetCommand::MoveItem { from, to });
            }
        }
        // Right-click drops a slot's whole stack at the player's feet. The server
        // confirms with an inventory snapshot, so no optimistic update here.
        if let Some(idx) = dropped {
            if let Some(g) = self.game.as_mut() {
                g.move_from = None;
            }
            if let Some(net) = &self.net {
                let _ = net.commands.send(NetCommand::DropItem {
                    slot: idx as u8,
                    all: true,
                    dir: if facing { 1.0 } else { -1.0 },
                });
            }
        }
        // Crafting is server-authoritative: send the request and let the
        // resulting Inventory snapshot update the display.
        if let (Some(recipe), Some(net)) = (craft, &self.net) {
            let _ = net.commands.send(NetCommand::Craft { recipe });
        }
        if close && let Some(g) = self.game.as_mut() {
            g.inventory_open = false;
            g.move_from = None;
        }
    }

    /// The forge smelting GUI, opened by right-clicking a forge block. Lists the
    /// smelting recipes ([`SMELT_RECIPES`](crate::recipe::SMELT_RECIPES)) with a
    /// stock readout; smelting is server-authoritative (a request is sent and the
    /// resulting inventory snapshot updates the display).
    fn forge_window(&mut self, ui: &mut egui::Ui) {
        // Close if the forge this GUI belongs to is gone (mined or out of reach).
        let valid = {
            let g = self.game.as_ref().unwrap();
            g.forge_cell.is_some_and(|(x, y)| {
                g.world.get_block(x, y) == crate::block::FORGE && cell_in_reach(g, x, y)
            })
        };
        if !valid {
            if let Some(g) = self.game.as_mut() {
                g.forge_open = false;
                g.forge_cell = None;
            }
            return;
        }
        let registry = self.registry.clone();
        let inventory = self.game.as_ref().unwrap().inventory.clone();
        // The fuel the player has selected to burn, and how many units one smelt
        // spends of it. Mutated by the fuel picker below and written back at the end.
        let mut fuel = self.game.as_ref().unwrap().forge_fuel;
        let fuel_units = crate::block::forge_fuel_units(fuel).unwrap_or(1);
        let fuel_stock = inventory.count(fuel);
        // (recipe index, how many times to smelt)
        let mut smelt: Option<(u16, u32)> = None;
        // Tool to repair, if its button is clicked this frame.
        let mut repair: Option<BlockId> = None;
        let mut close = false;

        // The distinct worn tools the player is carrying, in slot order, each
        // with its current/maximum durability — one repair row per tool.
        let worn: Vec<(BlockId, u16, u16)> = {
            let mut seen = Vec::new();
            for slot in inventory.slots().iter().flatten() {
                let (item, _, dur) = *slot;
                let max = crate::block::max_durability(item);
                if max > 0 && dur < max && !seen.iter().any(|(b, _, _)| *b == item) {
                    seen.push((item, dur, max));
                }
            }
            seen
        };

        egui::Window::new("Forge")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label("Smelt raw materials into refined goods (consumes fuel).");
                ui.separator();

                // Fuel picker: pick which fuel to burn. Each smelt spends one
                // charge — a charge being one wood/coal or four bark.
                ui.label("Fuel:");
                ui.horizontal(|ui| {
                    for &item in crate::block::FORGE_FUELS {
                        let units = crate::block::forge_fuel_units(item).unwrap_or(1);
                        let name = item_display_name(registry.get(item).name);
                        let label = if units > 1 {
                            format!("{name} ×{units}")
                        } else {
                            name
                        };
                        let have = inventory.count(item) > 0;
                        ui.add_enabled_ui(have, |ui| {
                            ui.selectable_value(&mut fuel, item, label);
                        });
                    }
                });
                ui.separator();

                // How many charges of the chosen fuel the player can afford.
                let fuel_crafts = fuel_stock / fuel_units;
                for (idx, recipe) in crate::recipe::SMELT_RECIPES.iter().enumerate() {
                    let one = recipe.craftable(&inventory) && fuel_crafts >= 1;
                    let max = recipe.max_crafts(&inventory).min(fuel_crafts);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(one, egui::Button::new(recipe.name))
                            .on_hover_text(recipe_tooltip(&registry, recipe))
                            .clicked()
                        {
                            smelt = Some((idx as u16, 1));
                        }
                        if ui
                            .add_enabled(max > 1, egui::Button::new(format!("All ({max})")))
                            .clicked()
                        {
                            smelt = Some((idx as u16, max));
                        }
                        ui.weak(recipe_summary(&registry, recipe));
                    });
                }
                ui.add_space(8.0);
                ui.weak(format!(
                    "Raw iron: {}   ·   Raw tungsten: {}   ·   Wood: {}   ·   Coal: {}   ·   Bark: {}",
                    inventory.count(crate::block::RAW_IRON),
                    inventory.count(crate::block::RAW_TUNGSTEN),
                    inventory.count(crate::block::WOOD),
                    inventory.count(crate::block::COAL),
                    inventory.count(crate::block::BARK),
                ));
                ui.weak(format!(
                    "Iron ingots: {}   ·   Tungsten ingots: {}",
                    inventory.count(crate::block::IRON_INGOT),
                    inventory.count(crate::block::TUNGSTEN_INGOT),
                ));

                // Repair worn tools using their crafting material.
                ui.separator();
                ui.label("Repair tools (spends their material).");
                if worn.is_empty() {
                    ui.weak("No worn tools to repair.");
                }
                for (item, dur, max) in &worn {
                    let material = crate::block::repair_material(*item);
                    let have = material.is_some_and(|m| inventory.count(m) > 0);
                    let name = item_display_name(registry.get(*item).name);
                    let mat_name = material
                        .map(|m| item_display_name(registry.get(m).name))
                        .unwrap_or_default();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(have, egui::Button::new(format!("Repair {name}")))
                            .on_hover_text(format!("Spends 1 {mat_name}"))
                            .clicked()
                        {
                            repair = Some(*item);
                        }
                        ui.weak(format!("{dur}/{max}"));
                    });
                }

                ui.add_space(4.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });

        // Remember the chosen fuel for next time the GUI opens.
        if let Some(g) = self.game.as_mut() {
            g.forge_fuel = fuel;
        }
        // Smelting is server-authoritative: send the request and let the
        // resulting Inventory snapshot update the display.
        if let (Some((recipe, count)), Some(net)) = (smelt, &self.net)
            && count > 0
        {
            let _ = net.commands.send(NetCommand::Smelt {
                recipe,
                count,
                fuel,
            });
        }
        // Repairing is server-authoritative too: it consumes the material and
        // replies with an updated inventory snapshot.
        if let (Some(item), Some(net)) = (repair, &self.net) {
            let _ = net.commands.send(NetCommand::Repair { item });
        }
        if close && let Some(g) = self.game.as_mut() {
            g.forge_open = false;
            g.forge_cell = None;
        }
    }

    /// The campfire GUI, opened by right-clicking a campfire block. Lets the
    /// player feed it fuel (wood, coal, or bark) to light it and keep it burning, and
    /// cook raw meat on it while lit. Cooking and fueling are server-authoritative
    /// (requests are sent and the resulting snapshots update the display).
    fn campfire_window(&mut self, ui: &mut egui::Ui) {
        // Close if the campfire this GUI belongs to is gone or out of reach.
        let (valid, lit, cell) = {
            let g = self.game.as_ref().unwrap();
            match g.campfire_cell {
                Some((x, y)) => {
                    let block = g.world.get_block(x, y);
                    (
                        crate::block::is_campfire(block) && cell_in_reach(g, x, y),
                        block == crate::block::CAMPFIRE_LIT,
                        (x, y),
                    )
                }
                None => (false, false, (0, 0)),
            }
        };
        if !valid {
            if let Some(g) = self.game.as_mut() {
                g.campfire_open = false;
                g.campfire_cell = None;
            }
            return;
        }
        let registry = self.registry.clone();
        let inventory = self.game.as_ref().unwrap().inventory.clone();
        let mut fuel: Option<BlockId> = None;
        // (recipe index, how many times to cook)
        let mut cook: Option<(u16, u32)> = None;
        let mut close = false;

        egui::Window::new("Campfire")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if lit {
                    ui.colored_label(egui::Color32::from_rgb(240, 160, 60), "🔥 Burning");
                } else {
                    ui.colored_label(egui::Color32::GRAY, "Unlit — add fuel to light it");
                }
                ui.separator();

                // Fuel: wood burns long, bark gives a smaller boost.
                ui.label("Add fuel to keep it burning:");
                ui.horizontal(|ui| {
                    let have_wood = inventory.count(crate::block::WOOD) > 0;
                    if ui
                        .add_enabled(have_wood, egui::Button::new("Add Wood"))
                        .on_hover_text("Burns a long while")
                        .clicked()
                    {
                        fuel = Some(crate::block::WOOD);
                    }
                    let have_bark = inventory.count(crate::block::BARK) > 0;
                    if ui
                        .add_enabled(have_bark, egui::Button::new("Add Bark"))
                        .on_hover_text("A nice little fire boost")
                        .clicked()
                    {
                        fuel = Some(crate::block::BARK);
                    }
                    let have_coal = inventory.count(crate::block::COAL) > 0;
                    if ui
                        .add_enabled(have_coal, egui::Button::new("Add Coal"))
                        .on_hover_text("Burns the longest")
                        .clicked()
                    {
                        fuel = Some(crate::block::COAL);
                    }
                });
                ui.weak(format!(
                    "Wood: {}   ·   Bark: {}   ·   Coal: {}",
                    inventory.count(crate::block::WOOD),
                    inventory.count(crate::block::BARK),
                    inventory.count(crate::block::COAL),
                ));

                // Cooking: only while lit.
                ui.separator();
                ui.label("Cook (campfire must be lit):");
                for (idx, recipe) in crate::recipe::COOK_RECIPES.iter().enumerate() {
                    let one = lit && recipe.craftable(&inventory);
                    let max = recipe.max_crafts(&inventory);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(one, egui::Button::new(recipe.name))
                            .on_hover_text(recipe_tooltip(&registry, recipe))
                            .clicked()
                        {
                            cook = Some((idx as u16, 1));
                        }
                        if ui
                            .add_enabled(lit && max > 1, egui::Button::new(format!("All ({max})")))
                            .clicked()
                        {
                            cook = Some((idx as u16, max));
                        }
                        ui.weak(recipe_summary(&registry, recipe));
                    });
                }

                ui.add_space(4.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });

        let (x, y) = cell;
        if let (Some(fuel), Some(net)) = (fuel, &self.net) {
            let _ = net.commands.send(NetCommand::FuelCampfire { x, y, fuel });
        }
        if let (Some((recipe, count)), Some(net)) = (cook, &self.net)
            && count > 0
        {
            let _ = net.commands.send(NetCommand::Cook {
                x,
                y,
                recipe,
                count,
            });
        }
        if close && let Some(g) = self.game.as_mut() {
            g.campfire_open = false;
            g.campfire_cell = None;
        }
    }

    /// The creator-tools window, shown in-game whenever this client is allowed
    /// creator mode. A top toggle enters/leaves creator mode; while active it
    /// toggles flight, jumps the world clock, spawns creatures, picks the block
    /// placed for free by infinite-block placement, and gives items.
    fn creator_window(&mut self, ui: &mut egui::Ui) {
        #[allow(clippy::type_complexity)]
        let (
            was_creator,
            mut creator,
            mut fly,
            fly_mult,
            mut time,
            mut creator_block,
            player_pos,
            mut tool,
            sel,
            pasting,
        ) = {
            let g = self.game.as_ref().unwrap();
            (
                g.creator,
                g.creator,
                g.fly,
                g.fly_speed_mult,
                g.time_of_day,
                g.creator_block,
                g.pos,
                g.creator_tool,
                selection_bounds(g),
                g.pending_paste
                    .as_ref()
                    .map(|s| (s.width, s.height, s.entities.len())),
            )
        };
        // How many capturable creatures sit inside the current selection (shown so
        // the creator knows the save will include them).
        let sel_entities = sel.map(|(x0, y0, x1, y1)| {
            let g = self.game.as_ref().unwrap();
            let (ox, oy) = (x0 as f32 * TILE_SIZE, y0 as f32 * TILE_SIZE);
            let rw = (x1 - x0 + 1) as f32 * TILE_SIZE;
            let rh = (y1 - y0 + 1) as f32 * TILE_SIZE;
            g.entities
                .values()
                .filter(|e| {
                    !matches!(e.kind, EntityKind::Player { .. })
                        && e.x >= ox
                        && e.x < ox + rw
                        && e.y >= oy
                        && e.y < oy + rh
                })
                .count()
        });
        let registry = self.registry.clone();
        // The item-giver input lives on `self`, but the window closure below
        // doesn't borrow `self`; pull the fields into locals and write them back.
        let mut give_input = std::mem::take(&mut self.give_item_input);
        let mut give_count = self.give_item_count;
        let mut set_time: Option<f32> = None;
        let mut spawn: Option<EntityKind> = None;
        let mut give: Option<(BlockId, u32)> = None;
        // Structure-tool inputs/intents, applied after the closure (which can't
        // borrow `self`).
        let mut name_input = std::mem::take(&mut self.structure_name_input);
        let structure_list = self.structure_list.clone();
        let structure_status = self.structure_status.clone();
        let mut do_save = false;
        let mut load_name: Option<String> = None;
        let mut delete_name: Option<String> = None;
        let mut refresh_list = false;
        let mut clear_sel = false;
        let mut cancel_paste = false;

        egui::Window::new("🛠 Creator tools")
            .anchor(egui::Align2::RIGHT_TOP, [-8.0, 56.0])
            .resizable(false)
            .collapsible(true)
            .show(ui.ctx(), |ui| {
                ui.checkbox(&mut creator, "Creator mode");
                if !creator {
                    ui.weak("Enable to fly, build freely, and be ignored by monsters.");
                    return;
                }
                ui.separator();
                ui.checkbox(&mut fly, "Fly  (W/Space up · S/↓ down)");
                ui.weak(format!("Fly speed ×{fly_mult:.1} — scroll wheel to adjust"));

                ui.separator();
                ui.label("Time of day");
                let label = if daylight::is_night(time) {
                    "🌙 night"
                } else {
                    "☀ day"
                };
                let resp = ui.add(
                    egui::Slider::new(&mut time, 0.0..=0.999)
                        .show_value(false)
                        .text(label),
                );
                if resp.changed() {
                    set_time = Some(time);
                }
                ui.horizontal(|ui| {
                    if ui.button("Dawn").clicked() {
                        set_time = Some(0.0);
                    }
                    if ui.button("Noon").clicked() {
                        set_time = Some(0.25);
                    }
                    if ui.button("Dusk").clicked() {
                        set_time = Some(0.5);
                    }
                    if ui.button("Midnight").clicked() {
                        set_time = Some(0.75);
                    }
                });

                ui.separator();
                ui.label("Spawn entity (at player)");
                ui.horizontal(|ui| {
                    if ui.button("Slime").clicked() {
                        spawn = Some(EntityKind::Slime);
                    }
                    if ui.button("Chicken").clicked() {
                        spawn = Some(EntityKind::Chicken);
                    }
                    if ui.button("Goat").clicked() {
                        spawn = Some(EntityKind::Goat);
                    }
                    if ui.button("Cat").clicked() {
                        spawn = Some(EntityKind::Cat {
                            owner: None,
                            sitting: false,
                        });
                    }
                    if ui.button("Puppy").clicked() {
                        spawn = Some(EntityKind::Puppy {
                            owner: None,
                            sitting: false,
                        });
                    }
                    if ui.button("Horse").clicked() {
                        spawn = Some(EntityKind::Horse { owner: None });
                    }
                    if ui.button("Zombie").clicked() {
                        spawn = Some(EntityKind::Zombie);
                    }
                    if ui.button("Spider").clicked() {
                        spawn = Some(EntityKind::Spider);
                    }
                    if ui.button("Snake").clicked() {
                        spawn = Some(EntityKind::Snake);
                    }
                    if ui.button("Skeleton").clicked() {
                        spawn = Some(EntityKind::Skeleton);
                    }
                    if ui.button("Charred Skeleton").clicked() {
                        spawn = Some(EntityKind::CharredSkeleton);
                    }
                });

                ui.separator();
                ui.label("Infinite block (RMB places)");
                ui.horizontal(|ui| {
                    for block in [STONE, DIRT, GRASS, LOG, LEAVES, CHARRED_ROCK, FIRE] {
                        let name = registry.get(block).name;
                        if ui.selectable_label(creator_block == block, name).clicked() {
                            creator_block = block;
                        }
                    }
                });

                ui.separator();
                ui.label("Give item (id or name)");
                let resolved = resolve_item(&registry, &give_input);
                ui.horizontal(|ui| {
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut give_input)
                            .desired_width(120.0)
                            .hint_text("id or name"),
                    );
                    ui.add(egui::DragValue::new(&mut give_count).range(1..=999));
                    let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let enabled = resolved.is_some();
                    if ui.add_enabled(enabled, egui::Button::new("Give")).clicked()
                        || (submit && enabled)
                    {
                        give = resolved.map(|item| (item, give_count.max(1)));
                        give_input.clear();
                    }
                });
                // Resolved-item preview / typo warning under the field.
                match resolved {
                    Some(item) => {
                        ui.label(format!("→ {} (#{item})", registry.get(item).name));
                    }
                    None if !give_input.trim().is_empty() => {
                        ui.colored_label(egui::Color32::LIGHT_RED, "no such item");
                    }
                    None => {}
                }

                ui.separator();
                ui.label("Structures");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut tool, CreatorTool::Build, "🛠 Build");
                    ui.selectable_value(&mut tool, CreatorTool::Select, "⬚ Select");
                });
                if tool == CreatorTool::Select {
                    ui.weak("Drag LMB to box a region · RMB clears.");
                }

                // Save the current selection.
                if let Some((x0, y0, x1, y1)) = sel {
                    ui.label(format!(
                        "Selection {}×{} · {} mob(s)",
                        x1 - x0 + 1,
                        y1 - y0 + 1,
                        sel_entities.unwrap_or(0)
                    ));
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut name_input)
                                .desired_width(110.0)
                                .hint_text("name"),
                        );
                        let ok = crate::save::sanitize_structure_name(&name_input).is_some();
                        if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
                            do_save = true;
                        }
                        if ui.button("Clear").clicked() {
                            clear_sel = true;
                        }
                    });
                } else {
                    ui.weak("Select a region to save it.");
                }

                // The saved-structure library: load (to paste) or delete.
                ui.horizontal(|ui| {
                    ui.label("Saved:");
                    if ui.small_button("⟳").clicked() {
                        refresh_list = true;
                    }
                });
                if structure_list.is_empty() {
                    ui.weak("None saved yet.");
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(110.0)
                        .show(ui, |ui| {
                            for nm in &structure_list {
                                ui.horizontal(|ui| {
                                    if ui.button("Load").clicked() {
                                        load_name = Some(nm.clone());
                                    }
                                    if ui.small_button("🗑").clicked() {
                                        delete_name = Some(nm.clone());
                                    }
                                    ui.label(nm);
                                });
                            }
                        });
                }

                if let Some((w, h, n)) = pasting {
                    ui.colored_label(
                        egui::Color32::LIGHT_BLUE,
                        format!("Placing {w}×{h} · {n} mob(s): LMB stamps · RMB cancels"),
                    );
                    if ui.button("Cancel paste").clicked() {
                        cancel_paste = true;
                    }
                }
                if !structure_status.is_empty() {
                    ui.weak(&structure_status);
                }
            });

        // Leaving creator mode also drops flight so the player doesn't hang midair.
        if !creator {
            fly = false;
        }
        // Apply UI changes back to game state.
        if let Some(g) = self.game.as_mut() {
            g.creator = creator;
            g.fly = fly;
            g.creator_block = creator_block;
            g.creator_tool = tool;
            if clear_sel {
                g.sel_a = None;
                g.sel_b = None;
            }
            if cancel_paste {
                g.pending_paste = None;
            }
            if let Some(t) = set_time {
                g.time_of_day = t; // optimistic; the server confirms via TimeOfDay
            }
        }
        // Restore the item-giver input/count edited inside the window closure.
        self.give_item_input = give_input;
        self.give_item_count = give_count;
        // Refresh the saved-structure list on demand, or the first time creator
        // mode is entered this session (so the library is populated up front).
        if refresh_list || (creator && !was_creator) {
            self.structure_list = crate::save::list_structures();
        }
        // Capture the selected region from the mirrored world and save it.
        if do_save && let Some((x0, y0, x1, y1)) = sel {
            let structure = {
                let g = self.game.as_ref().unwrap();
                // Capture every non-player creature in the region, as world-pixel
                // positions; `from_region` offsets and clips them to the bounds.
                let ents: Vec<(f32, f32, EntityKind)> = g
                    .entities
                    .values()
                    .filter(|e| !matches!(e.kind, EntityKind::Player { .. }))
                    .map(|e| (e.x, e.y, structure_entity_kind(&e.kind)))
                    .collect();
                Structure::from_region(x0, y0, x1, y1, |x, y| g.world.get_block(x, y), ents)
            };
            match crate::save::save_structure(&name_input, &structure) {
                Ok(()) => {
                    self.structure_status = format!(
                        "Saved '{}' ({}×{}, {} mob(s)).",
                        name_input.trim(),
                        structure.width,
                        structure.height,
                        structure.entities.len()
                    );
                    self.structure_list = crate::save::list_structures();
                    name_input.clear();
                }
                Err(e) => self.structure_status = format!("Save failed: {e:#}"),
            }
        }
        // Load a structure into the paste buffer (the world interaction stamps it).
        if let Some(nm) = load_name {
            match crate::save::load_structure(&nm) {
                Ok(s) => {
                    let (w, h, n) = (s.width, s.height, s.entities.len());
                    if let Some(g) = self.game.as_mut() {
                        g.pending_paste = Some(s);
                    }
                    self.structure_status =
                        format!("Loaded '{nm}' ({w}×{h}, {n} mob(s)) — LMB to place.");
                }
                Err(e) => self.structure_status = format!("Load failed: {e:#}"),
            }
        }
        if let Some(nm) = delete_name {
            let _ = crate::save::delete_structure(&nm);
            self.structure_list = crate::save::list_structures();
            self.structure_status = format!("Deleted '{nm}'.");
        }
        self.structure_name_input = name_input;
        // Forward authoritative-state changes to the server.
        if let Some(net) = &self.net {
            // Tell the server when creator mode flips, so monsters start/stop
            // ignoring this player.
            if creator != was_creator {
                let _ = net.commands.send(NetCommand::SetCreator { on: creator });
            }
            if let Some(t) = set_time {
                let _ = net.commands.send(NetCommand::SetTime { t });
            }
            if let Some(kind) = spawn {
                let _ = net.commands.send(NetCommand::SpawnEntity {
                    kind,
                    x: player_pos.x,
                    y: player_pos.y,
                });
            }
            if let Some((item, count)) = give {
                let _ = net.commands.send(NetCommand::GiveItem { item, count });
            }
        }
    }

    // --- Rendering ---

    fn render_frame(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if self.gfx.is_none() || self.egui_state.is_none() {
            return;
        }

        let raw = self.egui_state.as_mut().unwrap().take_egui_input(&window);
        let ctx = self.egui_ctx.clone();
        let full = ctx.run_ui(raw, |ui| self.build_ui(ui));
        self.egui_state
            .as_mut()
            .unwrap()
            .handle_platform_output(&window, full.platform_output);
        let jobs = ctx.tessellate(full.shapes, full.pixels_per_point);

        let (tiles, camera) = self.build_scene();
        let sky = match self.game.as_ref() {
            // The underworld's sky is a smouldering near-black, not the day sky.
            Some(g) if g.dim == Dimension::Underworld => [0.08, 0.02, 0.02, 1.0],
            Some(g) => daylight::sky_color(g.time_of_day),
            None => [0.45, 0.62, 0.86, 1.0],
        };

        let capture = std::mem::take(&mut self.screenshot_requested);
        if let Some(gfx) = self.gfx.as_mut() {
            let captured = gfx.render(
                &tiles,
                camera,
                sky,
                EguiFrame {
                    jobs,
                    textures_delta: full.textures_delta,
                    pixels_per_point: full.pixels_per_point,
                },
                capture,
            );
            if let Some(frame) = captured {
                self.save_screenshot(frame);
            }
        }
    }

    /// Encode and write a captured frame to disk on a background thread (PNG +
    /// compressed JPEG), reporting the result in the status line.
    fn save_screenshot(&mut self, frame: CapturedFrame) {
        let CapturedFrame {
            width,
            height,
            rgba,
        } = frame;
        std::thread::spawn(move || match screenshot::save(rgba, width, height) {
            Ok(path) => log::info!("saved screenshot to {}", path.display()),
            Err(e) => log::warn!("failed to save screenshot: {e}"),
        });
        self.status = "Screenshot saved.".to_string();
    }

    fn build_scene(&self) -> (Vec<TileInstance>, CameraUniform) {
        let gfx = self.gfx.as_ref().unwrap();
        let (vw, vh) = (gfx.size.width.max(1) as f32, gfx.size.height.max(1) as f32);

        let Some(g) = &self.game else {
            return (Vec::new(), CameraUniform::new([0.0, 0.0], [vw, vh], ZOOM));
        };

        let view_w = vw / ZOOM;
        let view_h = vh / ZOOM;
        let center = view_center(g);
        let offset = center - Vec2::new(view_w * 0.5, view_h * 0.5);

        let mut tiles = Vec::new();

        // Daylight tint: everything in the world dims toward night. The underworld
        // knows no day or night — it sits in a permanent dim, faintly warm gloom.
        let tint = if g.dim == Dimension::Underworld {
            [0.5, 0.34, 0.30, 1.0]
        } else {
            let b = daylight::brightness(g.time_of_day);
            [b, b, b, 1.0]
        };

        let left = (offset.x / TILE_SIZE).floor() as i32 - CHUNK_MARGIN;
        let right = ((offset.x + view_w) / TILE_SIZE).ceil() as i32 + CHUNK_MARGIN;
        let top = ((offset.y / TILE_SIZE).floor() as i32 - CHUNK_MARGIN).max(0);
        let bottom =
            (((offset.y + view_h) / TILE_SIZE).ceil() as i32 + CHUNK_MARGIN).min(WORLD_HEIGHT);

        for ty in top..bottom {
            for tx in left..right {
                let id = g.world.get_block(tx, ty);
                let def = self.registry.get(id);
                if !def.visible {
                    continue;
                }
                let uv = self.atlas.block(id);
                tiles.push(TileInstance {
                    pos: [tx as f32 * TILE_SIZE, ty as f32 * TILE_SIZE],
                    size: [TILE_SIZE, TILE_SIZE],
                    uv_min: uv.min,
                    uv_max: uv.max,
                    color: tint,
                });
            }
        }

        // Block-breaking overlay: darken the targeted cell as mining progresses.
        if let Some((tx, ty)) = g.break_target {
            let block = g.world.get_block(tx, ty);
            let held = g
                .inventory
                .get(g.selected_slot)
                .map(|(b, _, _)| b)
                .unwrap_or(AIR);
            let secs =
                self.registry.get(block).break_secs * crate::block::mine_speed_mult(block, held);
            if block != AIR && secs > 0.0 {
                let frac = (g.break_progress / secs).clamp(0.0, 1.0);
                tiles.push(flat_quad(
                    self.atlas.white(),
                    tx as f32 * TILE_SIZE,
                    ty as f32 * TILE_SIZE,
                    TILE_SIZE,
                    TILE_SIZE,
                    [0.0, 0.0, 0.0, frac * 0.7],
                ));
            }
        }

        // Horses currently being ridden (by any remote player, or by us) are drawn
        // as part of their rider's combined sprite, so the standalone horse entity is
        // hidden to avoid drawing it twice.
        let ridden_horses: HashSet<EntityId> = g
            .entities
            .values()
            .filter_map(|e| e.riding)
            .chain(g.riding)
            .collect();

        // Other entities — remote players and server creatures (drawn over tiles).
        for e in g.entities.values() {
            let (w, h) = e.size();
            // A horse someone is riding is drawn via its rider's combined sprite, so
            // skip the standalone horse here.
            if ridden_horses.contains(&e.id) {
                continue;
            }
            // Dropped items render as a small version of their block sprite, not
            // an animation sheet.
            if let EntityKind::DroppedItem { block, .. } = e.kind {
                let uv = self.atlas.block(block);
                tiles.push(TileInstance {
                    pos: [e.x, e.y],
                    size: [w, h],
                    uv_min: uv.min,
                    uv_max: uv.max,
                    color: tint,
                });
                continue;
            }
            // A remote player riding a horse shows the combined player/horse sprite
            // (the horse is part of the art), animated as it moves, centred on their
            // body and resting on their feet — just like the boat pose below.
            if e.riding.is_some() && matches!(e.kind, EntityKind::Player { .. }) {
                let hdef = &sprite::PLAYER_HORSE_SPRITE;
                let (hw, hh) = (hdef.frame_w as f32, hdef.frame_h as f32);
                let facing = g.facing.get(&e.id).copied().unwrap_or(true);
                let frame = sprite::frame_index(e.vx.abs() > 1.0, self.anim_time, hdef);
                tiles.push(entity_instance(
                    self.atlas.sprite_frame(hdef.name, frame),
                    e.x + (w - hw) * 0.5,
                    e.y + h - hh,
                    hw,
                    hh,
                    facing,
                    flash_tint(tint, e.hit_flash),
                ));
                if e.health < e.max_health && e.max_health > 0 {
                    push_health_bar(
                        &mut tiles,
                        self.atlas.white(),
                        e.x,
                        e.y,
                        w,
                        e.health,
                        e.max_health,
                    );
                }
                continue;
            }
            // A remote player riding a boat shows the boat sprite (the rider is part
            // of the art), centred on their body and resting on their feet, exactly
            // as the local avatar is drawn while boating.
            if e.boating && matches!(e.kind, EntityKind::Player { .. }) {
                let bdef = &sprite::BOAT_SPRITE;
                let (bw, bh) = (bdef.frame_w as f32, bdef.frame_h as f32);
                let facing = g.facing.get(&e.id).copied().unwrap_or(true);
                tiles.push(entity_instance(
                    self.atlas.sprite_frame(bdef.name, 0),
                    e.x + (w - bw) * 0.5,
                    e.y + h - bh,
                    bw,
                    bh,
                    facing,
                    flash_tint(tint, e.hit_flash),
                ));
                if e.health < e.max_health && e.max_health > 0 {
                    push_health_bar(
                        &mut tiles,
                        self.atlas.white(),
                        e.x,
                        e.y,
                        w,
                        e.health,
                        e.max_health,
                    );
                }
                continue;
            }
            // A dying zombie plays its one-shot crumble (frame stepped by the
            // death timer); everything else uses its walk sheet (frame stepped by
            // the shared animation clock when moving).
            // A dying creature plays its one-shot death sheet (frame stepped by the
            // death timer): a zombie crumbling at dawn, a slain snake writhing.
            let death = if e.dying > 0.0 {
                sprite::death_sprite_for(&e.kind).map(|d| {
                    let total = e.kind.death_time().unwrap_or(e.dying);
                    let progress = 1.0 - (e.dying / total).clamp(0.0, 1.0);
                    let frame = ((progress * d.frames as f32) as u32).min(d.frames - 1);
                    (d, frame)
                })
            } else {
                None
            };
            let (def, frame) = if let Some((d, frame)) = death {
                (d, frame)
            } else if e.lunge > 0.0 && matches!(e.kind, EntityKind::Snake) {
                // A lunging snake plays its one-shot strike (frame stepped by the
                // lunge timer): it coils through the wind-up then springs.
                let d = &sprite::SNAKE_ATTACK_SPRITE;
                let progress = 1.0 - (e.lunge / crate::entity::SNAKE_LUNGE_TIME).clamp(0.0, 1.0);
                let frame = ((progress * d.frames as f32) as u32).min(d.frames - 1);
                (d, frame)
            } else if matches!(e.kind, EntityKind::Puppy { sitting: true, .. }) {
                // A sitting puppy loops its idle sheet off the shared clock even
                // though it's standing still (frame_index would otherwise freeze a
                // motionless entity on frame 0).
                let d = &sprite::PUPPY_SIT_SPRITE;
                (d, sprite::frame_index(true, self.anim_time, d))
            } else {
                let def = sprite::sprite_for(&e.kind);
                (
                    def,
                    sprite::frame_index(e.vx.abs() > 1.0, self.anim_time, def),
                )
            };
            let facing = g.facing.get(&e.id).copied().unwrap_or(true);
            tiles.push(entity_instance(
                self.atlas.sprite_frame(def.name, frame),
                e.x,
                e.y,
                w,
                h,
                facing,
                flash_tint(tint, e.hit_flash),
            ));
            // A small health bar floats over any wounded creature (but not while
            // it is crumbling away).
            if e.dying <= 0.0 && e.health < e.max_health && e.max_health > 0 {
                push_health_bar(
                    &mut tiles,
                    self.atlas.white(),
                    e.x,
                    e.y,
                    w,
                    e.health,
                    e.max_health,
                );
            }
        }
        // Self (the special, locally-simulated player entity). While boating, the
        // boat sprite (which already includes the seated rider) is drawn in place
        // of the plain player, centred on the player box and resting its hull on
        // the player's feet.
        if g.boating {
            let def = &sprite::BOAT_SPRITE;
            let (bw, bh) = (def.frame_w as f32, def.frame_h as f32);
            tiles.push(entity_instance(
                self.atlas.sprite_frame(def.name, 0),
                g.pos.x + (PLAYER_W - bw) * 0.5,
                g.pos.y + PLAYER_H - bh,
                bw,
                bh,
                g.player_facing,
                flash_tint(tint, g.hit_flash),
            ));
        } else if g.riding.is_some() {
            // Mounted: the combined player/horse sprite (the horse is part of the
            // art) replaces the plain avatar, animated as the player gallops, centred
            // on the player box and resting on their feet — like the boat pose above.
            let def = &sprite::PLAYER_HORSE_SPRITE;
            let (hw, hh) = (def.frame_w as f32, def.frame_h as f32);
            let frame = sprite::frame_index(g.vel.x.abs() > 1.0, self.anim_time, def);
            tiles.push(entity_instance(
                self.atlas.sprite_frame(def.name, frame),
                g.pos.x + (PLAYER_W - hw) * 0.5,
                g.pos.y + PLAYER_H - hh,
                hw,
                hh,
                g.player_facing,
                flash_tint(tint, g.hit_flash),
            ));
        } else {
            let def = &sprite::PLAYER_SPRITE;
            let frame = sprite::frame_index(g.vel.x.abs() > 1.0, self.anim_time, def);
            tiles.push(entity_instance(
                self.atlas.sprite_frame(def.name, frame),
                g.pos.x,
                g.pos.y,
                PLAYER_W,
                PLAYER_H,
                g.player_facing,
                flash_tint(tint, g.hit_flash),
            ));
        }

        (
            tiles,
            CameraUniform::new([offset.x, offset.y], [vw, vh], ZOOM),
        )
    }
}

/// Project the ray from `rect`'s centre toward `target` onto the rectangle's
/// border, giving the point where an off-screen waypoint pins to the edge.
fn clamp_to_rect_edge(rect: egui::Rect, target: egui::Pos2) -> egui::Pos2 {
    let origin = rect.center();
    let d = target - origin;
    let half_x = (rect.width() * 0.5).max(1.0);
    let half_y = (rect.height() * 0.5).max(1.0);
    let tx = if d.x.abs() > 1e-3 {
        half_x / d.x.abs()
    } else {
        f32::INFINITY
    };
    let ty = if d.y.abs() > 1e-3 {
        half_y / d.y.abs()
    } else {
        f32::INFINITY
    };
    origin + d * tx.min(ty)
}

/// A vivid colour for a freshly-dropped waypoint, seeded from the wall clock so
/// successive waypoints get visibly different hues. The colour rides along to the
/// server and is stored with the waypoint, so it never changes afterwards.
fn random_waypoint_color() -> [f32; 3] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut h = nanos.wrapping_mul(2_654_435_761);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 13;
    let hue = (h & 0xFFFF) as f32 / 65535.0;
    hsv_to_rgb(hue, 0.65, 0.95)
}

/// Convert HSV (each component in `0..=1`) to RGB in `0..=1`.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h6 = (h.fract() * 6.0).rem_euclid(6.0);
    let i = h6.floor() as i32;
    let f = h6 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// Append a two-quad (red fill over dark backing) health bar centered above an
/// entity of width `w` whose top-left is at `(x, y)`.
fn push_health_bar(
    tiles: &mut Vec<TileInstance>,
    white: UvRect,
    x: f32,
    y: f32,
    w: f32,
    health: i32,
    max_health: i32,
) {
    const BAR_H: f32 = 1.5;
    const PAD: f32 = 2.0;
    let frac = (health as f32 / max_health as f32).clamp(0.0, 1.0);
    let bx = x - PAD * 0.5;
    let bw = w + PAD;
    let by = y - BAR_H - 1.0;
    tiles.push(flat_quad(white, bx, by, bw, BAR_H, [0.15, 0.0, 0.0, 0.85]));
    tiles.push(flat_quad(
        white,
        bx,
        by,
        bw * frac,
        BAR_H,
        [0.85, 0.1, 0.1, 0.95],
    ));
}

/// A compact "in → out" line for a recipe, e.g. `1 log → 1 wood, 4 bark`.
fn recipe_summary(registry: &BlockRegistry, recipe: &crate::recipe::Recipe) -> String {
    let names = |items: &[(BlockId, u32)]| {
        items
            .iter()
            .map(|(item, n)| format!("{} {}", n, registry.get(*item).name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("{} → {}", names(recipe.inputs), names(recipe.outputs))
}

/// Hover text spelling out a recipe's inputs and outputs.
fn recipe_tooltip(registry: &BlockRegistry, recipe: &crate::recipe::Recipe) -> String {
    format!("{}\n{}", recipe.name, recipe_summary(registry, recipe))
}

/// Draw one inventory/hotbar slot: a framed cell with the item's sprite, its
/// stack count, and an optional key number. `highlight` marks a pending move
/// source; `selected` marks the active hotbar slot. Hovering a filled slot
/// shows the item's name. Returns the click response.
fn slot_widget(
    ui: &mut egui::Ui,
    registry: &BlockRegistry,
    atlas: &Atlas,
    tex: egui::TextureId,
    slot: Slot,
    key: Option<usize>,
    highlight: bool,
    selected: bool,
) -> egui::Response {
    const SIZE: f32 = 40.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let radius = egui::CornerRadius::same(3);

    let bg = if selected {
        egui::Color32::from_rgb(70, 64, 36)
    } else {
        egui::Color32::from_rgb(36, 36, 42)
    };
    painter.rect_filled(rect, radius, bg);

    if let Some((block, count, dur)) = slot {
        // Draw the block/item's actual sprite from its cell in the atlas.
        let uv = atlas.block(block);
        painter.image(
            tex,
            rect.shrink(6.0),
            egui::Rect::from_min_max(
                egui::pos2(uv.min[0], uv.min[1]),
                egui::pos2(uv.max[0], uv.max[1]),
            ),
            egui::Color32::WHITE,
        );
        // A worn tool shows a durability bar across its bottom edge, green when
        // fresh and shading to red as it nears breaking.
        let max = crate::block::max_durability(block);
        if max > 0 && dur < max {
            let frac = dur as f32 / max as f32;
            let track = egui::Rect::from_min_max(
                rect.left_bottom() + egui::vec2(4.0, -7.0),
                rect.right_bottom() + egui::vec2(-4.0, -4.0),
            );
            painter.rect_filled(track, 1, egui::Color32::from_black_alpha(180));
            let fill = egui::Rect::from_min_max(
                track.min,
                egui::pos2(track.min.x + track.width() * frac, track.max.y),
            );
            let color = egui::Color32::from_rgb(
                (220.0 * (1.0 - frac) + 40.0 * frac) as u8,
                (200.0 * frac + 40.0 * (1.0 - frac)) as u8,
                50,
            );
            painter.rect_filled(fill, 1, color);
        }
        if count > 1 {
            let anchor = egui::Align2::RIGHT_BOTTOM;
            let font = egui::FontId::proportional(12.0);
            let label = count.to_string();
            // A dark drop-shadow keeps the count legible over busy sprites.
            painter.text(
                rect.right_bottom() - egui::vec2(2.0, 1.0),
                anchor,
                &label,
                font.clone(),
                egui::Color32::from_black_alpha(200),
            );
            painter.text(
                rect.right_bottom() - egui::vec2(3.0, 2.0),
                anchor,
                &label,
                font,
                egui::Color32::WHITE,
            );
        }
    }
    if let Some(k) = key {
        painter.text(
            rect.left_top() + egui::vec2(3.0, 1.0),
            egui::Align2::LEFT_TOP,
            (k + 1).to_string(),
            egui::FontId::proportional(10.0),
            egui::Color32::from_gray(190),
        );
    }

    let (width, color) = if highlight {
        (2.0, egui::Color32::from_rgb(240, 220, 120))
    } else if selected {
        (2.0, egui::Color32::from_rgb(200, 180, 90))
    } else {
        (1.0, egui::Color32::from_gray(90))
    };
    painter.rect_stroke(
        rect,
        radius,
        egui::Stroke::new(width, color),
        egui::StrokeKind::Inside,
    );

    // Name the item on hover (empty slots have nothing to show); a tool also
    // shows its remaining durability.
    match slot {
        Some((block, _, dur)) => {
            let name = item_display_name(registry.get(block).name);
            let max = crate::block::max_durability(block);
            let label = if max > 0 {
                format!("{name} ({dur}/{max})")
            } else {
                name
            };
            resp.on_hover_text(label)
        }
        None => resp,
    }
}

/// Turn a block/item id name like `iron_pickaxe` into a display label like
/// `Iron Pickaxe` for tooltips.
fn item_display_name(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A `:token:` chat icon resolved to the art it should draw.
enum ChatIcon {
    /// A block or item, drawn from its atlas tile.
    Block(BlockId),
    /// An entity, drawn from frame 0 of its animation sheet.
    Sprite(&'static str),
}

/// Resolve a `:token:` to a block/item tile or an entity sprite, or `None` if the
/// token names nothing drawable (so it should render as literal text).
fn resolve_chat_icon(registry: &BlockRegistry, token: &str) -> Option<ChatIcon> {
    if let Some(def) = registry.iter().find(|d| d.visible && d.name == token) {
        return Some(ChatIcon::Block(def.id));
    }
    CHAT_ENTITY_ICONS
        .iter()
        .find(|name| **name == token)
        .map(|name| ChatIcon::Sprite(name))
}

/// Draw one chat line: the sender's name (tinted by a stable per-name color),
/// then the message body with any `:icon:` tokens replaced by their sprites.
fn render_chat_line(
    ui: &mut egui::Ui,
    registry: &BlockRegistry,
    atlas: &Atlas,
    tex: egui::TextureId,
    from: &str,
    text: &str,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 1.0);
        ui.label(
            egui::RichText::new(format!("{from}:"))
                .strong()
                .color(chat_name_color(from)),
        );
        render_chat_body(ui, registry, atlas, tex, text);
    });
}

/// Render a message body, swapping each `:token:` that names a block, item, or
/// entity for its sprite and leaving everything else as text. An unmatched or
/// malformed `:...:` is left verbatim.
fn render_chat_body(
    ui: &mut egui::Ui,
    registry: &BlockRegistry,
    atlas: &Atlas,
    tex: egui::TextureId,
    text: &str,
) {
    let mut rest = text;
    while !rest.is_empty() {
        let Some(open) = rest.find(':') else {
            ui.label(rest);
            break;
        };
        // Emit any text preceding the colon.
        if open > 0 {
            ui.label(&rest[..open]);
        }
        let after = &rest[open + 1..];
        match after.find(':') {
            Some(close_rel) => {
                let token = &after[..close_rel];
                let is_token = !token.is_empty()
                    && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if is_token && let Some(icon) = resolve_chat_icon(registry, token) {
                    add_chat_icon(ui, atlas, tex, icon, token);
                    rest = &after[close_rel + 1..];
                    continue;
                }
                // Not a drawable token: emit the literal ':' and resume right after
                // it, so a later ':' can still open a real token.
                ui.label(":");
                rest = after;
            }
            // No closing colon: the remainder (including this ':') is plain text.
            None => {
                ui.label(&rest[open..]);
                break;
            }
        }
    }
}

/// Draw a single inline chat icon (16px) from the atlas, named on hover.
fn add_chat_icon(
    ui: &mut egui::Ui,
    atlas: &Atlas,
    tex: egui::TextureId,
    icon: ChatIcon,
    token: &str,
) {
    let uv = match icon {
        ChatIcon::Block(id) => atlas.block(id),
        ChatIcon::Sprite(name) => atlas.sprite_frame(name, 0),
    };
    let img = egui::Image::new(egui::load::SizedTexture::new(tex, egui::vec2(16.0, 16.0))).uv(
        egui::Rect::from_min_max(
            egui::pos2(uv.min[0], uv.min[1]),
            egui::pos2(uv.max[0], uv.max[1]),
        ),
    );
    ui.add(img).on_hover_text(item_display_name(token));
}

/// A stable, readable color for a chat sender's name, chosen from a small palette
/// by hashing the name so a given player keeps the same color.
fn chat_name_color(name: &str) -> egui::Color32 {
    const PALETTE: [egui::Color32; 8] = [
        egui::Color32::from_rgb(0xf2, 0x8b, 0x82),
        egui::Color32::from_rgb(0xf7, 0xc6, 0x6b),
        egui::Color32::from_rgb(0xb6, 0xe3, 0x7a),
        egui::Color32::from_rgb(0x7a, 0xd1, 0xc4),
        egui::Color32::from_rgb(0x8a, 0xb4, 0xf8),
        egui::Color32::from_rgb(0xc5, 0x9b, 0xf0),
        egui::Color32::from_rgb(0xf2, 0x9d, 0xd0),
        egui::Color32::from_rgb(0xd7, 0xcc, 0xb0),
    ];
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    PALETTE[(h as usize) % PALETTE.len()]
}

// --- Free functions (kept out of `&mut self` to ease borrow checking) ----

/// Paint the player's HUD health bar: a red fill over a dark backing with a
/// `♥ current / max` readout.
fn health_bar(ui: &mut egui::Ui, health: i32, max_health: i32) {
    let frac = if max_health > 0 {
        (health as f32 / max_health as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(130.0, 16.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let radius = egui::CornerRadius::same(3);
    painter.rect_filled(rect, radius, egui::Color32::from_rgb(40, 12, 12));
    let mut fill = rect;
    fill.set_width(rect.width() * frac);
    painter.rect_filled(fill, radius, egui::Color32::from_rgb(200, 40, 40));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("♥ {health} / {max_health}"),
        egui::FontId::proportional(12.0),
        egui::Color32::WHITE,
    );
}

/// Build a textured quad for an entity, mirroring the sprite horizontally when
/// it faces left (the shader interpolates uv across the quad, so swapping the U
/// bounds flips it).
fn entity_instance(
    uv: UvRect,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    facing_right: bool,
    tint: [f32; 4],
) -> TileInstance {
    let (u0, u1) = if facing_right {
        (uv.min[0], uv.max[0])
    } else {
        (uv.max[0], uv.min[0])
    };
    TileInstance {
        pos: [x, y],
        size: [w, h],
        uv_min: [u0, uv.min[1]],
        uv_max: [u1, uv.max[1]],
        color: tint,
    }
}

/// Build a solid-color flat quad (using the atlas's white cell) for overlays
/// like the mining indicator and entity health bars.
/// Tint an entity red in proportion to its remaining hit-flash. `base` is the
/// normal (daylight) tint; as `flash` approaches [`HIT_FLASH_TIME`] the green and
/// blue channels are crushed so the sprite reads as a red flash, fading back to
/// `base` as the timer runs out.
fn flash_tint(base: [f32; 4], flash: f32) -> [f32; 4] {
    if flash <= 0.0 {
        return base;
    }
    let k = (flash / HIT_FLASH_TIME).clamp(0.0, 1.0);
    let fade = 1.0 - 0.85 * k;
    [base[0], base[1] * fade, base[2] * fade, base[3]]
}

fn flat_quad(white: UvRect, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) -> TileInstance {
    TileInstance {
        pos: [x, y],
        size: [w, h],
        uv_min: white.min,
        uv_max: white.max,
        color,
    }
}

/// Advance the local player one physics step. Returns fall damage (hit points)
/// to apply if the player just landed from a damaging fall.
fn step_physics(game: &mut GameState, reg: &BlockRegistry, input: &Input, dt: f32) -> Option<i32> {
    let was_grounded = game.on_ground;

    // Horizontal intent, plus any lingering knockback shove (which decays).
    let dir = (input.right as i32 - input.left as i32) as f32;
    game.vel.x = dir * MOVE_SPEED + game.knockback_x;
    game.knockback_x *= (1.0 - KNOCKBACK_DAMP * dt).max(0.0);
    if game.knockback_x.abs() < 1.0 {
        game.knockback_x = 0.0;
    }
    if dir > 0.0 {
        game.player_facing = true;
    } else if dir < 0.0 {
        game.player_facing = false;
    }

    // Creator-mode flight: no gravity; rise/fall from the jump/down inputs. Blocks
    // still stop the player (fly, not noclip), and fall damage never applies. The
    // scroll-wheel multiplier scales both horizontal and vertical flight speed.
    if game.creator && game.fly {
        let mult = game.fly_speed_mult;
        game.vel.x *= mult;
        game.vel.y = (input.down as i32 - input.jump as i32) as f32 * FLY_SPEED * mult;
        move_x(game, reg, game.vel.x * dt);
        let landed = move_y(game, reg, game.vel.y * dt);
        game.on_ground = landed && game.vel.y >= 0.0;
        game.air_min_y = game.pos.y;
        return None;
    }

    // Boating: while riding a boat over water the player sits on the surface and
    // glides faster than walking instead of swimming. Buoyancy eases them to a
    // floating waterline (so a boat dropped into deep water bobs up to the top), the
    // fall apex resets so landing afloat never hurts, and the surface counts as
    // solid footing. Overrides swimming.
    if game.boating {
        if let Some(surface) = water_surface_y(game) {
            // Glide across the surface, faster than walking and far faster than a
            // swim; the float spring keeps the hull riding on the waterline.
            game.vel.x *= BOAT_SPEED_MULT;
            let target = surface - PLAYER_H * 0.6;
            game.vel.y = ((target - game.pos.y) * BOAT_FLOAT_STIFFNESS)
                .clamp(-BOAT_FLOAT_MAX_SPEED, BOAT_FLOAT_MAX_SPEED);
            move_x(game, reg, game.vel.x * dt);
            move_y(game, reg, game.vel.y * dt);
            game.on_ground = true;
            game.air_min_y = game.pos.y;
            return None;
        }
        // A boat carried onto dry land is dead weight: shuffle it along slowly and
        // let the normal jump/gravity arc below carry the player (so they can still
        // walk off a beach), but never at a fast land pace.
        game.vel.x *= BOAT_LAND_DRAG;
    }

    // Riding a horse: a gallop carries the rider faster than running. Only the
    // horizontal speed changes — the player still jumps and falls on the normal arc
    // below, and the horse is glued beneath them server-side — so we just scale the
    // intent and fall through to the ordinary ground physics.
    if game.riding.is_some() {
        game.vel.x *= HORSE_RIDE_SPEED_MULT;
    }

    // Ladder climbing: while overlapping a ladder the player clings to it,
    // ignoring gravity, and moves vertically with the jump (up) and down inputs.
    // This overrides the normal jump/gravity arc below.
    if player_on_ladder(game) {
        let climb = (input.down as i32 - input.jump as i32) as f32;
        game.vel.y = climb * CLIMB_SPEED;
        move_x(game, reg, game.vel.x * dt);
        let landed = move_y(game, reg, game.vel.y * dt);
        // Standing on solid footing at the foot of the ladder still counts as
        // grounded, so stepping off and walking away works normally.
        game.on_ground = landed && game.vel.y >= 0.0;
        // Reset the fall apex so dropping off a ladder never deals fall damage
        // for the climb itself.
        game.air_min_y = game.pos.y;
        return None;
    }

    // Swimming: while the player's body overlaps water it floats instead of
    // falling — buoyancy nearly cancels gravity, horizontal movement is dragged,
    // and jump/down paddle up and down. Water also cushions any fall, so an
    // entering plunge deals no fall damage. Overrides the jump/gravity arc below.
    if player_in_water(game) {
        game.vel.x *= WATER_DRAG;
        if input.jump {
            game.vel.y = -SWIM_SPEED;
        } else if input.down {
            game.vel.y = SWIM_SPEED;
        } else {
            // Buoyancy eases the vertical speed toward a slow, drifting sink.
            game.vel.y = (game.vel.y + WATER_GRAVITY * dt).clamp(-SWIM_SPEED, WATER_SINK_SPEED);
        }
        move_x(game, reg, game.vel.x * dt);
        let landed = move_y(game, reg, game.vel.y * dt);
        // Cap the rise at the waterline over *open* water: a swimmer treads water at
        // the surface and can't launch out of the top to skim across. At a shore
        // (solid ground alongside) the cap lifts so they can rise and climb out onto
        // the bank — the only way out of the water on foot. (Boats glide on the
        // surface, handled above.)
        if !near_shore(game, reg)
            && let Some(surface) = water_surface_y(game)
        {
            let min_y = surface - SWIM_SURFACE_POKE;
            if game.pos.y < min_y {
                game.pos.y = min_y;
                game.vel.y = game.vel.y.max(0.0);
            }
        }
        // Resting on the sea floor still counts as grounded so stepping back out
        // onto dry land walks normally.
        game.on_ground = landed && game.vel.y >= 0.0;
        // Water breaks the fall: never carry fall damage through it.
        game.air_min_y = game.pos.y;
        return None;
    }

    // Jump (only when grounded).
    if input.jump && game.on_ground {
        game.vel.y = JUMP_VELOCITY;
        game.on_ground = false;
    }

    // Gravity.
    game.vel.y = (game.vel.y + GRAVITY * dt).min(900.0);

    // Move and resolve on each axis independently.
    move_x(game, reg, game.vel.x * dt);
    let landed = move_y(game, reg, game.vel.y * dt);
    game.on_ground = landed;

    // Track the apex of any airborne arc so a fall is measured from its top.
    if !game.on_ground {
        game.air_min_y = game.air_min_y.min(game.pos.y);
        return None;
    }

    // Just landed: convert the drop beyond the safe distance into damage.
    let mut damage = None;
    if !was_grounded {
        let fall_tiles = (game.pos.y - game.air_min_y) / TILE_SIZE;
        let over = fall_tiles - SAFE_FALL_TILES;
        if over > 0.0 {
            damage = Some((over * FALL_DAMAGE_PER_TILE).floor() as i32);
        }
    }
    game.air_min_y = game.pos.y;
    damage
}

const EPS: f32 = 0.01;

fn move_x(game: &mut GameState, reg: &BlockRegistry, dx: f32) {
    if dx == 0.0 {
        return;
    }
    let new_x = game.pos.x + dx;
    let y0 = (game.pos.y / TILE_SIZE).floor() as i32;
    let y1 = ((game.pos.y + PLAYER_H - EPS) / TILE_SIZE).floor() as i32;
    if dx > 0.0 {
        let tx = ((new_x + PLAYER_W - EPS) / TILE_SIZE).floor() as i32;
        if column_solid(game, reg, tx, y0, y1) {
            game.pos.x = tx as f32 * TILE_SIZE - PLAYER_W;
            game.vel.x = 0.0;
            return;
        }
    } else {
        let tx = (new_x / TILE_SIZE).floor() as i32;
        if column_solid(game, reg, tx, y0, y1) {
            game.pos.x = (tx + 1) as f32 * TILE_SIZE;
            game.vel.x = 0.0;
            return;
        }
    }
    game.pos.x = new_x;
}

/// Returns whether the player is resting on the ground after the move.
fn move_y(game: &mut GameState, reg: &BlockRegistry, dy: f32) -> bool {
    if dy == 0.0 {
        return game.on_ground;
    }
    let new_y = game.pos.y + dy;
    let x0 = (game.pos.x / TILE_SIZE).floor() as i32;
    let x1 = ((game.pos.x + PLAYER_W - EPS) / TILE_SIZE).floor() as i32;
    if dy > 0.0 {
        let ty = ((new_y + PLAYER_H - EPS) / TILE_SIZE).floor() as i32;
        if row_solid(game, reg, ty, x0, x1) {
            game.pos.y = ty as f32 * TILE_SIZE - PLAYER_H;
            game.vel.y = 0.0;
            return true;
        }
    } else {
        let ty = (new_y / TILE_SIZE).floor() as i32;
        if row_solid(game, reg, ty, x0, x1) {
            game.pos.y = (ty + 1) as f32 * TILE_SIZE;
            game.vel.y = 0.0;
            return false;
        }
    }
    game.pos.y = new_y;
    false
}

/// Whether a ladder may be placed at `(tx, ty)` — it must mount on the side of a
/// wall (a solid block to the left or right) or extend a ladder run directly
/// below an existing one. A rope ladder may additionally hang from a solid block
/// directly above (anchored at a shaft's mouth). Non-ladder blocks are
/// unaffected. Mirrors the authoritative check in [`crate::server`].
fn ladder_supported(
    game: &GameState,
    reg: &BlockRegistry,
    block: BlockId,
    tx: i32,
    ty: i32,
) -> bool {
    if !crate::block::is_climbable(block) {
        return true;
    }
    reg.is_solid(game.world.get_block(tx - 1, ty))
        || reg.is_solid(game.world.get_block(tx + 1, ty))
        || crate::block::is_climbable(game.world.get_block(tx, ty - 1))
        || (crate::block::is_rope_ladder(block) && reg.is_solid(game.world.get_block(tx, ty - 1)))
}

/// Whether a door may be placed at `(tx, ty)`: it stands two cells tall, so the
/// cell directly above the target must also be empty and clear of the player.
/// Non-door blocks are unaffected. Mirrors the authoritative check in
/// [`crate::server`].
fn door_clear_above(game: &GameState, block: BlockId, tx: i32, ty: i32) -> bool {
    if block != crate::block::DOOR {
        return true;
    }
    game.world.get_block(tx, ty - 1) == AIR && !overlaps_player(game, tx, ty - 1)
}

/// Whether the player's body currently overlaps any ladder cell — the condition
/// for climbing instead of falling.
fn player_on_ladder(game: &GameState) -> bool {
    let x0 = (game.pos.x / TILE_SIZE).floor() as i32;
    let x1 = ((game.pos.x + PLAYER_W - EPS) / TILE_SIZE).floor() as i32;
    let y0 = (game.pos.y / TILE_SIZE).floor() as i32;
    let y1 = ((game.pos.y + PLAYER_H - EPS) / TILE_SIZE).floor() as i32;
    (y0..=y1).any(|ty| (x0..=x1).any(|tx| crate::block::is_climbable(game.world.get_block(tx, ty))))
}

/// Whether the player is swimming: their body overlaps a water cell, or water sits
/// just below their feet (within [`WATER_SURFACE_CLING`]). The downward margin
/// keeps a swimmer bobbing at the surface in the slow swim state instead of letting
/// them pop a pixel into open air and travel at full land speed — the old skim.
fn player_in_water(game: &GameState) -> bool {
    let x0 = (game.pos.x / TILE_SIZE).floor() as i32;
    let x1 = ((game.pos.x + PLAYER_W - EPS) / TILE_SIZE).floor() as i32;
    let y0 = (game.pos.y / TILE_SIZE).floor() as i32;
    let y1 = ((game.pos.y + PLAYER_H - EPS + WATER_SURFACE_CLING) / TILE_SIZE).floor() as i32;
    (y0..=y1).any(|ty| (x0..=x1).any(|tx| crate::block::is_water(game.world.get_block(tx, ty))))
}

/// Whether the swimmer is up against a shore: a solid block in the column just to
/// the player's left or right, around foot level. That's a bank to climb onto, so
/// the surface rise-cap lifts here (letting them clamber out) while staying clamped
/// over open water (so they can't skim across the top). See [`step_physics`].
fn near_shore(game: &GameState, reg: &BlockRegistry) -> bool {
    let left = ((game.pos.x - EPS) / TILE_SIZE).floor() as i32;
    let right = ((game.pos.x + PLAYER_W) / TILE_SIZE).floor() as i32;
    let feet = ((game.pos.y + PLAYER_H - EPS) / TILE_SIZE).floor() as i32;
    [left, right]
        .iter()
        .any(|&tx| (feet - 1..=feet + 1).any(|ty| reg.is_solid(game.world.get_block(tx, ty))))
}

/// World-pixel y of the water surface a boating player rides on: the top edge of
/// the highest water cell in the player's centre column anywhere from just above
/// their head down to just below their feet, or `None` if there's no water there
/// (e.g. a boat carried onto land). Scanning the whole body — not just the midline
/// — means a boat floating high on the surface is still detected, so it keeps
/// gliding instead of dropping into the swim code. Used to settle the boat onto the
/// surface (see [`step_physics`]).
fn water_surface_y(game: &GameState) -> Option<f32> {
    let cx = ((game.pos.x + PLAYER_W * 0.5) / TILE_SIZE).floor() as i32;
    let top = ((game.pos.y - TILE_SIZE) / TILE_SIZE).floor() as i32;
    let foot = ((game.pos.y + PLAYER_H - EPS + WATER_SURFACE_CLING) / TILE_SIZE).floor() as i32;
    (top..=foot)
        .find(|&ty| crate::block::is_water(game.world.get_block(cx, ty)))
        .map(|ty| ty as f32 * TILE_SIZE)
}

fn column_solid(game: &GameState, reg: &BlockRegistry, tx: i32, y0: i32, y1: i32) -> bool {
    (y0..=y1).any(|ty| reg.is_solid(game.world.get_block(tx, ty)))
}

fn row_solid(game: &GameState, reg: &BlockRegistry, ty: i32, x0: i32, x1: i32) -> bool {
    (x0..=x1).any(|tx| reg.is_solid(game.world.get_block(tx, ty)))
}

/// World-pixel point the camera centers on and chunk streaming tracks: while
/// spectating (admin `/spectate`) it is the watched player's center, so the view
/// follows them; otherwise it is the local avatar's center. Falls back to the
/// avatar if the spectated entity isn't currently mirrored.
fn view_center(game: &GameState) -> Vec2 {
    if let Some(tid) = game.spectating
        && let Some(e) = game.entities.get(tid)
    {
        return Vec2::new(e.x, e.y) + Vec2::new(PLAYER_W * 0.5, PLAYER_H * 0.5);
    }
    game.pos + Vec2::new(PLAYER_W * 0.5, PLAYER_H * 0.5)
}

fn request_chunks(game: &mut GameState, gfx: Option<&Gfx>, net: Option<&NetHandle>) {
    let (Some(gfx), Some(net)) = (gfx, net) else {
        return;
    };
    let view_w = gfx.size.width.max(1) as f32 / ZOOM;
    let view_h = gfx.size.height.max(1) as f32 / ZOOM;
    let center = view_center(game);
    let min = center - Vec2::new(view_w * 0.5, view_h * 0.5);
    let max = center + Vec2::new(view_w * 0.5, view_h * 0.5);

    let (cmin, _) = to_chunk(
        (min.x / TILE_SIZE).floor() as i32,
        (min.y / TILE_SIZE).floor() as i32,
    );
    let (cmax, _) = to_chunk(
        (max.x / TILE_SIZE).ceil() as i32,
        (max.y / TILE_SIZE).ceil() as i32,
    );

    for cy in (cmin.1 - CHUNK_MARGIN)..=(cmax.1 + CHUNK_MARGIN) {
        for cx in (cmin.0 - CHUNK_MARGIN)..=(cmax.0 + CHUNK_MARGIN) {
            if cy < 0 || cy * CHUNK_SIZE >= WORLD_HEIGHT {
                continue;
            }
            if game.requested.insert((cx, cy)) {
                let _ = net.commands.send(NetCommand::RequestChunk {
                    dim: game.dim,
                    cx,
                    cy,
                });
            }
        }
    }
}

fn handle_block_actions(
    game: &mut GameState,
    reg: &BlockRegistry,
    input: &Input,
    gfx: Option<&Gfx>,
    net: Option<&NetHandle>,
    dt: f32,
) {
    game.action_timer -= dt;

    // Open menus (inventory, forge, campfire) capture the mouse for their own UI;
    // don't mine or place in the world while one is open.
    if game.inventory_open || game.forge_open || game.campfire_open {
        game.break_target = None;
        game.break_progress = 0.0;
        return;
    }

    let (Some(gfx), Some(net)) = (gfx, net) else {
        return;
    };

    // Mouse (physical px) -> world point + cell.
    let view_w = gfx.size.width.max(1) as f32 / ZOOM;
    let view_h = gfx.size.height.max(1) as f32 / ZOOM;
    let center = game.pos + Vec2::new(PLAYER_W * 0.5, PLAYER_H * 0.5);
    let offset = center - Vec2::new(view_w * 0.5, view_h * 0.5);
    let world = offset + Vec2::new(input.mouse.0 / ZOOM, input.mouse.1 / ZOOM);
    let tx = (world.x / TILE_SIZE).floor() as i32;
    let ty = (world.y / TILE_SIZE).floor() as i32;

    // Rising edges (this frame pressed, last frame not) for the creator tools,
    // where a click should fire once rather than repeat while held.
    let lmb_edge = input.breaking && !game.sel_prev_lmb;
    let rmb_edge = input.placing && !game.sel_prev_rmb;
    game.sel_prev_lmb = input.breaking;
    game.sel_prev_rmb = input.placing;

    // Creator structure tools intercept the mouse before normal mining/placing.
    if game.creator
        && creator_structure_input(game, net, tx, ty, lmb_edge, rmb_edge, input.breaking)
    {
        return;
    }

    // Left button: swing at a creature under the cursor, else mine the block.
    if input.breaking {
        if let Some(target) = creature_at(game, world) {
            game.break_target = None;
            game.break_progress = 0.0;
            if game.action_timer <= 0.0 {
                let held = game
                    .inventory
                    .get(game.selected_slot)
                    .map(|(b, _, _)| b)
                    .unwrap_or(AIR);
                let _ = net.commands.send(NetCommand::Attack { target, held });
                game.action_timer = ACTION_COOLDOWN;
            }
        } else {
            mine_block(game, reg, net, tx, ty, dt);
        }
    } else {
        game.break_target = None;
        game.break_progress = 0.0;
    }

    // Right-clicking mounts or dismounts a horse, taking priority over placing a
    // block or opening a cell GUI. While mounted, any right-click dismounts; while on
    // foot, right-clicking your own tamed horse within reach mounts it. The mount is
    // confirmed authoritatively by the server (which validates ownership and reach),
    // so we only send the request — the riding state is adopted from its echo.
    if input.placing && game.action_timer <= 0.0 {
        if game.riding.is_some() {
            let _ = net.commands.send(NetCommand::SetRiding { horse: None });
            game.action_timer = ACTION_COOLDOWN;
            return;
        }
        if let Some(target) = creature_at(game, world)
            && game
                .entities
                .get(target)
                .is_some_and(|e| matches!(e.kind, EntityKind::Horse { owner: Some(_) }))
        {
            let _ = net.commands.send(NetCommand::SetRiding {
                horse: Some(target),
            });
            game.action_timer = ACTION_COOLDOWN;
            return;
        }
    }

    // Right-clicking a forge opens its smelting GUI instead of placing a block.
    // This takes priority over both creator-mode and normal placement.
    if input.placing
        && game.action_timer <= 0.0
        && game.world.get_block(tx, ty) == crate::block::FORGE
        && cell_in_reach(game, tx, ty)
    {
        game.forge_open = true;
        game.forge_cell = Some((tx, ty));
        game.action_timer = ACTION_COOLDOWN;
        return;
    }

    // Right-clicking a campfire (lit or not) opens its GUI, for fueling and
    // cooking, instead of placing a block.
    if input.placing
        && game.action_timer <= 0.0
        && crate::block::is_campfire(game.world.get_block(tx, ty))
        && cell_in_reach(game, tx, ty)
    {
        game.campfire_open = true;
        game.campfire_cell = Some((tx, ty));
        game.action_timer = ACTION_COOLDOWN;
        // Interacting with a campfire makes it this player's respawn point.
        let _ = net.commands.send(NetCommand::SetRespawn { x: tx, y: ty });
        return;
    }

    // Right-clicking a door swings it open or shut instead of placing a block.
    // The door spans two cells; we flip both halves optimistically and let the
    // server (authoritative over both) confirm or correct.
    if input.placing
        && game.action_timer <= 0.0
        && crate::block::is_door(game.world.get_block(tx, ty))
        && cell_in_reach(game, tx, ty)
    {
        // Anchor on the lower half: if the cursor is on a top, the lower half is
        // the cell below. The upper half is always directly above the lower one.
        let by = if crate::block::is_door_bottom(game.world.get_block(tx, ty)) {
            ty
        } else {
            ty + 1
        };
        let opening = game.world.get_block(tx, by) == crate::block::DOOR;
        let (lower, upper) = if opening {
            (crate::block::DOOR_OPEN, crate::block::DOOR_OPEN_TOP)
        } else {
            (crate::block::DOOR, crate::block::DOOR_TOP)
        };
        game.world.set_block(tx, by, lower);
        game.world.set_block(tx, by - 1, upper);
        let _ = net.commands.send(NetCommand::ToggleDoor { x: tx, y: ty });
        game.action_timer = ACTION_COOLDOWN;
        return;
    }

    // Right-clicking with a bucket scoops or pours water — a special use, not a
    // normal block placement. An empty bucket fills from a water cell; a water
    // bucket empties into an open cell. The server is authoritative over both the
    // block and the inventory swap; we update optimistically and let it correct.
    if input.placing && game.action_timer <= 0.0 && cell_in_reach(game, tx, ty) {
        let slot = game.selected_slot;
        if let Some((held, _, _)) = game
            .inventory
            .get(slot)
            .filter(|(b, _, _)| crate::block::is_bucket(*b))
        {
            let cell = game.world.get_block(tx, ty);
            let used = if held == crate::block::BUCKET && crate::block::is_water(cell) {
                game.world.set_block(tx, ty, AIR);
                true
            } else if held == crate::block::WATER_BUCKET
                && cell == AIR
                && (0..WORLD_HEIGHT).contains(&ty)
                && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                    .iter()
                    .any(|(dx, dy)| game.world.get_block(tx + dx, ty + dy) != AIR)
            {
                game.world.set_block(tx, ty, crate::block::WATER);
                true
            } else {
                false
            };
            if used {
                // Optimistically spend the held bucket; the server returns the
                // swapped bucket via an authoritative Inventory snapshot.
                game.inventory.take_one(slot);
                let _ = net.commands.send(NetCommand::UseBucket {
                    x: tx,
                    y: ty,
                    slot: slot as u8,
                });
                game.action_timer = ACTION_COOLDOWN;
                return;
            }
        }
    }

    // Right-clicking while holding the fire key warps the player between
    // dimensions. It acts on the player, not a target cell, so it needs no reach
    // check; the server picks the landing spot and re-streams the new dimension.
    if input.placing && game.action_timer <= 0.0 {
        let slot = game.selected_slot;
        if game
            .inventory
            .get(slot)
            .is_some_and(|(b, _, _)| crate::block::is_fire_key(b))
        {
            let _ = net
                .commands
                .send(NetCommand::UseFireKey { slot: slot as u8 });
            game.action_timer = ACTION_COOLDOWN;
            return;
        }
    }

    // Right-clicking while holding a boat climbs aboard (or steps back out). It
    // acts on the player, not a target cell, so it needs no reach check, and the
    // boat is a vehicle — never consumed — so nothing is sent to the server; the
    // riding state is purely a local movement mode (see [`step_physics`]).
    if input.placing && game.action_timer <= 0.0 {
        let slot = game.selected_slot;
        if game
            .inventory
            .get(slot)
            .is_some_and(|(b, _, _)| crate::block::is_boat(b))
        {
            game.boating = !game.boating;
            // Share the rider pose so other clients draw us in (or out of) the boat.
            let _ = net
                .commands
                .send(NetCommand::SetBoating { on: game.boating });
            game.action_timer = ACTION_COOLDOWN;
            return;
        }
    }

    // Creator mode: right button places the creator-selected block for free, with
    // no inventory cost or adjacency requirement (infinite blocks).
    if game.creator {
        if input.placing && game.action_timer <= 0.0 && (0..WORLD_HEIGHT).contains(&ty) {
            let block = game.creator_block;
            if game.world.get_block(tx, ty) == AIR && !overlaps_player(game, tx, ty) {
                game.world.set_block(tx, ty, block);
                let _ = net.commands.send(NetCommand::CreatorSetBlock {
                    x: tx,
                    y: ty,
                    block,
                });
                game.action_timer = ACTION_COOLDOWN;
            }
        }
        return;
    }

    // Right button: place the selected hotbar slot's block on a fixed cooldown,
    // but only if that slot holds something to spend.
    if input.placing && game.action_timer <= 0.0 && (0..WORLD_HEIGHT).contains(&ty) {
        let slot = game.selected_slot;
        let current = game.world.get_block(tx, ty);
        if let Some((block, _, _)) = game.inventory.get(slot)
            && reg.is_placeable(block)
            && current == AIR
            && !overlaps_player(game, tx, ty)
            && cell_in_reach(game, tx, ty)
            && ladder_supported(game, reg, block, tx, ty)
            && door_clear_above(game, block, tx, ty)
        {
            game.world.set_block(tx, ty, block);
            // Optimistically spend one; the server confirms (or corrects) with an
            // authoritative Inventory snapshot.
            game.inventory.take_one(slot);
            let _ = net.commands.send(NetCommand::PlaceBlock {
                x: tx,
                y: ty,
                slot: slot as u8,
            });
            game.action_timer = ACTION_COOLDOWN;
        }
    }
}

/// Drive the creator structure tools from the mouse. Returns `true` when it has
/// handled the click (so the caller skips normal mining/placing): always while a
/// structure is queued for pasting or the Select tool is active; `false` for the
/// Build tool, which falls through to the usual creator placement.
fn creator_structure_input(
    game: &mut GameState,
    net: &NetHandle,
    tx: i32,
    ty: i32,
    lmb_edge: bool,
    rmb_edge: bool,
    lmb_down: bool,
) -> bool {
    // A loaded structure waiting to be placed takes priority over any tool:
    // left-click stamps it with its top-left at the cursor, right-click cancels.
    if let Some(structure) = &game.pending_paste {
        if rmb_edge {
            game.pending_paste = None;
            return true;
        }
        if lmb_edge {
            let cells: Vec<(i32, i32, BlockId)> = structure
                .solid_offsets()
                .map(|(dx, dy, b)| (tx + dx, ty + dy, b))
                .filter(|&(_, y, _)| (0..WORLD_HEIGHT).contains(&y))
                .collect();
            if !cells.is_empty() {
                // Apply optimistically; the server echoes a BlocksUpdate to all
                // clients (this one included, which is idempotent).
                for &(x, y, b) in &cells {
                    game.world.set_block(x, y, b);
                }
                let _ = net.commands.send(NetCommand::CreatorSetBlocks { cells });
            }
            // Re-spawn the captured creatures relative to the stamp anchor (the
            // cursor cell). The server owns entities, so just ask it to spawn each.
            let anchor_x = tx as f32 * TILE_SIZE;
            let anchor_y = ty as f32 * TILE_SIZE;
            for se in &structure.entities {
                let _ = net.commands.send(NetCommand::SpawnEntity {
                    kind: se.kind.clone(),
                    x: anchor_x + se.dx,
                    y: anchor_y + se.dy,
                });
            }
        }
        return true;
    }

    match game.creator_tool {
        CreatorTool::Build => false,
        CreatorTool::Select => {
            // Right-click clears any selection; left-drag defines a new rectangle
            // (a fresh press anchors one corner, holding drags the opposite one).
            if rmb_edge {
                game.sel_a = None;
                game.sel_b = None;
            }
            if lmb_edge {
                game.sel_a = Some((tx, ty));
                game.sel_b = Some((tx, ty));
            } else if lmb_down && game.sel_a.is_some() {
                game.sel_b = Some((tx, ty));
            }
            true
        }
    }
}

/// Normalize an entity kind for storage in a structure: a captured pet (cat,
/// puppy, or horse) is set wild (owner cleared) so a pasted or worldgen copy
/// doesn't belong to whoever tamed the original. Other kinds are taken as-is.
fn structure_entity_kind(kind: &EntityKind) -> EntityKind {
    match kind {
        EntityKind::Cat { sitting, .. } => EntityKind::Cat {
            owner: None,
            sitting: *sitting,
        },
        EntityKind::Puppy { sitting, .. } => EntityKind::Puppy {
            owner: None,
            sitting: *sitting,
        },
        EntityKind::Horse { .. } => EntityKind::Horse { owner: None },
        other => other.clone(),
    }
}

/// The inclusive world-cell bounds `(x0, y0, x1, y1)` of the current creator
/// selection, if both corners are set.
fn selection_bounds(game: &GameState) -> Option<(i32, i32, i32, i32)> {
    let ((ax, ay), (bx, by)) = game.sel_a.zip(game.sel_b)?;
    Some((ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)))
}

/// Accumulate mining progress on the targeted cell, breaking it once the block's
/// [`break_secs`](crate::block::BlockDef::break_secs) delay has elapsed.
fn mine_block(
    game: &mut GameState,
    reg: &BlockRegistry,
    net: &NetHandle,
    tx: i32,
    ty: i32,
    dt: f32,
) {
    let current = if (0..WORLD_HEIGHT).contains(&ty) {
        game.world.get_block(tx, ty)
    } else {
        AIR
    };
    // Can't mine air or water (a fluid — scoop it with a bucket instead), or a
    // cell beyond melee reach.
    if current == AIR || crate::block::is_water(current) || !cell_in_reach(game, tx, ty) {
        game.break_target = None;
        game.break_progress = 0.0;
        return;
    }

    // Restart progress whenever the targeted cell changes.
    if game.break_target != Some((tx, ty)) {
        game.break_target = Some((tx, ty));
        game.break_progress = 0.0;
    }
    game.break_progress += dt;

    // The held tool scales how fast the block breaks: a pickaxe shreds stone,
    // bare hands barely scratch it.
    let held = game
        .inventory
        .get(game.selected_slot)
        .map(|(b, _, _)| b)
        .unwrap_or(AIR);
    let secs = reg.get(current).break_secs * crate::block::mine_speed_mult(current, held);

    if game.break_progress >= secs {
        game.world.set_block(tx, ty, AIR);
        let _ = net.commands.send(NetCommand::SetBlock {
            x: tx,
            y: ty,
            block: AIR,
            held,
        });
        game.break_target = None;
        game.break_progress = 0.0;
    }
}

/// Id of an attackable entity (another player or a creature, but not a dropped
/// item) whose AABB contains `world` and is within melee reach of the player, if
/// any. Our own avatar isn't in `entities`, so it can't be hit.
fn creature_at(game: &GameState, world: Vec2) -> Option<EntityId> {
    for e in game.entities.values() {
        if e.kind.is_item() {
            continue;
        }
        let (w, h) = e.size();
        let inside = world.x >= e.x && world.x <= e.x + w && world.y >= e.y && world.y <= e.y + h;
        if inside
            && aabb_gap_px(game.pos.x, game.pos.y, PLAYER_W, PLAYER_H, e.x, e.y, w, h)
                <= PLAYER_ATTACK_REACH
        {
            return Some(e.id);
        }
    }
    None
}

/// Whether cell `(tx, ty)` is close enough to place into — gated by the same
/// melee reach used for attacks, so you can only build where you could swing.
fn cell_in_reach(game: &GameState, tx: i32, ty: i32) -> bool {
    aabb_gap_px(
        game.pos.x,
        game.pos.y,
        PLAYER_W,
        PLAYER_H,
        tx as f32 * TILE_SIZE,
        ty as f32 * TILE_SIZE,
        TILE_SIZE,
        TILE_SIZE,
    ) <= PLAYER_ATTACK_REACH
}

/// Smallest gap (px) between two AABBs; `0.0` when they overlap.
fn aabb_gap_px(ax: f32, ay: f32, aw: f32, ah: f32, bx: f32, by: f32, bw: f32, bh: f32) -> f32 {
    let gx = (bx - (ax + aw)).max(ax - (bx + bw)).max(0.0);
    let gy = (by - (ay + ah)).max(ay - (by + bh)).max(0.0);
    gx.max(gy)
}

fn overlaps_player(game: &GameState, tx: i32, ty: i32) -> bool {
    let bx0 = tx as f32 * TILE_SIZE;
    let by0 = ty as f32 * TILE_SIZE;
    let bx1 = bx0 + TILE_SIZE;
    let by1 = by0 + TILE_SIZE;
    let px0 = game.pos.x;
    let py0 = game.pos.y;
    let px1 = px0 + PLAYER_W;
    let py1 = py0 + PLAYER_H;
    px0 < bx1 && px1 > bx0 && py0 < by1 && py1 > by0
}

// --- winit application handler -------------------------------------------

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Survival Cubed")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        let gfx = match pollster::block_on(Gfx::new(window.clone(), &self.atlas)) {
            Ok(g) => g,
            Err(e) => {
                log::error!("failed to init graphics: {e:#}");
                event_loop.exit();
                return;
            }
        };

        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        self.window = Some(window);
        self.gfx = Some(gfx);
        self.egui_state = Some(egui_state);
        self.last_frame = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Let egui consume the event first.
        if let (Some(state), Some(window)) = (self.egui_state.as_mut(), self.window.as_ref()) {
            let _ = state.on_window_event(window, &event);
        }

        let wants_kb = self.egui_ctx.egui_wants_keyboard_input();
        let wants_pointer = self.egui_ctx.egui_wants_pointer_input();

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.resize(size);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.input.mouse = (position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                if pressed && wants_pointer {
                    // Click landed on egui; ignore for world interaction.
                } else {
                    match button {
                        MouseButton::Left => self.input.breaking = pressed,
                        MouseButton::Right => self.input.placing = pressed,
                        _ => {}
                    }
                }
                if !pressed {
                    // Always clear on release so buttons can't get stuck.
                    match button {
                        MouseButton::Left => self.input.breaking = false,
                        MouseButton::Right => self.input.placing = false,
                        _ => {}
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // While flying in creator mode, the scroll wheel ramps fly speed up
                // and down. (When egui wants the pointer, let it scroll instead.)
                if !wants_pointer
                    && let Some(g) = self.game.as_mut()
                    && g.creator
                    && g.fly
                {
                    let notches = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                    };
                    g.fly_speed_mult = (g.fly_speed_mult + notches * FLY_MULT_STEP)
                        .clamp(FLY_MULT_MIN, FLY_MULT_MAX);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(code) = event.physical_key {
                    if wants_kb {
                        // Typing into a text field; don't drive the player.
                        self.input.left = false;
                        self.input.right = false;
                        self.input.jump = false;
                        self.input.down = false;
                    } else {
                        self.handle_key(code, pressed);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32().min(0.05);
                self.last_frame = now;
                self.anim_time += dt;

                self.poll_net();
                self.update(dt);
                self.render_frame();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl App {
    fn handle_key(&mut self, code: KeyCode, pressed: bool) {
        match code {
            KeyCode::KeyA | KeyCode::ArrowLeft => self.input.left = pressed,
            KeyCode::KeyD | KeyCode::ArrowRight => self.input.right = pressed,
            KeyCode::Space | KeyCode::KeyW | KeyCode::ArrowUp => self.input.jump = pressed,
            KeyCode::KeyS | KeyCode::ArrowDown => self.input.down = pressed,
            KeyCode::Digit1 if pressed => self.select_slot(0),
            KeyCode::Digit2 if pressed => self.select_slot(1),
            KeyCode::Digit3 if pressed => self.select_slot(2),
            KeyCode::Digit4 if pressed => self.select_slot(3),
            KeyCode::Digit5 if pressed => self.select_slot(4),
            KeyCode::Digit6 if pressed => self.select_slot(5),
            KeyCode::Digit7 if pressed => self.select_slot(6),
            KeyCode::Digit8 if pressed => self.select_slot(7),
            KeyCode::Digit9 if pressed => self.select_slot(8),
            KeyCode::KeyE if pressed => self.toggle_inventory(),
            // Q drops one item from the selected hotbar slot (to discard or gift
            // it). Shift+Q would be a whole stack, but the inventory screen's
            // right-click handles bulk drops, so Q stays a single item.
            KeyCode::KeyQ if pressed => self.drop_selected(),
            // F eats the food in the selected hotbar slot, if any.
            KeyCode::KeyF if pressed => self.eat_selected(),
            // M drops a personal waypoint at the player's feet; N removes the one
            // nearest to them.
            KeyCode::KeyM if pressed => self.add_waypoint(),
            KeyCode::KeyN if pressed => self.remove_nearest_waypoint(),
            // F2 captures a screenshot of the world (without the HUD) on the
            // next rendered frame.
            KeyCode::F2 if pressed => self.screenshot_requested = true,
            // Enter or T opens the chat box (typing is then captured by egui).
            KeyCode::Enter | KeyCode::KeyT if pressed => self.open_chat(),
            // Escape closes an open menu (inventory, forge, or campfire) if any,
            // otherwise leaves the world.
            KeyCode::Escape if pressed => {
                let menu_open = self
                    .game
                    .as_ref()
                    .is_some_and(|g| g.inventory_open || g.forge_open || g.campfire_open);
                if menu_open {
                    if let Some(g) = &mut self.game {
                        g.inventory_open = false;
                        g.move_from = None;
                        g.forge_open = false;
                        g.forge_cell = None;
                        g.campfire_open = false;
                        g.campfire_cell = None;
                    }
                } else {
                    self.leave();
                }
            }
            _ => {}
        }
    }

    /// Select hotbar slot `slot` (`0..HOTBAR_SLOTS`) as the block to place.
    fn select_slot(&mut self, slot: usize) {
        if let Some(g) = &mut self.game {
            g.selected_slot = slot;
        }
    }

    /// Drop one item from the selected hotbar slot at the player's feet, tossed
    /// in the direction they face. No-op while a menu is open (the inventory
    /// screen has its own drop affordance) or the slot is empty.
    fn drop_selected(&mut self) {
        let cmd = {
            let Some(g) = self.game.as_ref() else {
                return;
            };
            if g.inventory_open || g.forge_open || g.campfire_open {
                return;
            }
            if g.inventory.get(g.selected_slot).is_none() {
                return;
            }
            let dir = if g.player_facing { 1.0 } else { -1.0 };
            NetCommand::DropItem {
                slot: g.selected_slot as u8,
                all: false,
                dir,
            }
        };
        if let Some(net) = &self.net {
            let _ = net.commands.send(cmd);
        }
    }

    /// Eat the food in the selected hotbar slot. No-op while a menu is open, or if
    /// the slot is empty or doesn't hold food. The server applies the health
    /// change and replies with a fresh inventory snapshot.
    fn eat_selected(&mut self) {
        let cmd = {
            let Some(g) = self.game.as_ref() else {
                return;
            };
            if g.inventory_open || g.forge_open || g.campfire_open {
                return;
            }
            match g.inventory.get(g.selected_slot) {
                Some((item, _, _)) if crate::block::is_food(item) => NetCommand::Eat {
                    slot: g.selected_slot as u8,
                },
                _ => return,
            }
        };
        if let Some(net) = &self.net {
            let _ = net.commands.send(cmd);
        }
    }

    /// Drop a personal waypoint at the player's current position, drawn with a
    /// fresh random colour. The server stores it and echoes the list back. No-op
    /// while a menu is open (to keep the key from firing under a GUI).
    fn add_waypoint(&mut self) {
        let cmd = {
            let Some(g) = self.game.as_ref() else {
                return;
            };
            if g.inventory_open || g.forge_open || g.campfire_open {
                return;
            }
            NetCommand::AddWaypoint {
                x: g.pos.x,
                y: g.pos.y,
                color: random_waypoint_color(),
            }
        };
        if let Some(net) = &self.net {
            let _ = net.commands.send(cmd);
        }
    }

    /// Remove the personal waypoint nearest the player. No-op while a menu is open
    /// or the player has none. The server resyncs the list.
    fn remove_nearest_waypoint(&mut self) {
        let cmd = {
            let Some(g) = self.game.as_ref() else {
                return;
            };
            if g.inventory_open || g.forge_open || g.campfire_open {
                return;
            }
            let nearest = g.waypoints.iter().min_by(|a, b| {
                let da = g.pos.distance_squared(Vec2::new(a.x, a.y));
                let db = g.pos.distance_squared(Vec2::new(b.x, b.y));
                da.total_cmp(&db)
            });
            let Some(nearest) = nearest else {
                return;
            };
            NetCommand::RemoveWaypoint {
                x: nearest.x,
                y: nearest.y,
            }
        };
        if let Some(net) = &self.net {
            let _ = net.commands.send(cmd);
        }
    }

    /// Open the chat input box and ask it to grab keyboard focus next frame.
    fn open_chat(&mut self) {
        if let Some(g) = &mut self.game
            && !g.chat_open
        {
            g.chat_open = true;
            g.chat_focus = true;
        }
    }

    /// Open or close the inventory management screen.
    fn toggle_inventory(&mut self) {
        if let Some(g) = &mut self.game {
            g.inventory_open = !g.inventory_open;
            if !g.inventory_open {
                g.move_from = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_icons_resolve_blocks_items_and_entities() {
        let reg = BlockRegistry::new();
        // Placeable block, plain item, and crafted tool all map to atlas tiles.
        assert!(matches!(
            resolve_chat_icon(&reg, "stone"),
            Some(ChatIcon::Block(_))
        ));
        assert!(matches!(
            resolve_chat_icon(&reg, "iron_pickaxe"),
            Some(ChatIcon::Block(_))
        ));
        // Entity sheets map to a sprite.
        assert!(matches!(
            resolve_chat_icon(&reg, "zombie"),
            Some(ChatIcon::Sprite("zombie"))
        ));
        // Air is invisible and unknown tokens don't resolve (rendered as text).
        assert!(resolve_chat_icon(&reg, "air").is_none());
        assert!(resolve_chat_icon(&reg, "dragon").is_none());
    }
}
