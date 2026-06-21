//! Client application: window + event loop (winit), egui UI, input, player
//! physics, and the bridge to the networking thread.

mod net;
mod render;
mod sprite;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use glam::Vec2;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::block::{AIR, BlockRegistry};
use crate::daylight;
use crate::discovery::{DiscoveredServer, LanBrowser};
use crate::entity::{Entities, EntityId, EntityKind, PLAYER_MAX_HEALTH};
use crate::inventory::{HOTBAR_SLOTS, Inventory, STORAGE_SLOTS, Slot};
use crate::protocol::BlockId;
use crate::server::{self, RunningServer};
use crate::world::{CHUNK_SIZE, TILE_SIZE, WORLD_HEIGHT, World, to_chunk};

use net::{NetCommand, NetEvent, NetHandle, connect};
use render::{Atlas, CameraUniform, EguiFrame, Gfx, TileInstance, UvRect};

// --- Tunables ------------------------------------------------------------

const GRAVITY: f32 = 1400.0;
const MOVE_SPEED: f32 = 150.0;
const JUMP_VELOCITY: f32 = -440.0;
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
const PLAYER_ATTACK_REACH: f32 = 12.0;
/// Seconds an entity tints red after taking a hit.
const HIT_FLASH_TIME: f32 = 0.25;
/// Exponential decay rate (per second) of the player's horizontal knockback, so
/// a shove fades over roughly a quarter second.
const KNOCKBACK_DAMP: f32 = 9.0;

/// Locate a textures subdirectory: `<assets>/textures/<sub>`.
///
/// Resolution order: `$SURVIVAL_CUBED_ASSETS`, then `./assets` (next to the
/// project / working dir), then `assets` beside the executable. The first
/// existing candidate wins; otherwise the working-dir path is returned so the
/// atlas loader can create it and drop starter textures there.
fn textures_dir(sub: &str) -> PathBuf {
    if let Ok(dir) = std::env::var("SURVIVAL_CUBED_ASSETS") {
        return PathBuf::from(dir).join("textures").join(sub);
    }
    let mut candidates = vec![PathBuf::from("assets").join("textures").join(sub)];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(parent.join("assets").join("textures").join(sub));
        }
    }
    candidates
        .iter()
        .find(|c| c.exists())
        .cloned()
        .unwrap_or_else(|| candidates.remove(0))
}

fn blocks_texture_dir() -> PathBuf {
    textures_dir("blocks")
}

fn entities_texture_dir() -> PathBuf {
    textures_dir("entities")
}

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
    world: World,
    /// All other entities (remote players and server creatures), mirrored from
    /// the server.
    entities: Entities,
    /// Facing per remote entity (`true` = right), remembered while it is idle.
    facing: HashMap<EntityId, bool>,
    /// Facing of this client's own player avatar.
    player_facing: bool,
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
}

impl GameState {
    fn new(entity_id: EntityId, spawn: Vec2) -> Self {
        GameState {
            entity_id,
            pos: spawn,
            vel: Vec2::ZERO,
            on_ground: false,
            world: World::new(),
            entities: Entities::new(),
            facing: HashMap::new(),
            player_facing: true,
            requested: HashSet::new(),
            selected_slot: 0,
            inventory: Inventory::new(),
            inventory_open: false,
            move_from: None,
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
        }
    }
}

struct App {
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,

    registry: Arc<BlockRegistry>,
    atlas: Atlas,

    screen: Screen,
    status: String,
    address_input: String,
    port_input: String,
    /// Name typed in the "New world" form.
    world_name_input: String,
    /// Seed typed in the "New world" form; empty means pick one at random.
    seed_input: String,
    /// Whether launching a world should also host it on the LAN.
    host_enabled: bool,

    net: Option<NetHandle>,
    server: Option<RunningServer>,
    pending_tofu: Option<PendingTofu>,
    game: Option<GameState>,
    /// Background mDNS browser feeding the menu's LAN server list, if discovery
    /// could be started.
    lan: Option<LanBrowser>,

    input: Input,
    last_frame: Instant,
    /// Seconds elapsed, used to drive sprite animation.
    anim_time: f32,
}

impl App {
    fn new() -> Self {
        let registry = Arc::new(BlockRegistry::new());
        let atlas = Atlas::build(&registry, &blocks_texture_dir(), &entities_texture_dir());
        App {
            window: None,
            gfx: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            registry,
            atlas,
            screen: Screen::Menu,
            status: String::new(),
            address_input: "127.0.0.1:5000".to_string(),
            port_input: "5000".to_string(),
            world_name_input: "world".to_string(),
            seed_input: String::new(),
            host_enabled: false,
            net: None,
            server: None,
            pending_tofu: None,
            game: None,
            lan: match crate::discovery::browse() {
                Ok(b) => Some(b),
                Err(e) => {
                    log::warn!("LAN discovery unavailable: {e:#}");
                    None
                }
            },
            input: Input::default(),
            last_frame: Instant::now(),
            anim_time: 0.0,
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
        let save_dir = crate::save::world_dir(&name);
        match server::start_server(server::local_bind(), seed, save_dir) {
            Ok(srv) => {
                let handle = connect(srv.addr, name.clone(), Some(srv.fingerprint));
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
        let save_dir = crate::save::world_dir(&name);
        match server::start_server(server::host_bind(port), seed, save_dir) {
            Ok(mut srv) => {
                srv.advertise(&format!("Survival Cubed: {name} :{port}"));
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                let handle = connect(addr, name.clone(), Some(srv.fingerprint));
                self.server = Some(srv);
                self.net = Some(handle);
                self.screen = Screen::Connecting;
                self.status = format!("Hosting '{name}' on port {port}...");
            }
            Err(e) => self.status = format!("Failed to host: {e:#}"),
        }
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
        let handle = connect(addr, label.clone(), None);
        self.net = Some(handle);
        self.screen = Screen::Connecting;
        self.status = format!("Connecting to {label}...");
    }

    /// Join a server discovered on the LAN. Its advertised fingerprint is passed
    /// as a pre-trusted cert, so a LAN join needs no TOFU prompt.
    fn start_join_lan(&mut self, server: DiscoveredServer) {
        let handle = connect(server.addr, server.addr.to_string(), server.fingerprint);
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
            } => {
                self.game = Some(GameState::new(entity_id, Vec2::new(spawn_x, spawn_y)));
                self.screen = Screen::InGame;
                self.status.clear();
            }
            NetEvent::Chunk { cx, cy, blocks } => {
                if let Some(g) = &mut self.game {
                    g.world
                        .insert_chunk((cx, cy), crate::world::Chunk::from_vec(blocks));
                }
            }
            NetEvent::BlockUpdate { x, y, block } => {
                if let Some(g) = &mut self.game {
                    g.world.set_block(x, y, block);
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
            NetEvent::EntityDespawn { id } => {
                if let Some(g) = &mut self.game {
                    g.entities.remove(id);
                    g.facing.remove(&id);
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
            NetEvent::Respawn { x, y } => {
                if let Some(g) = &mut self.game {
                    g.pos = Vec2::new(x, y);
                    g.vel = Vec2::ZERO;
                    g.knockback_x = 0.0;
                    g.air_min_y = y;
                    g.last_sent = g.pos;
                }
            }
            NetEvent::Inventory { slots } => {
                if let Some(g) = &mut self.game {
                    g.inventory = Inventory::from_slots(slots);
                }
            }
            NetEvent::Disconnected { reason } => {
                self.status = format!("Disconnected: {reason}");
                self.net = None;
                self.server = None;
                self.game = None;
                self.pending_tofu = None;
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
        }

        let fall_damage = step_physics(game, reg, input, dt);
        if let (Some(amount), Some(net)) = (fall_damage, &self.net) {
            let _ = net.commands.send(NetCommand::FallDamage { amount });
        }
        request_chunks(game, self.gfx.as_ref(), self.net.as_ref());
        handle_block_actions(game, reg, input, self.gfx.as_ref(), self.net.as_ref(), dt);

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
        // TOFU prompt takes priority and is shown as a modal-ish window.
        if self.pending_tofu.is_some() {
            self.tofu_window(ui);
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
                if self.game.as_ref().is_some_and(|g| g.inventory_open) {
                    self.inventory_window(ui);
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
                ui.label("Move: A/D · Jump: Space · Mine: LMB · Place: RMB");
                ui.separator();
                ui.label("[1–9] Select · [E] Inventory");
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
                        let resp =
                            slot_widget(ui, &registry, *slot, Some(i), false, i == selected_slot);
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

    /// The full inventory management screen: storage grid plus the hotbar row,
    /// with click-to-move slot management. Shown over the HUD when toggled.
    fn inventory_window(&mut self, ui: &mut egui::Ui) {
        let (slots, move_from, selected_slot) = {
            let g = self.game.as_ref().unwrap();
            (g.inventory.to_slots(), g.move_from, g.selected_slot)
        };
        let registry = self.registry.clone();
        let mut clicked: Option<usize> = None;
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
                                slots.get(idx).copied().flatten(),
                                None,
                                move_from == Some(idx),
                                false,
                            );
                            if resp.clicked() {
                                clicked = Some(idx);
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
                            slots.get(idx).copied().flatten(),
                            Some(idx),
                            move_from == Some(idx),
                            idx == selected_slot,
                        );
                        if resp.clicked() {
                            clicked = Some(idx);
                        }
                    }
                });
                ui.add_space(6.0);
                ui.weak("Click a slot, then another, to move/stack · click it again to cancel");
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
        if close && let Some(g) = self.game.as_mut() {
            g.inventory_open = false;
            g.move_from = None;
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
        let sky = self
            .game
            .as_ref()
            .map(|g| daylight::sky_color(g.time_of_day))
            .unwrap_or([0.45, 0.62, 0.86, 1.0]);

        if let Some(gfx) = self.gfx.as_mut() {
            gfx.render(
                &tiles,
                camera,
                sky,
                EguiFrame {
                    jobs,
                    textures_delta: full.textures_delta,
                    pixels_per_point: full.pixels_per_point,
                },
            );
        }
    }

    fn build_scene(&self) -> (Vec<TileInstance>, CameraUniform) {
        let gfx = self.gfx.as_ref().unwrap();
        let (vw, vh) = (gfx.size.width.max(1) as f32, gfx.size.height.max(1) as f32);

        let Some(g) = &self.game else {
            return (Vec::new(), CameraUniform::new([0.0, 0.0], [vw, vh], ZOOM));
        };

        let view_w = vw / ZOOM;
        let view_h = vh / ZOOM;
        let center = g.pos + Vec2::new(PLAYER_W * 0.5, PLAYER_H * 0.5);
        let offset = center - Vec2::new(view_w * 0.5, view_h * 0.5);

        let mut tiles = Vec::new();

        // Daylight tint: everything in the world dims toward night.
        let b = daylight::brightness(g.time_of_day);
        let tint = [b, b, b, 1.0];

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
            let secs = self.registry.get(block).break_secs;
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

        // Other entities — remote players and server creatures (drawn over tiles).
        for e in g.entities.values() {
            let (w, h) = e.size();
            // Dropped items render as a small version of their block sprite, not
            // an animation sheet.
            if let EntityKind::DroppedItem { block } = e.kind {
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
            let def = sprite::sprite_for(&e.kind);
            let frame = sprite::frame_index(e.vx.abs() > 1.0, self.anim_time, def);
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
            // A small health bar floats over any wounded creature.
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
        }
        // Self (the special, locally-simulated player entity).
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

        (
            tiles,
            CameraUniform::new([offset.x, offset.y], [vw, vh], ZOOM),
        )
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

/// A representative flat color for a block, used to draw it as an inventory
/// icon (the wgpu atlas isn't an egui texture, so slots use solid swatches).
fn block_color(registry: &BlockRegistry, block: BlockId) -> egui::Color32 {
    match registry.get(block).name {
        "stone" => egui::Color32::from_rgb(120, 120, 128),
        "dirt" => egui::Color32::from_rgb(121, 85, 58),
        "grass" => egui::Color32::from_rgb(83, 150, 60),
        _ => egui::Color32::from_gray(150),
    }
}

/// Draw one inventory/hotbar slot: a framed cell with the block swatch, its
/// stack count, and an optional key number. `highlight` marks a pending move
/// source; `selected` marks the active hotbar slot. Returns the click response.
fn slot_widget(
    ui: &mut egui::Ui,
    registry: &BlockRegistry,
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

    if let Some((block, count)) = slot {
        painter.rect_filled(
            rect.shrink(7.0),
            egui::CornerRadius::same(2),
            block_color(registry, block),
        );
        if count > 1 {
            painter.text(
                rect.right_bottom() - egui::vec2(3.0, 2.0),
                egui::Align2::RIGHT_BOTTOM,
                count.to_string(),
                egui::FontId::proportional(12.0),
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
    resp
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

fn column_solid(game: &GameState, reg: &BlockRegistry, tx: i32, y0: i32, y1: i32) -> bool {
    (y0..=y1).any(|ty| reg.is_solid(game.world.get_block(tx, ty)))
}

fn row_solid(game: &GameState, reg: &BlockRegistry, ty: i32, x0: i32, x1: i32) -> bool {
    (x0..=x1).any(|tx| reg.is_solid(game.world.get_block(tx, ty)))
}

fn request_chunks(game: &mut GameState, gfx: Option<&Gfx>, net: Option<&NetHandle>) {
    let (Some(gfx), Some(net)) = (gfx, net) else {
        return;
    };
    let view_w = gfx.size.width.max(1) as f32 / ZOOM;
    let view_h = gfx.size.height.max(1) as f32 / ZOOM;
    let center = game.pos + Vec2::new(PLAYER_W * 0.5, PLAYER_H * 0.5);
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
                let _ = net.commands.send(NetCommand::RequestChunk { cx, cy });
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

    // The inventory screen captures the mouse for slot management; don't mine or
    // place in the world while it's open.
    if game.inventory_open {
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

    // Left button: swing at a creature under the cursor, else mine the block.
    if input.breaking {
        if let Some(target) = creature_at(game, world) {
            game.break_target = None;
            game.break_progress = 0.0;
            if game.action_timer <= 0.0 {
                let _ = net.commands.send(NetCommand::Attack { target });
                game.action_timer = ACTION_COOLDOWN;
            }
        } else {
            mine_block(game, reg, net, tx, ty, dt);
        }
    } else {
        game.break_target = None;
        game.break_progress = 0.0;
    }

    // Right button: place the selected hotbar slot's block on a fixed cooldown,
    // but only if that slot holds something to spend.
    if input.placing && game.action_timer <= 0.0 && (0..WORLD_HEIGHT).contains(&ty) {
        let slot = game.selected_slot;
        let current = game.world.get_block(tx, ty);
        if let Some((block, _)) = game.inventory.get(slot)
            && current == AIR
            && !overlaps_player(game, tx, ty)
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
    if current == AIR {
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

    if game.break_progress >= reg.get(current).break_secs {
        game.world.set_block(tx, ty, AIR);
        let _ = net.commands.send(NetCommand::SetBlock {
            x: tx,
            y: ty,
            block: AIR,
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
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let PhysicalKey::Code(code) = event.physical_key {
                    if wants_kb {
                        // Typing into a text field; don't drive the player.
                        self.input.left = false;
                        self.input.right = false;
                        self.input.jump = false;
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
            // Escape closes the inventory if open, otherwise leaves the world.
            KeyCode::Escape if pressed => {
                if self.game.as_ref().is_some_and(|g| g.inventory_open) {
                    if let Some(g) = &mut self.game {
                        g.inventory_open = false;
                        g.move_from = None;
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
