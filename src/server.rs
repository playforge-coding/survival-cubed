//! Authoritative game server over QUIC (quinn).
//!
//! The server uses a self-signed certificate persisted in its world directory,
//! so its fingerprint stays stable across restarts and clients' pinned TOFU
//! entries keep working. A brand-new world generates and saves a fresh pair on
//! first launch. The fingerprint is surfaced to the client so the TOFU flow
//! (see [`crate::client::net`]) can pin it. For singleplayer the client embeds
//! a server in-process and auto-trusts that fingerprint.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use parking_lot::Mutex;
use quinn::Endpoint;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use tokio::sync::mpsc;

use crate::auth;
use crate::block::BlockRegistry;
use crate::daylight;
use crate::entity::{
    AXE_SIZE, BONE_SIZE, Entities, Entity, EntityId, EntityKind, FIREBALL_SIZE, ITEM_SIZE,
    MAGIC_FIREBALL_SIZE, PLAYER_SIZE, SUMMONER_FIREBALL_SIZE,
};
use crate::inventory::{CHEST_SLOTS, Inventory, Slot, move_between, move_within};
use crate::net::{VERSION_MISMATCH_CLOSE, fingerprint, read_msg, read_version, write_msg};
use crate::protocol::{
    ALPN, BlockId, ClientMessage, PASSWORD_MAX_LEN, PROTOCOL_VERSION, ServerMessage, SlotRef,
    Waypoint,
};
use crate::save::{SavedPlayer, WorldMeta, WorldStore};
use crate::world::{
    CHUNK_AREA, CHUNK_SIZE, ChunkCoord, Dimension, NUM_DIMENSIONS, TILE_SIZE, WORLD_HEIGHT, World,
};
use crate::worldgen::{
    Biome, WorldGen, arena_player_spawn, arena_room_ceiling_y, arena_room_center_x,
    arena_room_left_x, spawn_point,
};

/// How often the server simulates non-player entities, in seconds.
const TICK_DT: f32 = 0.05;
/// Horizontal wander speed of a slime, in pixels/second.
const SLIME_SPEED: f32 = 30.0;
/// Leisurely wander speed of a chicken, in pixels/second.
const CHICKEN_WANDER_SPEED: f32 = 22.0;
/// Panicked run speed of a chicken fleeing a player, in pixels/second.
const CHICKEN_FLEE_SPEED: f32 = 70.0;
/// Seconds a chicken keeps bolting after being hit before settling down.
const CHICKEN_FLEE_TIME: f32 = 4.0;
/// Unhurried wander speed of a goat, in pixels/second.
const GOAT_SPEED: f32 = 18.0;
/// Padding wander speed of a cat, in pixels/second.
const CAT_SPEED: f32 = 24.0;
/// Percent chance that a fresh forest surface chunk seeds a cat. Deliberately
/// tiny — cats are a rare forest find, not a common critter.
const CAT_CHUNK_CHANCE: u32 = 6;
/// Distance (px) past which a tamed cat teleports to its owner instead of
/// padding along on the ground. Set beyond a screen's reach so a cat keeping
/// pace stays put, and only a cat the player has truly left behind warps to them.
const CAT_TELEPORT_DIST: f32 = 320.0;
/// Trotting speed of a puppy, in pixels/second — quicker than a cat so it can
/// run down the skeletons and chickens it hunts.
const PUPPY_SPEED: f32 = 34.0;
/// Percent chance that a fresh forest surface chunk seeds a puppy. As rare a
/// forest find as the cat.
const PUPPY_CHUNK_CHANCE: u32 = 6;
/// Distance (px) past which a tamed puppy teleports to its owner instead of
/// trotting along on the ground — same leash as the cat's.
const PUPPY_TELEPORT_DIST: f32 = 320.0;
/// Unhurried wander speed of a horse, in pixels/second. (The gallop the rider
/// gets while mounted is a faster, client-side movement mode; this is just how a
/// loose horse ambles.)
const HORSE_SPEED: f32 = 26.0;
/// Percent chance that a fresh plains surface chunk seeds a wild horse.
const HORSE_CHUNK_CHANCE: u32 = 6;
/// Distance (px) past which a tamed horse teleports to its owner instead of
/// ambling along on the ground — same leash as the cat's and puppy's.
const HORSE_TELEPORT_DIST: f32 = 320.0;
/// How far (px) a puppy notices a skeleton/chicken to hunt, or raw meat to eat.
const PUPPY_AGGRO: f32 = 220.0;
/// Maximum gap (px between AABBs) at which a puppy lands a bite on its prey.
const PUPPY_ATTACK_RANGE: f32 = 4.0;
/// Maximum gap (px between AABBs) at which a puppy reaches a meat drop to eat it.
const PUPPY_EAT_REACH: f32 = 3.0;
/// Damage a puppy deals per bite.
const PUPPY_DAMAGE: i32 = 6;
/// Seconds a puppy waits between bites.
const PUPPY_ATTACK_INTERVAL: f32 = 0.8;
/// Hit points a puppy recovers each time it eats a dropped piece of raw meat.
const PUPPY_EAT_HEAL: i32 = 3;

/// Ambling speed of a farmer going about its rounds, in pixels/second — a touch
/// brisker than a goat as it crosses the plains between its quarry.
const FARMER_SPEED: f32 = 30.0;
/// Sprinting speed of a farmer bolting from a monster, in pixels/second — fast
/// enough to outrun most of what hunts it, like a panicked chicken.
const FARMER_FLEE_SPEED: f32 = 64.0;
/// How far (px) a farmer notices a chicken or goat to hunt, or raw meat to collect.
const FARMER_AGGRO: f32 = 200.0;
/// How close (px) a monster must come before a farmer drops everything and flees it.
/// Wider than its hunting range, so it spots danger before it spots dinner.
const FARMER_FLEE_RANGE: f32 = 220.0;
/// Maximum gap (px between AABBs) at which a farmer lands a blow on the animal it hunts.
const FARMER_ATTACK_RANGE: f32 = 4.0;
/// Maximum gap (px between AABBs) at which a farmer reaches a meat drop to collect it.
const FARMER_EAT_REACH: f32 = 3.0;
/// Damage a farmer deals per strike — enough to fell a chicken in two blows and a
/// goat in three.
const FARMER_DAMAGE: i32 = 6;
/// Seconds a farmer waits between strikes.
const FARMER_ATTACK_INTERVAL: f32 = 0.8;
/// Hit points a farmer recovers each time it collects (and eats) a piece of raw meat.
const FARMER_EAT_HEAL: i32 = 3;
/// Percent chance that a fresh plains surface chunk seeds a wandering farmer — as
/// rare a sight as the knights and musketeers that share the plains.
const FARMER_CHUNK_CHANCE: u32 = 4;
/// Shambling speed of a zombie, in pixels/second — noticeably slower than a
/// slime, so a player can outrun one but is in trouble if cornered.
const ZOMBIE_SPEED: f32 = 16.0;
/// How far (px) a zombie notices and chases a player. Reaches a little further
/// than a slime since zombies are the night's dedicated hunters.
const ZOMBIE_AGGRO: f32 = 200.0;
/// Maximum gap (px between AABBs) at which a zombie can land a hit.
const ZOMBIE_ATTACK_RANGE: f32 = 4.0;
/// Damage a zombie deals per hit — a heavy blow compared with a slime's nip.
const ZOMBIE_DAMAGE: i32 = 7;
/// Seconds a zombie waits between hits.
const ZOMBIE_ATTACK_INTERVAL: f32 = 1.2;
/// How often the server tries to spawn night zombies near players, in seconds.
const ZOMBIE_SPAWN_INTERVAL: f32 = 6.0;
/// Most live zombies allowed per connected player; spawning pauses at the cap.
const ZOMBIE_MAX_PER_PLAYER: usize = 5;
/// Nearest a freshly spawned zombie appears to its target player, in pixels —
/// far enough to be just off-screen so they don't pop in at point-blank range.
const ZOMBIE_SPAWN_MIN_DIST: f32 = 220.0;
/// Farthest a freshly spawned zombie appears from its target player, in pixels.
const ZOMBIE_SPAWN_MAX_DIST: f32 = 360.0;
/// Quick scuttling speed of a spider, in pixels/second — faster than anything
/// else that walks, so it closes distance and a player can't simply outrun it.
const SPIDER_SPEED: f32 = 46.0;
/// Speed (px/s) at which a chasing spider scales a wall it has run into. Matched
/// to its ground speed so climbing never stalls the pursuit.
const SPIDER_CLIMB_SPEED: f32 = 46.0;
/// How far (px) a spider notices a player and gives chase.
const SPIDER_AGGRO: f32 = 180.0;
/// Maximum gap (px between AABBs) at which a spider can land a bite.
const SPIDER_ATTACK_RANGE: f32 = 4.0;
/// Damage a spider deals per bite.
const SPIDER_DAMAGE: i32 = 4;
/// Seconds a spider waits between bites.
const SPIDER_ATTACK_INTERVAL: f32 = 0.9;
/// Percent chance that a fresh, eligible chunk (forest surface, or anywhere deep
/// underground) seeds spiders.
const SPIDER_CHUNK_CHANCE: u32 = 35;
/// Most spiders a single eligible chunk seeds at once.
const SPIDER_CHUNK_MAX: u32 = 2;
/// World row at or below which a chunk counts as the deep dark spiders haunt, so
/// they only nest in the underground caverns and never in surface tunnels that
/// open to daylight. Sits well below the surface baseline (`WORLD_HEIGHT / 2`).
const SPIDER_CAVERN_MIN_Y: i32 = WORLD_HEIGHT * 11 / 16;
/// Slithering speed of a snake as it closes on a player, in pixels/second —
/// brisker than a zombie but no sprinter; the danger is its lunge, not its pace.
const SNAKE_SPEED: f32 = 30.0;
/// How far (px) a snake notices a player and begins hunting.
const SNAKE_AGGRO: f32 = 170.0;
/// Gap (px between AABBs) at which a snake stops and commits to a wind-up lunge.
/// Larger than a melee range so the snake strikes from a short distance, springing
/// the rest of the way.
const SNAKE_LUNGE_RANGE: f32 = 22.0;
/// Gap (px between AABBs), measured at the moment the strike lands, within which a
/// snake's lunge connects. Generous enough to cover the forward spring, so a
/// player who holds still is bitten but one who breaks away during the telegraphed
/// wind-up escapes.
const SNAKE_LUNGE_REACH: f32 = 26.0;
/// Forward speed (px/s) of the snake's body during the strike half of its lunge —
/// a quick spring toward the locked-in heading.
const SNAKE_LUNGE_SPEED: f32 = 90.0;
/// Seconds the snake spends coiling (telegraphing) before the strike lands, out of
/// the total [`crate::entity::SNAKE_LUNGE_TIME`]. The remainder is the spring.
const SNAKE_WINDUP_TIME: f32 = 0.4;
/// Seconds of strike remaining (of [`crate::entity::SNAKE_LUNGE_TIME`]) once the
/// wind-up ends: the bite is resolved as this threshold is crossed and the snake
/// springs forward for the rest.
const SNAKE_STRIKE_TIME: f32 = crate::entity::SNAKE_LUNGE_TIME - SNAKE_WINDUP_TIME;
/// Damage a snake's lunge bite deals.
const SNAKE_DAMAGE: i32 = 6;
/// Seconds a snake waits between lunges (the recovery after one finishes).
const SNAKE_ATTACK_INTERVAL: f32 = 1.6;
/// Percent chance that a fresh desert surface chunk seeds snakes.
const SNAKE_CHUNK_CHANCE: u32 = 30;
/// Most snakes a single eligible desert chunk seeds at once.
const SNAKE_CHUNK_MAX: u32 = 2;
/// Stalking speed of a skeleton, in pixels/second — a touch quicker than a
/// zombie, since it wants to reposition for a clean shot rather than just maul.
const SKELETON_SPEED: f32 = 22.0;
/// How far (px) a skeleton notices a player and begins stalking/firing.
const SKELETON_AGGRO: f32 = 240.0;
/// Maximum gap (px between AABBs) at which a skeleton will loose a bone — it
/// stops advancing and throws once a player is this close.
const SKELETON_THROW_RANGE: f32 = 190.0;
/// Standoff gap (px between AABBs) a skeleton tries to keep: it backs away from a
/// player closer than this so it can keep peppering them from range.
const SKELETON_KEEP_DIST: f32 = 90.0;
/// Seconds a skeleton waits between throws.
const SKELETON_THROW_INTERVAL: f32 = 1.6;
/// Of the night undead the server spawns near players, the percent that arrive as
/// skeletons rather than zombies.
const SKELETON_SPAWN_PERCENT: u32 = 30;
/// Flight speed of a thrown bone, in pixels/second.
const BONE_SPEED: f32 = 170.0;
/// Damage a bone deals on striking a player.
const BONE_DAMAGE: i32 = 5;
/// Seconds a thrown bone stays airborne before it gives out and despawns, in case
/// it never hits anything.
const BONE_LIFETIME: f32 = 3.0;
/// Maximum gap (px between AABBs) at which an in-flight bone counts as striking a
/// player.
const BONE_HIT_RANGE: f32 = 2.0;
/// Stalking speed of a dark knight, in pixels/second — a trained warrior, quicker on
/// its feet than the shambling skeleton it otherwise fights like.
const DARK_KNIGHT_SPEED: f32 = 24.0;
/// How far (px) a dark knight notices a player or knight and begins stalking/throwing.
const DARK_KNIGHT_AGGRO: f32 = 260.0;
/// Maximum gap (px between AABBs) at which a dark knight will loose an axe — it stops
/// advancing and throws once a target is this close.
const DARK_KNIGHT_THROW_RANGE: f32 = 200.0;
/// Standoff gap (px between AABBs) a dark knight tries to keep: it backs away from a
/// target closer than this so it can keep peppering them from range.
const DARK_KNIGHT_KEEP_DIST: f32 = 95.0;
/// Seconds a dark knight waits between axe throws.
const DARK_KNIGHT_THROW_INTERVAL: f32 = 1.8;
/// Of the night mobs the server spawns near players, the percent that arrive as dark
/// knights (in any biome). Deliberately small — they are a rare, dangerous prize.
const DARK_KNIGHT_NIGHT_PERCENT: u32 = 6;
/// Percent chance a slain dark knight's tungsten haul also includes a finished
/// tungsten tool or weapon, on top of its ingots.
const DARK_KNIGHT_TOOL_DROP_PERCENT: u32 = 55;
/// Percent chance a slain dark knight also spills a full suit of tungsten armor —
/// deliberately small, so a suit looted from a knight is a rare prize.
const DARK_KNIGHT_ARMOR_DROP_PERCENT: u32 = 7;
/// Percent chance a slain [`EntityKind::Necromancer`] drops its
/// [`crate::block::SUMMONER_SPELL`] spellbook. Deliberately small — the spellbook is
/// a rare prize and (for now) the only source of the world's first spell.
const NECROMANCER_SPELLBOOK_DROP_PERCENT: u32 = 8;
/// Durability a worn suit of armor loses each time it soaks an enemy blow. One
/// point per hit, so a suit lasts for as many hits as its
/// [`max_durability`](crate::block::max_durability) before it must be mended.
const ARMOR_WEAR_PER_HIT: u16 = 1;
/// Flight speed of a thrown axe, in pixels/second.
const AXE_SPEED: f32 = 165.0;
/// Damage an axe deals on striking a player or knight — heavier than a thrown bone,
/// fitting a deadlier foe.
const AXE_DAMAGE: i32 = 8;
/// Seconds a thrown axe stays airborne before it gives out and despawns, in case it
/// never hits anything.
const AXE_LIFETIME: f32 = 3.0;
/// Maximum gap (px between AABBs) at which an in-flight axe counts as striking a
/// player or knight.
const AXE_HIT_RANGE: f32 = 2.0;
/// Stalking speed of a musketeer, in pixels/second — a touch quicker than the dark
/// musketeer it mirrors, as it repositions for a clean shot.
const MUSKETEER_SPEED: f32 = 30.0;
/// How far (px) a musketeer notices a monster and begins stalking/firing.
const MUSKETEER_AGGRO: f32 = 270.0;
/// Maximum gap (px between AABBs) at which a musketeer will fire — it stops advancing
/// and shoots once a monster is this close.
const MUSKETEER_SHOOT_RANGE: f32 = 210.0;
/// Standoff gap (px between AABBs) a musketeer tries to keep, backing away from a
/// monster closer than this so it can keep peppering it from range.
const MUSKETEER_KEEP_DIST: f32 = 90.0;
/// Seconds a musketeer waits between shots — a long reload, befitting a slow,
/// hard-hitting firearm (its bullet lands a heavy [`FRIENDLY_BULLET_DAMAGE`] blow).
const MUSKETEER_SHOOT_INTERVAL: f32 = 2.6;
/// Stalking speed of a dark musketeer, in pixels/second — the same trained foot as the
/// dark knight it marches with.
const DARK_MUSKETEER_SPEED: f32 = 24.0;
/// How far (px) a dark musketeer notices a player, knight or musketeer and begins
/// stalking/firing.
const DARK_MUSKETEER_AGGRO: f32 = 270.0;
/// Maximum gap (px between AABBs) at which a dark musketeer will fire.
const DARK_MUSKETEER_SHOOT_RANGE: f32 = 210.0;
/// Standoff gap (px between AABBs) a dark musketeer tries to keep so it can keep firing
/// from range.
const DARK_MUSKETEER_KEEP_DIST: f32 = 95.0;
/// Seconds a dark musketeer waits between shots — the same slow reload as the friendly
/// [`MUSKETEER_SHOOT_INTERVAL`] musketeer it mirrors.
const DARK_MUSKETEER_SHOOT_INTERVAL: f32 = 2.6;
/// Flight speed of a fired bullet, in pixels/second — far faster than any thrown
/// projectile, befitting a firearm.
const BULLET_SPEED: f32 = 340.0;
/// Damage a hostile bullet (a dark musketeer's shot) deals on striking a player,
/// knight or musketeer — the same heavy blow as the friendly [`FRIENDLY_BULLET_DAMAGE`]
/// musket, since a dark musketeer carries the same slow, hard-hitting firearm.
const BULLET_DAMAGE: i32 = 30;
/// Damage a friendly bullet (a musketeer's or a player's musket shot) deals to the
/// monster it strikes — a heavy wallop, fitting a slow-loading firearm: a musket hits
/// far harder than any melee swing, the trade-off being its long reload.
const FRIENDLY_BULLET_DAMAGE: i32 = 30;
/// Seconds a bullet (hostile or friendly) stays airborne before it gives out, in case
/// it never hits anything. Short — a bullet is fast and travels its range quickly.
const BULLET_LIFETIME: f32 = 1.5;
/// The musket's reload time: the minimum gap between a player's shots, enforced
/// server-side so a fast-firing client can't fan a slow firearm. Mirrors the client's
/// own `MUSKET_COOLDOWN` (a hair shorter, so client-paced shots are never rejected for
/// arriving a frame early).
const MUSKET_COOLDOWN: Duration = Duration::from_millis(1350);
/// Maximum gap (px between AABBs) at which an in-flight bullet counts as striking.
const BULLET_HIT_RANGE: f32 = 2.0;
/// Charging speed of a charred skeleton, in pixels/second — quicker than a zombie,
/// since it commits hard to running its prey down.
const CHARRED_SKELETON_SPEED: f32 = 26.0;
/// How far (px) a charred skeleton notices a player and gives chase.
const CHARRED_SKELETON_AGGRO: f32 = 220.0;
/// Maximum gap (px between AABBs) at which a charred skeleton lands a melee blow.
const CHARRED_SKELETON_ATTACK_RANGE: f32 = 4.0;
/// Damage a charred skeleton deals per hit — heavier than a zombie's blow, making
/// it the underworld's most dangerous brawler.
const CHARRED_SKELETON_DAMAGE: i32 = 10;
/// Seconds a charred skeleton waits between blows.
const CHARRED_SKELETON_ATTACK_INTERVAL: f32 = 1.0;
/// Seconds between a chasing charred skeleton laying down each tongue of fire, so
/// it leaves a broken trail of flame rather than a solid wall.
const CHARRED_SKELETON_FIRE_INTERVAL: f32 = 0.35;
/// Percent chance that a fresh underworld chunk seeds charred skeletons.
const CHARRED_SKELETON_CHUNK_CHANCE: u32 = 45;
/// Most charred skeletons a single fresh underworld chunk seeds at once.
const CHARRED_SKELETON_CHUNK_MAX: u32 = 2;
/// Flitting speed of a demon, in pixels/second — a touch quicker than a charred
/// skeleton, since it wants to reposition for a clean shot rather than brawl.
const DEMON_SPEED: f32 = 28.0;
/// How far (px) a demon notices a player and begins stalking/firing.
const DEMON_AGGRO: f32 = 240.0;
/// Maximum gap (px between AABBs) at which a demon will hurl a fireball — it stops
/// advancing and fires once a player is this close.
const DEMON_SHOOT_RANGE: f32 = 200.0;
/// Standoff gap (px between AABBs) a demon tries to keep: it backs away from a
/// player closer than this so it can keep peppering them from range.
const DEMON_KEEP_DIST: f32 = 100.0;
/// Seconds a demon waits between fireballs.
const DEMON_SHOOT_INTERVAL: f32 = 2.0;
/// Flight speed of a hurled fireball, in pixels/second.
const FIREBALL_SPEED: f32 = 150.0;
/// Damage a fireball deals on striking a player — heavier than a thrown bone.
const FIREBALL_DAMAGE: i32 = 7;
/// Seconds a fireball stays airborne before it gives out and despawns, in case it
/// never hits anything.
const FIREBALL_LIFETIME: f32 = 3.0;
/// Maximum gap (px between AABBs) at which an in-flight fireball counts as striking
/// a player.
const FIREBALL_HIT_RANGE: f32 = 2.0;
/// Percent chance that a fresh underworld chunk seeds a demon — lower than
/// [`CHARRED_SKELETON_CHUNK_CHANCE`] so demons are the rarer underworld threat. A
/// seeded chunk gets a single demon (where the charred skeleton can come in pairs),
/// keeping these ranged casters thin on the ground.
const DEMON_CHUNK_CHANCE: u32 = 18;

// --- Gargoyle: a stone hopper of the charred expanse ---
/// How far (px) a gargoyle notices a player and begins hopping after them.
const GARGOYLE_AGGRO: f32 = 240.0;
/// Horizontal speed (px/s) of a gargoyle's ordinary roaming/chasing hop — the arc it
/// springs in as it closes the distance (it never walks).
const GARGOYLE_HOP_VX: f32 = 60.0;
/// Upward launch velocity (px/s, negative is up) of a gargoyle's ordinary hop.
const GARGOYLE_HOP_VY: f32 = -200.0;
/// Seconds a gargoyle sits gathered on the ground between ordinary hops.
const GARGOYLE_HOP_INTERVAL: f32 = 0.55;
/// Maximum gap (px between AABBs) at which a gargoyle commits to its **jump-slam** — it
/// leaps at any player closer than this, trying to land on them. Kept in step with the
/// arc's reach (`GARGOYLE_SLAM_MAX_VX * GARGOYLE_SLAM_AIRTIME`) so a slam loosed at the
/// edge of range can still come down on a stationary target rather than always short.
const GARGOYLE_SLAM_RANGE: f32 = 90.0;
/// Upward launch velocity (px/s, negative is up) of a gargoyle's slam leap — a higher,
/// harder spring than its ordinary hop so it can drop onto the player from above.
const GARGOYLE_SLAM_VY: f32 = -300.0;
/// Nominal airtime (s) of a slam leap, used to aim its horizontal velocity so the arc
/// lands on the player: roughly `2 * |GARGOYLE_SLAM_VY| / GRAVITY`.
const GARGOYLE_SLAM_AIRTIME: f32 = 0.43;
/// Cap (px/s) on a slam leap's horizontal velocity, so a distant target doesn't fling
/// the gargoyle across the cavern at absurd speed.
const GARGOYLE_SLAM_MAX_VX: f32 = 210.0;
/// Maximum gap (px between AABBs) at which a gargoyle's landing slam connects — it has
/// to come down essentially on top of the player.
const GARGOYLE_SLAM_REACH: f32 = 14.0;
/// Damage a gargoyle's landing slam deals to whoever it crashes onto.
const GARGOYLE_SLAM_DAMAGE: i32 = 12;
/// Seconds a gargoyle recovers (on top of the slam's own airtime) before it can leap
/// at the player again.
const GARGOYLE_ATTACK_INTERVAL: f32 = 1.2;
/// Percent chance that a fresh underworld chunk seeds a gargoyle — a touch lower than
/// [`DEMON_CHUNK_CHANCE`] so gargoyles are slightly rarer than demons.
const GARGOYLE_CHUNK_CHANCE: u32 = 14;

/// Lumbering speed of an orc, in pixels/second — slower than even a zombie, the
/// previous slowest thing afoot. Its menace is the slam, never the chase.
const ORC_SPEED: f32 = 12.0;
/// How far (px) an orc notices a player and begins lumbering after them.
const ORC_AGGRO: f32 = 200.0;
/// Gap (px between AABBs) at which an orc stops and commits to a wind-up slam.
const ORC_SLAM_RANGE: f32 = 14.0;
/// Maximum gap (px between AABBs) at which a committed slam connects when the fists
/// hit the ground. A touch generous so the heavy blow still lands if the player
/// only just backed off.
const ORC_SLAM_REACH: f32 = 18.0;
/// Seconds of slam remaining (of [`crate::entity::ORC_SLAM_TIME`]) at which the
/// fists hit the ground and the blow lands. The slam sheet is six frames and the
/// impact is its frame 3, so the strike connects halfway through the animation.
const ORC_SLAM_STRIKE_TIME: f32 = crate::entity::ORC_SLAM_TIME * 0.5;
/// Damage an orc's slam deals — by far the heaviest blow in the world, enough to
/// gut an unarmoured player in two hits.
const ORC_SLAM_DAMAGE: i32 = 16;
/// Seconds an orc waits between slams (the recovery after one finishes).
const ORC_SLAM_INTERVAL: f32 = 1.5;
/// Percent chance that a fresh underworld chunk seeds orcs.
const ORC_CHUNK_CHANCE: u32 = 28;
/// Most orcs a single fresh underworld chunk seeds at once.
const ORC_CHUNK_MAX: u32 = 2;
/// Flying speed of an enchanted demon, in pixels/second — quicker than the ordinary
/// demon it was, since the mage's glamour lets it chase the player through the air.
const ENCHANTED_DEMON_SPEED: f32 = 34.0;
/// How far (px) an enchanted demon notices a player and begins stalking/firing —
/// reaching farther than the ordinary demon.
const ENCHANTED_DEMON_AGGRO: f32 = 280.0;
/// Maximum gap (px between AABBs) at which an enchanted demon will hurl a magic
/// fireball — it covers more range than the ordinary demon's fireball.
const ENCHANTED_DEMON_SHOOT_RANGE: f32 = 260.0;
/// Standoff gap (px between AABBs) an enchanted demon tries to keep, backing away
/// from a player closer than this so it can keep peppering them from range.
const ENCHANTED_DEMON_KEEP_DIST: f32 = 120.0;
/// Seconds an enchanted demon waits between magic fireballs.
const ENCHANTED_DEMON_SHOOT_INTERVAL: f32 = 2.0;
/// Spread (radians) of an enchanted demon's volley: rather than one bolt it looses
/// three at a time — one straight at the target and two fanned this far above and
/// below it — so the player must dodge a wider arc.
const ENCHANTED_DEMON_SPREAD: f32 = 0.32;
/// Flight speed of a hurled magic fireball, in pixels/second — faster than an
/// ordinary fireball.
const MAGIC_FIREBALL_SPEED: f32 = 170.0;
/// Damage a magic fireball deals on striking a player — heavier than an ordinary
/// fireball's [`FIREBALL_DAMAGE`].
const MAGIC_FIREBALL_DAMAGE: i32 = 12;
/// Seconds a magic fireball stays airborne before it gives out and despawns. Longer
/// than an ordinary fireball's life, so it covers more range before fizzling.
const MAGIC_FIREBALL_LIFETIME: f32 = 4.0;
/// Maximum gap (px between AABBs) at which an in-flight magic fireball counts as
/// striking a player — a touch more forgiving than an ordinary fireball's.
const MAGIC_FIREBALL_HIT_RANGE: f32 = 3.0;

// --- Dragon (underworld flying miniboss) ---------------------------------------
/// Flying speed of a dragon, in pixels/second — swifter than the enchanted demon it
/// shares the air with, befitting a miniboss that runs a fleeing player down.
const DRAGON_SPEED: f32 = 42.0;
/// How far (px) a dragon notices a player and begins wheeling in to attack.
const DRAGON_AGGRO: f32 = 360.0;
/// Maximum gap (px between AABBs) at which a dragon will breathe a fireball — it
/// stops closing and looses once a player is this near.
const DRAGON_SHOOT_RANGE: f32 = 280.0;
/// Standoff gap (px between AABBs) a dragon tries to keep: it backs off from a
/// player closer than this so it can keep raining fire from range.
const DRAGON_KEEP_DIST: f32 = 150.0;
/// Seconds a dragon waits between fireballs — a brisker cadence than the demons,
/// since a lone miniboss must threaten on its own.
const DRAGON_SHOOT_INTERVAL: f32 = 1.4;
/// Permille (out of 1000) chance that a fresh underworld charred chunk seeds a
/// dragon — far rarer than any other underworld spawner, so a dragon is a genuine
/// once-in-a-while event rather than ambient life.
const DRAGON_CHUNK_PERMILLE: u32 = 12;
/// Open (non-solid) cells a dragon's spawn column must clear above its floor before
/// it will seed one there. This keeps a dragon out of cramped tunnels, dropping it
/// only into a tall cavern where it flies in plain sight (so its music never plays
/// over an unseen foe).
const DRAGON_SPAWN_CLEARANCE: i32 = 10;
/// How high (px) above the cavern floor a freshly seeded dragon hovers, so it
/// begins aloft in the open rather than perched on the rock.
const DRAGON_SPAWN_HOVER: f32 = 6.0 * TILE_SIZE;

// --- Minotaur (underworld horned miniboss) -------------------------------------
/// How far (px) a minotaur notices a player and begins hunting. Generous like the
/// dragon's, befitting a miniboss that commands its patch of the depths.
const MINOTAUR_AGGRO: f32 = 360.0;
/// Ground speed of a minotaur as it **hulks** after a player, in pixels/second —
/// slower than even an orc, a ponderous brute until it commits to a charge.
const MINOTAUR_HULK_SPEED: f32 = 22.0;
/// Ground speed of a charging minotaur, in pixels/second — it lowers its head and
/// barrels in far faster than a player can walk, so a headbutt charge must be dodged
/// rather than outrun in a straight line.
const MINOTAUR_CHARGE_SPEED: f32 = 168.0;
/// Gap (px between AABBs) within which a minotaur, off cooldown, leaps into a **jump
/// slam** rather than charging — the player is close enough to crush from above.
const MINOTAUR_SLAM_RANGE: f32 = 48.0;
/// Maximum gap (px between AABBs) at which a minotaur, off cooldown and with a clear
/// line on the player, lowers its head and commits to a **headbutt charge** — beyond
/// slam range but within this it picks the charge.
const MINOTAUR_CHARGE_RANGE: f32 = 240.0;
/// How far above/below the minotaur's own center (px) a target may be for a headbutt
/// charge to make sense — a charge runs along the ground, so it only commits when the
/// player is roughly level with it (otherwise it hulks closer or slams).
const MINOTAUR_CHARGE_LEVEL: f32 = 36.0;
/// Upward launch speed (px/s) of a minotaur's jump slam — a leap that carries it a few
/// blocks clear of the ground so it hangs before crashing down. Paired with the reduced
/// [`MINOTAUR_HANG_GRAVITY`], it rises to its apex roughly halfway through the rise/hang
/// phase, then floats there before the drop.
const MINOTAUR_JUMP_VY: f32 = -240.0;
/// Gravity scale applied to a minotaur while it rises and **hangs** at the apex of its
/// jump slam (a fraction of normal [`GRAVITY`]), so it floats menacingly before the
/// drop rather than snapping straight back down.
const MINOTAUR_HANG_GRAVITY: f32 = 0.32;
/// Downward speed (px/s) a minotaur slams at once its jump enters the drop phase — it
/// crashes back to earth far faster than it rose.
const MINOTAUR_SLAM_VY: f32 = 620.0;
/// Seconds of jump slam remaining (of [`crate::entity::MINOTAUR_SLAM_TIME`]) at which
/// the leap gives way to the crashing **drop**: until then it rises and hangs, after
/// which it plummets. A little past halfway, so the hang reads before the slam.
const MINOTAUR_SLAM_DROP_TIME: f32 = crate::entity::MINOTAUR_SLAM_TIME * 0.45;
/// Horizontal speed (px/s) a minotaur carries toward its target as it leaps, so a jump
/// slam comes down roughly *on* the player rather than straight up and down.
const MINOTAUR_LEAP_VX: f32 = 70.0;
/// Radius (px, from the minotaur's center) within which a jump slam's shockwave catches
/// a **grounded** target when it lands. Wide — the whole point of the slam is that it's
/// an area attack you escape by being airborne, not by being a step away.
const MINOTAUR_SLAM_RADIUS: f32 = 56.0;
/// Damage a minotaur's jump slam deals to each grounded target caught in its landing.
const MINOTAUR_SLAM_DAMAGE: i32 = 18;
/// Damage a minotaur's headbutt charge deals to the target it gores.
const MINOTAUR_HEADBUTT_DAMAGE: i32 = 16;
/// Maximum gap (px between AABBs) at which a charging minotaur's headbutt connects.
const MINOTAUR_HEADBUTT_REACH: f32 = 12.0;
/// Seconds a headbutt charge runs before the minotaur gives up the rush (if it hasn't
/// already gored its target or slammed into a wall) and returns to hulking.
const MINOTAUR_CHARGE_TIME: f32 = 1.3;
/// Seconds a minotaur waits between attacks (the recovery set when one finishes).
const MINOTAUR_ATTACK_INTERVAL: f32 = 1.6;
/// Permille (out of 1000) chance that a fresh underworld charred chunk seeds a
/// minotaur — about as rare as a [`DRAGON_CHUNK_PERMILLE`] dragon, so meeting one is a
/// genuine once-in-a-while event rather than ambient life.
const MINOTAUR_CHUNK_PERMILLE: u32 = 12;
/// Open (non-solid) cells a minotaur's spawn column must clear above its floor before
/// it will seed one there — enough headroom for its great height and its jump slam, so
/// it isn't wedged into a cramped tunnel.
const MINOTAUR_SPAWN_CLEARANCE: i32 = 6;

// --- White dragon (the dragonian steed companion) ------------------------------
/// How far (px) a riderless white dragon notices a monster and wheels in to breathe
/// fire at it. Generous like the hostile dragon's aggro, so the steed actively keeps
/// the area around its owner clear.
const WHITE_DRAGON_AGGRO: f32 = 360.0;
/// Maximum gap (px between AABBs) at which a riderless white dragon breathes a friendly
/// fireball at the monster it is hunting.
const WHITE_DRAGON_SHOOT_RANGE: f32 = 280.0;
/// Seconds a riderless white dragon waits between friendly fireballs.
const WHITE_DRAGON_SHOOT_INTERVAL: f32 = 1.2;
/// Gap (px between AABBs) within which a riderless white dragon, with no monster to
/// hunt, settles into a gentle hover beside its owner rather than pressing closer.
const WHITE_DRAGON_FOLLOW_GAP: f32 = 40.0;
/// How far (px) an owner may stray before their white dragon teleports to their side
/// (it never despawns for distance, like a pet — it snaps to its owner instead).
const WHITE_DRAGON_TELEPORT_DIST: f32 = 640.0;
/// Seconds a ridden white dragon waits between fireballs its rider breathes on the
/// breath key — a brisk cadence, but capped so the steed can't loose a wall of fire.
const WHITE_DRAGON_BREATH_INTERVAL: f32 = 0.45;
/// Flight speed (px/s, each axis) of a white dragon being remotely piloted by its
/// summoner — snappier than the AI's lazy [`DRAGON_SPEED`] so it handles like a body.
const WHITE_DRAGON_CONTROL_SPEED: f32 = 150.0;
/// How far (px) a remotely-piloted white dragon may stray from its summoner before it
/// is held back: the reach of the telepathic bond the steed grants. Eight chunks in any
/// direction (see [`CHUNK_SIZE`]/[`TILE_SIZE`]) — a wide leash, but not unlimited.
pub const WHITE_DRAGON_CONTROL_RANGE: f32 = 8.0 * CHUNK_SIZE as f32 * TILE_SIZE;
/// Damage a friendly dragon fireball deals to a monster it strikes — the same wallop
/// the hostile dragon's fireball lands on a player.
const FRIENDLY_DRAGON_FIREBALL_DAMAGE: i32 = 10;
/// Gap (px between AABBs) within which a friendly dragon fireball counts as striking
/// the monster it flies into.
const FRIENDLY_DRAGON_FIREBALL_HIT_RANGE: f32 = 2.0;
/// Seconds a friendly dragon fireball stays airborne before it burns out (reusing the
/// `attack_cd` field as its life timer, like the other fireballs).
const FRIENDLY_DRAGON_FIREBALL_LIFETIME: f32 = 2.4;
/// Minimum seconds between fireballs spat by a [dragon plate](crate::block::DRAGON_PLATE_SPELL)
/// warded player's empty hand — the cadence the server enforces (via the player's
/// `attack_cd`) so the bare-handed breath can't be fired faster than this.
const DRAGON_PLATE_BREATH_INTERVAL: f32 = 0.5;

// --- Demon king (arena boss) ---------------------------------------------------
/// How far (px) the demon king notices a player and gives chase. Generous — it
/// commands the whole arena, so a fighter is never out of its sight for long.
const DEMON_KING_AGGRO: f32 = 460.0;
/// Ground speed of the demon king, in pixels/second. It fights on foot the whole
/// bout. A touch faster than a fleeing target can run.
const DEMON_KING_SPEED: f32 = 46.0;
/// How many dark knights the demon king summons when it enrages (past two-thirds
/// health) — two, marching alongside [`DEMON_KING_DARK_MUSKETEER_COUNT`] dark
/// musketeers.
const DEMON_KING_DARK_KNIGHT_COUNT: u32 = 2;
/// How many dark musketeers the demon king summons when it enrages, fighting at range
/// alongside the [`DEMON_KING_DARK_KNIGHT_COUNT`] dark knights.
const DEMON_KING_DARK_MUSKETEER_COUNT: u32 = 2;
/// Spacing (px) between the dark warriors the king summons as they appear around the
/// player on the arena floor.
const DEMON_KING_KNIGHT_SPACING: f32 = 64.0;
/// Seconds the demon king waits between attacks (the cooldown set when one begins).
const DEMON_KING_ATTACK_INTERVAL: f32 = 1.7;
/// Seconds of attack remaining (of [`crate::entity::DEMON_KING_ATTACK_TIME`]) at
/// which the wind-up gives way to the strike: the frame the king looses its bolts
/// or brings its fists down. Half-way through, like the orc slam.
const DEMON_KING_STRIKE_TIME: f32 = crate::entity::DEMON_KING_ATTACK_TIME * 0.5;
/// Angular spacing (radians) between the five ordinary fireballs in the king's
/// fan — a wide sweep that's hard to sidestep at close range.
const DEMON_KING_FIREBALL_SPREAD: f32 = 0.20;
/// Angular spacing (radians) between the three magic fireballs in the king's
/// tighter, harder-hitting spread.
const DEMON_KING_MAGIC_SPREAD: f32 = 0.28;
/// Reach (px between AABBs) of the demon king's melee slam — wider than the orc's,
/// matching its great size.
const DEMON_KING_SLAM_REACH: f32 = 26.0;
/// Damage the demon king's slam deals to a player it connects with.
const DEMON_KING_SLAM_DAMAGE: i32 = 22;
/// How far to the side of a player (px) a freshly raised demon king first appears,
/// so it makes a dramatic entrance across the arena rather than on top of them.
const DEMON_KING_SPAWN_DIST: f32 = 170.0;
/// How many orc-mage guardians stand watch over the demon king. The king is
/// invulnerable until all of them are slain.
const DEMON_KING_GUARD_COUNT: u32 = 8;
/// Spacing (px) between successive orc-mage guardians as they fan out from the
/// challenger across the arena floor.
const DEMON_KING_GUARD_SPACING: f32 = 90.0;
/// Seconds between arena boss-presence checks (raising the king when the arena
/// holds a player and none yet reigns). Coarse — the boss is a once-per-world event.
const BOSS_CHECK_INTERVAL: f32 = 2.0;

// --- Twinscale (post-game arena superboss) -------------------------------------
/// In-world seconds after the demon king is slain before Twinscale is raised — five
/// full day/night cycles (see [`daylight::DAY_LENGTH_SECS`]).
const TWINSCALE_DELAY_SECS: f32 = 5.0 * daylight::DAY_LENGTH_SECS;
/// How far (px) Twinscale notices a challenger and turns to rain its arsenal down.
const TWINSCALE_AGGRO: f32 = 900.0;
/// Flight speed of Twinscale, in pixels/second — swift for its great size as it wheels
/// over the arena and runs its quarry down. Brisk enough to keep the pressure on a
/// rider who tries to drift away, yet shy of the steed's control speed so a steady
/// flier can still open ground to dodge.
const TWINSCALE_SPEED: f32 = 110.0;
/// How far below the room's open ceiling (px) Twinscale holds station — high out of
/// reach of ground attacks, so a challenger must take to the air to fight it.
const TWINSCALE_HOVER_DROP: f32 = 2.0 * TILE_SIZE;
/// Maximum gap (px between AABBs) at which Twinscale looses an attack — generous enough
/// to reach the floor from its high perch, so it pelts a grounded challenger and
/// pressures them up onto a steed rather than hovering idle out of reach.
const TWINSCALE_SHOOT_RANGE: f32 = 980.0;
/// Horizontal deadzone (px) within which Twinscale stops closing on its target — it
/// flies straight toward the challenger and pulls up only once it is nearly overhead,
/// so it hovers right on top of you rather than keeping a comfortable standoff. That
/// pursuit is what stops you from parking in one spot and letting every volley miss.
const TWINSCALE_PURSUE_DIST: f32 = 40.0;
/// Seconds Twinscale waits between attacks (the cooldown set when one begins).
const TWINSCALE_ATTACK_INTERVAL: f32 = 1.9;
/// Seconds of attack remaining (of [`crate::entity::TWINSCALE_ATTACK_TIME`]) at which
/// the wind-up gives way to the strike — the frame its bolts loose. Halfway through.
const TWINSCALE_STRIKE_TIME: f32 = crate::entity::TWINSCALE_ATTACK_TIME * 0.5;
/// Number of ordinary fireballs in Twinscale's wide fan.
const TWINSCALE_FIREBALL_COUNT: i32 = 10;
/// Angular spacing (radians) between the fireballs in that fan.
const TWINSCALE_FIREBALL_SPREAD: f32 = 0.16;
/// Number of magic fireballs in Twinscale's spread.
const TWINSCALE_MAGIC_COUNT: i32 = 6;
/// Angular spacing (radians) between the magic fireballs in that spread.
const TWINSCALE_MAGIC_SPREAD: f32 = 0.20;
/// Number of summoner bolts Twinscale looses at once (each bursts into a skull).
const TWINSCALE_SUMMONER_COUNT: i32 = 2;
/// Angular spacing (radians) between Twinscale's summoner bolts.
const TWINSCALE_SUMMONER_SPREAD: f32 = 0.30;
/// Horizontal gap (px between centres) within which a challenger counts as "right
/// below" Twinscale, so it forgoes its rolled fan and pours an unbroken stream of magic
/// fireballs straight down on them — a fresh bolt every tick — punishing anyone who
/// tries to hide in its blind spot directly underneath.
const TWINSCALE_BELOW_THRESHOLD: f32 = 90.0;
/// How many dragons Twinscale summons when it drops to half health.
const TWINSCALE_DRAGON_COUNT: u32 = 3;
/// Horizontal spacing (px) between the dragons Twinscale summons as they appear across
/// the upper room.
const TWINSCALE_DRAGON_SPACING: f32 = 200.0;
/// How far Twinscale first appears to the side of the room center (px), so it makes a
/// dramatic entrance sweeping across the high room.
const TWINSCALE_SPAWN_OFFSET: f32 = 240.0;
/// Lumbering speed of an orc mage, in pixels/second — quicker than the brute orc so
/// it can reposition to shepherd its demons, but it never closes for a fight.
const ORC_MAGE_SPEED: f32 = 16.0;
/// How far (px) an orc mage notices a player and shies away from them — it is a
/// support caster and keeps out of melee.
const ORC_MAGE_AGGRO: f32 = 200.0;
/// Gap (px between AABBs) inside which an orc mage backs away from a player.
const ORC_MAGE_FLEE_DIST: f32 = 90.0;
/// How far (px, center-to-center) an orc mage will seek out an ordinary demon to
/// empower, drifting toward it when nothing nearer demands its attention.
const ORC_MAGE_ASSIST_RANGE: f32 = 260.0;
/// Gap (px between AABBs) inside which an orc mage can enchant an ordinary demon.
const ORC_MAGE_ENCHANT_RANGE: f32 = 56.0;
/// Seconds an orc mage waits between enchant casts.
const ORC_MAGE_ENCHANT_INTERVAL: f32 = 6.0;
/// Percent chance that a fresh underworld chunk seeds an orc mage — rarer than the
/// demons it shepherds.
const ORC_MAGE_CHUNK_CHANCE: u32 = 10;
/// Percent chance that an orc mage seeded into a chunk arrives having already
/// enchanted a demon — a partner [`EntityKind::EnchantedDemon`] spawns alongside it,
/// since enchanted demons never spawn on their own.
const ORC_MAGE_PREENCHANTED_CHANCE: u32 = 55;
/// Drifting speed of an ash twister, in pixels/second — it whirls toward players
/// at a steady, unhurried pace.
const ASH_TWISTER_SPEED: f32 = 30.0;
/// How far (px) an ash twister notices a player and begins drifting after them.
const ASH_TWISTER_AGGRO: f32 = 200.0;
/// Maximum gap (px between AABBs) at which an ash twister can catch a player and
/// fling them skyward.
const ASH_TWISTER_ATTACK_RANGE: f32 = 4.0;
/// Upward launch velocity (px/s) an ash twister flings a caught player with. Far
/// stronger than the small [`KNOCKBACK_Y`] pop of an ordinary hit: tuned (against
/// the client's gravity) to throw the player ~20 tiles up so the fall back down
/// comfortably clears the safe-fall window and deals fall damage on landing.
const ASH_TWISTER_LAUNCH: f32 = 950.0;
/// Sideways toss (px/s) added to the launch, so the player is flung up *and* away
/// rather than straight back down onto the twister.
const ASH_TWISTER_TOSS: f32 = 90.0;
/// Direct damage an ash twister's buffeting deals on contact. Deliberately small —
/// the threat is the fall the launch sets up, not the touch itself.
const ASH_TWISTER_DAMAGE: i32 = 2;
/// Seconds an ash twister waits between throws, long enough that a flung player has
/// landed (and can scramble clear) before it can catch them again.
const ASH_TWISTER_ATTACK_INTERVAL: f32 = 2.0;
/// Percent chance that a fresh underworld chunk seeds ash twisters. Only ash-valley
/// columns actually take one (see [`maybe_spawn_ash_twisters`]), so the effective
/// rate across the whole underworld is lower than this.
const ASH_TWISTER_CHUNK_CHANCE: u32 = 40;
/// Most ash twisters a single fresh underworld chunk seeds at once.
const ASH_TWISTER_CHUNK_MAX: u32 = 2;
/// Stalking speed of a necromancer, in pixels/second — it kites the player like a
/// skeleton, repositioning for a clean shot rather than charging in.
const NECROMANCER_SPEED: f32 = 22.0;
/// How far (px) a necromancer notices a player and begins stalking/summoning.
const NECROMANCER_AGGRO: f32 = 240.0;
/// Maximum gap (px between AABBs) at which a necromancer looses a summoner fireball.
const NECROMANCER_SHOOT_RANGE: f32 = 200.0;
/// Standoff gap (px between AABBs) a necromancer tries to keep, backing away from a
/// player closer than this so it can keep summoning from range.
const NECROMANCER_KEEP_DIST: f32 = 110.0;
/// Seconds a necromancer waits between summoner fireballs — slower than a skeleton's
/// bone cadence, since each shot conjures a skull.
const NECROMANCER_SHOOT_INTERVAL: f32 = 2.8;
/// Of the night undead the server spawns near players, the percent that arrive as a
/// necromancer instead of a zombie/skeleton — but only when the spawn lands on the
/// **desert** surface, the necromancer's overworld haunt. So like its skulls it is a
/// nighttime desert threat that burns off at daybreak.
const NECROMANCER_NIGHT_PERCENT: u32 = 35;
/// Percent chance that a fresh **ash valley** (underworld) chunk seeds a necromancer.
const NECROMANCER_ASH_CHUNK_CHANCE: u32 = 22;
/// Flight speed of a hurled summoner fireball, in pixels/second.
const SUMMONER_FIREBALL_SPEED: f32 = 130.0;
/// Damage a summoner fireball deals on directly striking a player — light, since its
/// real menace is the skull it bursts into.
const SUMMONER_FIREBALL_DAMAGE: i32 = 4;
/// Seconds a summoner fireball stays airborne before it gives out and bursts.
const SUMMONER_FIREBALL_LIFETIME: f32 = 2.5;
/// Maximum gap (px between AABBs) at which an in-flight summoner fireball counts as
/// striking a player.
const SUMMONER_FIREBALL_HIT_RANGE: f32 = 2.0;
/// Horizontal hop speed of a bouncing skull, in pixels/second.
const SKULL_SPEED: f32 = 64.0;
/// Upward velocity (px/s, negative is up) a skull springs with each time it lands —
/// a brisk, lively bounce a couple of tiles high.
const SKULL_BOUNCE_VELOCITY: f32 = -340.0;
/// How far (px) a skull notices a player and steers its hops toward them.
const SKULL_AGGRO: f32 = 220.0;
/// Maximum gap (px between AABBs) at which a skull gnashes the player on contact.
const SKULL_ATTACK_RANGE: f32 = 3.0;
/// Damage a skull deals when it catches the player.
const SKULL_DAMAGE: i32 = 5;
/// Seconds a skull waits between bites so a player it's bouncing on isn't chewed
/// every tick.
const SKULL_ATTACK_INTERVAL: f32 = 0.8;
/// Seconds a summoned skull caroms around before it gives out and despawns.
const SKULL_LIFETIME: f32 = 14.0;
/// Radius (world px) of the [sunburst spell](crate::block::SUNBURST_SPELL)'s blast —
/// ten blocks out from the caster, within which every daylight-burning undead is
/// slain at once.
const SUNBURST_RADIUS: f32 = 10.0 * TILE_SIZE;
/// Marching speed of a knight on foot, in pixels/second — brisk enough to keep up
/// with a walking owner and run down the foes it's sent against.
const KNIGHT_SPEED: f32 = 40.0;
/// Marching speed of a *mounted* knight, in pixels/second — a gallop, faster than
/// it manages on foot, to match the horse it rides.
const KNIGHT_MOUNTED_SPEED: f32 = 64.0;
/// How far (px, between AABBs) a knight will range from its owner to engage the
/// enemy its owner last struck. Beyond this it breaks off and returns to heel.
const KNIGHT_AGGRO: f32 = 280.0;
/// Maximum gap (px between AABBs) at which a knight lands a melee blow.
const KNIGHT_ATTACK_RANGE: f32 = 6.0;
/// Damage a knight deals per swing — a hard-hitting man-at-arms.
const KNIGHT_DAMAGE: i32 = 12;
/// Seconds a knight waits between swings.
const KNIGHT_ATTACK_INTERVAL: f32 = 0.7;
/// Gap (px between AABBs) past which a recruited knight that is just following (not
/// fighting) walks toward its owner; inside it, it holds station.
const KNIGHT_FOLLOW_GAP: f32 = 36.0;
/// Center-distance (px) past which a recruited knight teleports to its owner, the
/// way a pet does — generous, so it finishes a fight before snapping back.
const KNIGHT_TELEPORT_DIST: f32 = 360.0;
/// Maximum gap (px between AABBs) at which a knight mounts a nearby *wild* horse.
const KNIGHT_MOUNT_RANGE: f32 = 40.0;
/// Percent chance that a fresh plains chunk seeds a knight — rarer than the horses
/// that share the plains (see [`HORSE_CHUNK_CHANCE`]).
const KNIGHT_CHUNK_CHANCE: u32 = 3;
/// Percent of freshly spawned knights that arrive already mounted (riding behind a
/// horse's worth of [`crate::entity::HORSE_MAX_HEALTH`] shield from the off).
const KNIGHT_SPAWN_MOUNTED_CHANCE: u32 = 30;

/// Marching speed of a mage on foot, in pixels/second — it keeps pace with a walking
/// owner as it shadows them and casts.
const MAGE_SPEED: f32 = 38.0;
/// How far (px, between AABBs) a mage will reach to loose a spell at a foe — a long
/// ranged caster, a touch farther than a knight's charge.
const MAGE_CAST_RANGE: f32 = 300.0;
/// Gap (px between AABBs) past which a recruited mage that is just following (not
/// casting) walks toward its owner; inside it, it holds station.
const MAGE_FOLLOW_GAP: f32 = 40.0;
/// Center-distance (px) past which a recruited mage teleports to its owner, the way a
/// knight (or pet) does.
const MAGE_TELEPORT_DIST: f32 = 360.0;
/// Seconds a mage waits between casts, so it looses a spell at a measured cadence
/// rather than every tick.
const MAGE_CAST_INTERVAL: f32 = 2.2;
/// How near the cursor (world px) a creature must be for a player's restore spell to
/// catch it — generous, so an aimed cast finds the foe under the pointer.
const RESTORE_RANGE: f32 = 4.0 * TILE_SIZE;
/// Seconds a tongue of charred-skeleton trail fire burns before guttering out.
/// Naturally generated fire (part of the terrain) is permanent and untracked.
const TRAIL_FIRE_LIFETIME: f32 = 4.0;
/// Damage a burning cell deals to a player standing in it, applied each time the
/// fire-damage cooldown elapses.
const FIRE_DAMAGE: i32 = 2;
/// Seconds between successive ticks of fire damage to a player standing in flame.
const FIRE_DAMAGE_INTERVAL: f32 = 0.5;
/// Distance (px) from the nearest player beyond which any non-player entity is
/// culled, so creatures and dropped items don't pile up in terrain no one is
/// near. Set comfortably past the screen edge and every spawn distance, so an
/// entity never vanishes in view or pops away the instant it spawns.
const DESPAWN_DIST: f32 = 1200.0;
/// How many spawn slots to scatter around the origin at world start. Each slot
/// spawns whatever creature its biome supports.
const SPAWN_SLOTS: i32 = 12;
/// Cell spacing between adjacent creature spawn slots.
const SPAWN_SPACING: i32 = 7;
/// Downward acceleration applied to simulated entities, in pixels/second².
const GRAVITY: f32 = 1400.0;
/// Terminal fall speed for simulated entities, in pixels/second.
const MAX_FALL: f32 = 900.0;
/// How far (px) a wandering creature strays from its home anchor before turning
/// back, keeping it loitering in a general area instead of marching off forever.
const WANDER_RANGE: f32 = 90.0;
/// Upward velocity (px/s) a ground creature uses to hop a single-block step in
/// its path. Tuned to clear one tile (16px) but not two, so creatures climb
/// gentle terrain without scaling walls.
const HOP_VELOCITY: f32 = -240.0;
/// Collision skin: keeps an entity's trailing edge from snapping into the next
/// cell when it is flush against a block boundary.
const EPS: f32 = 0.01;
/// How far (px) a slime notices and chases a player after dark.
const SLIME_AGGRO: f32 = 140.0;
/// Maximum gap (px between AABBs) at which a slime can land a hit.
const SLIME_ATTACK_RANGE: f32 = 4.0;
/// Damage a slime deals per bite.
const SLIME_DAMAGE: i32 = 3;
/// Seconds a slime waits between bites.
const SLIME_ATTACK_INTERVAL: f32 = 1.0;
/// Player melee reach (max gap, px, between attacker and target AABBs).
const PLAYER_ATTACK_REACH: f32 = 80.0;
/// Horizontal knockback speed (px/s) shoved onto whatever a hit lands on, away
/// from the attacker.
const KNOCKBACK_X: f32 = 180.0;
/// Upward knockback speed (px/s) — a small pop so a hit lifts the target a touch.
const KNOCKBACK_Y: f32 = 240.0;
/// How often the server broadcasts the current time of day, in seconds.
const TIME_BROADCAST_SECS: f32 = 2.0;
/// How often spreading water flows one cell outward, in seconds. Slower than the
/// entity tick so a poured bucket creeps out at a watery pace rather than
/// snapping across the map in a single frame.
const WATER_FLOW_SECS: f32 = 0.3;
/// Upward velocity (px/s) given to a freshly mined block so it visibly pops out
/// of the ground instead of being collected instantly.
const ITEM_POP_VELOCITY: f32 = -120.0;
/// Seconds a dropped item must lie on the ground before it can be picked up.
/// Gives it time to pop clear of the player who mined it.
const ITEM_PICKUP_DELAY: f32 = 0.4;
/// Maximum gap (px between AABBs) at which a player collects a dropped item —
/// i.e. they must essentially be touching it.
const ITEM_PICKUP_REACH: f32 = 2.0;
/// Per-tick horizontal speed retained by a sliding item on the ground (drag).
const ITEM_GROUND_DRAG: f32 = 0.8;
/// Horizontal speed (px/s) given to an item a player deliberately drops, tossed
/// in their facing direction so it lands a little away from them (and so a
/// discarded item doesn't immediately slide back underfoot).
const ITEM_DROP_VELOCITY_X: f32 = 90.0;
/// Seconds a player-dropped item must lie around before anyone (including the
/// dropper) can collect it. Longer than [`ITEM_PICKUP_DELAY`] so the dropper has
/// a moment to step away when discarding or gifting.
const ITEM_DROP_PICKUP_DELAY: f32 = 1.0;
/// How often the world is flushed to disk while running, in seconds.
const AUTOSAVE_SECS: f32 = 30.0;
/// Chebyshev distance (in cells) within which a leaf must find a log to survive.
/// When the last log in range is removed, the leaf decays (breaking, but still
/// dropping its items). Comfortably covers a generated tree's canopy.
const LEAF_SUPPORT_RANGE: i32 = 3;
/// Greatest length (in cells) one placed rope ladder unrolls before its rope
/// runs out. A deeper shaft needs a second rope ladder dropped onto the bottom
/// of the first to carry on down.
const ROPE_LADDER_MAX_DROP: i32 = 8;
/// Longest chat line the server will relay, in characters; longer lines are
/// truncated so a peer can't flood others with a huge message.
const MAX_CHAT_LEN: usize = 256;

/// QUIC application close code the server uses to reject a banned peer (see
/// [`Shared::banned_ips`]). Distinct from [`VERSION_MISMATCH_CLOSE`] so the two
/// rejections can be told apart on the wire; the accompanying reason carries the
/// human-readable explanation the client surfaces.
const BANNED_CLOSE: u32 = 2;

/// QUIC application close code the server uses to reject a failed login (a name
/// already in use, a wrong password, or a malformed/missing `Hello`). Distinct
/// from the version and ban close codes; the accompanying reason carries the
/// human-readable explanation the client shows the player.
const AUTH_FAILED_CLOSE: u32 = 3;

/// Longest player name the server accepts at login, in characters. Keeps names
/// to a sane width for chat attribution and the on-screen label.
const MAX_NAME_LEN: usize = 24;

/// Pseudo-sender attributed to server-originated chat lines: admin command
/// feedback and ban announcements. Distinct from any real player name.
const SERVER_CHAT_FROM: &str = "Server";

/// Chat sender name the demon king's taunt is attributed to, so its line reads as the
/// boss speaking rather than the server.
const DEMON_KING_CHAT_FROM: &str = "Demon King";
/// The taunt the demon king flings at a challenger the instant it is raised (when the
/// player walks into the arena room and the fight begins).
const DEMON_KING_TAUNT: &str =
    "You've done well. But that ends now. Like it did for your king! En garde!";

/// Chat sender name Twinscale's roar is attributed to, so its line reads as the boss
/// speaking rather than the server.
const TWINSCALE_CHAT_FROM: &str = "Twinscale";
/// The roar Twinscale looses as it descends on a challenger, five days after the demon
/// king fell.
const TWINSCALE_TAUNT: &str =
    "Two heads. Twice the fire. You should not have lingered. Now you will burn!";

/// File under the world's save directory holding banned IP addresses, one per
/// line (`#` comments and blanks ignored). Loaded on startup and rewritten
/// whenever a ban is added or lifted, so bans survive restarts.
const BANS_FILE: &str = "bans.txt";

/// Handle to a server running on its own thread + tokio runtime.
pub struct RunningServer {
    pub addr: SocketAddr,
    pub fingerprint: [u8; 32],
    /// The admin secret this server will accept in `Hello` to authorize the host.
    /// Passed to the host's own client so only it is treated as admin.
    pub creator_token: u64,
    // Keeping the endpoint alive keeps the server listening; dropping it (when
    // this handle is dropped) closes the server.
    _endpoint: Endpoint,
    /// Shared state, kept so the world can be flushed to disk when this handle
    /// is dropped (e.g. the player leaves a singleplayer session).
    shared: Arc<Shared>,
    /// Live LAN advertisement, if this server opted into discovery via
    /// [`RunningServer::advertise`]. Dropped (unregistering the service) when
    /// the server is.
    _discovery: Option<crate::discovery::LanAdvertiser>,
    /// Live UPnP port forwarding, if this server opted in via
    /// [`RunningServer::forward_port`]. Dropped (removing the router mapping)
    /// when the server is.
    _upnp: Option<crate::upnp::PortForward>,
    /// The shared MOQ relay, started once when the owner enables voice and/or
    /// webcam (see [`RunningServer::enable_voice`] / [`RunningServer::enable_webcam`]).
    /// Both features ride this single relay, so it is started at most once. Kept
    /// alive so it keeps accepting and forwarding; dropped (stopping it) when the
    /// server is.
    _relay: Option<crate::voice_relay::VoiceRelay>,
    /// The running relay's coordinates (port + cert), cached so a second
    /// `enable_*` call reuses the existing relay instead of starting another.
    relay_info: Option<crate::voice::VoiceInfo>,
    /// UPnP forwarding for the relay port, opened the first time a feature is
    /// enabled with `upnp` set. Separate from [`Self::_upnp`] because each mapping
    /// covers a single port.
    _voice_upnp: Option<crate::upnp::PortForward>,
}

impl RunningServer {
    /// Announce this server on the local network under `name` so nearby clients
    /// can discover and join it without typing an address. Best-effort: a
    /// failure (no mDNS stack, firewall) is logged and otherwise ignored.
    ///
    /// Not called for the embedded singleplayer server, which binds loopback.
    pub fn advertise(&mut self, name: &str) {
        match crate::discovery::advertise(self.addr.port(), name, &self.fingerprint) {
            Ok(a) => self._discovery = Some(a),
            Err(e) => log::warn!("LAN discovery unavailable: {e:#}"),
        }
    }

    /// Ask the local router to forward this server's UDP port via UPnP, so it is
    /// reachable from the internet without manual port forwarding. This exposes
    /// the server publicly — see [`crate::upnp::SECURITY_WARNING`]. Best-effort
    /// and asynchronous: the mapping is set up (and later renewed) on a
    /// background thread, and removed when this handle is dropped.
    pub fn forward_port(&mut self) {
        self._upnp = Some(crate::upnp::PortForward::open(self.addr.port()));
    }

    /// Start the shared media relay on `bind` if it isn't already running, and
    /// return its coordinates. Both voice and webcam ride one relay, so the first
    /// `enable_*` call stands up a separate QUIC endpoint (with its own self-signed
    /// certificate) and — when `upnp` is set — forwards its port on the router;
    /// later calls reuse it and ignore `bind`/`upnp`.
    fn ensure_relay(&mut self, bind: SocketAddr, upnp: bool) -> Result<crate::voice::VoiceInfo> {
        if let Some(info) = &self.relay_info {
            return Ok(info.clone());
        }
        // Called from the main thread, outside any runtime; run the relay setup on
        // the server's own runtime so its QUIC endpoint and accept loop live there.
        let (relay, info) = self
            .rt_handle()
            .block_on(crate::voice_relay::VoiceRelay::start(bind))?;
        if upnp {
            self._voice_upnp = Some(crate::upnp::PortForward::open(info.port));
        }
        self._relay = Some(relay);
        self.relay_info = Some(info.clone());
        Ok(info)
    }

    /// Enable the optional voice chat, so connected players can talk to each other
    /// (see [`crate::voice`]). Off unless the owner calls this: it starts the shared
    /// media relay (if not already up) and records its port and certificate
    /// fingerprint in [`Shared::voice`], handed to each joining client via
    /// [`ServerMessage::Welcome`].
    ///
    /// Returns the actual bound relay port (useful when `bind` used port 0). Errors
    /// if the relay endpoint can't be created (e.g. the port is taken); the rest of
    /// the server keeps running regardless.
    pub fn enable_voice(&mut self, bind: SocketAddr, upnp: bool) -> Result<u16> {
        let info = self.ensure_relay(bind, upnp)?;
        let port = info.port;
        *self.shared.voice.lock() = Some(info);
        Ok(port)
    }

    /// Enable the optional webcam video, a separate toggle from voice that shares
    /// the same relay (see [`crate::voice::video_broadcast_path`]). Starts the
    /// shared media relay if voice didn't already, and records its coordinates in
    /// [`Shared::webcam`] so joining clients learn to connect for video.
    ///
    /// Returns the actual bound relay port. Errors as [`Self::enable_voice`] does.
    pub fn enable_webcam(&mut self, bind: SocketAddr, upnp: bool) -> Result<u16> {
        let info = self.ensure_relay(bind, upnp)?;
        let port = info.port;
        *self.shared.webcam.lock() = Some(info);
        Ok(port)
    }

    /// Enable the live map sharing, which streams player positions and explored
    /// chunks over the same relay (see [`crate::voice::map_broadcast_path`]) so
    /// every client's in-game map shows the others. Unlike voice/webcam this
    /// carries no privacy concern, so it's enabled for every hosted server;
    /// starting the relay if it isn't already up and recording its coordinates in
    /// [`Shared::map`] for joining clients.
    ///
    /// Returns the actual bound relay port. Errors as [`Self::enable_voice`] does.
    pub fn enable_map(&mut self, bind: SocketAddr, upnp: bool) -> Result<u16> {
        let info = self.ensure_relay(bind, upnp)?;
        let port = info.port;
        *self.shared.map.lock() = Some(info);
        Ok(port)
    }

    /// The server's tokio runtime handle (captured at startup in [`Shared`]).
    fn rt_handle(&self) -> tokio::runtime::Handle {
        self.shared.rt.clone()
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        // Stop the autosave loop, then write a final, up-to-date save.
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.save();
    }
}

/// What the server thread reports back once the endpoint is listening (or the
/// error that stopped it): its address, certificate fingerprint, a handle to the
/// endpoint, and the shared state (so the caller can flush it on shutdown).
type Ready = Result<(SocketAddr, [u8; 32], Endpoint, Arc<Shared>)>;

/// One connected client, as seen by the server. The client's player avatar
/// lives in [`Shared::entities`] under the same id; this holds the channel used
/// to push messages to it, plus the bookkeeping admin commands need: the peer's
/// IP (to ban by) and a handle to its QUIC connection (to force-disconnect on a
/// kick or ban).
struct ClientHandle {
    tx: mpsc::UnboundedSender<ServerMessage>,
    /// Remote IP this client connected from — what `/ban <name>` records.
    addr: IpAddr,
    /// Connection handle, kept so an admin's task can [`quinn::Connection::close`]
    /// this client out (the closed stream ends its reader loop, running cleanup).
    conn: quinn::Connection,
}

/// Server world: stored chunks, the generator that fills them on demand, a
/// block registry for solidity queries during entity collision, and the disk
/// store chunks are loaded from and saved to.
struct ServerWorld {
    /// Which dimension's terrain this holds — selects the generator path and the
    /// chunk subdirectory on disk.
    dim: Dimension,
    world: World,
    generator: WorldGen,
    registry: BlockRegistry,
    store: WorldStore,
    /// Chunks modified since the last flush; only these are written to disk.
    dirty: HashSet<ChunkCoord>,
}

impl ServerWorld {
    /// Make sure chunk `(cx, cy)` is resident in memory: load it from disk if it
    /// was saved before, otherwise generate it fresh from the seed. Returns
    /// `true` only when the chunk was generated fresh this call (i.e. it had
    /// never existed before), so callers can react to brand-new terrain coming
    /// into being — e.g. seeding creatures into it.
    fn ensure(&mut self, cx: i32, cy: i32) -> bool {
        if self.world.has_chunk((cx, cy)) {
            return false;
        }
        let (chunk, fresh) = match self.store.load_chunk(self.dim, (cx, cy)) {
            Ok(Some(chunk)) => (chunk, false),
            Ok(None) => (self.generator.generate(self.dim, cx, cy), true),
            Err(e) => {
                log::error!("failed to load chunk ({cx}, {cy}); regenerating: {e:#}");
                (self.generator.generate(self.dim, cx, cy), true)
            }
        };
        self.world.insert_chunk((cx, cy), chunk);
        fresh
    }

    /// Write every dirty chunk to disk, clearing the dirty set on success.
    fn flush_chunks(&mut self) {
        for coord in self.dirty.drain() {
            if let Some(chunk) = self.world.get_chunk(coord)
                && let Err(e) = self.store.save_chunk(self.dim, coord, chunk)
            {
                log::error!("failed to save chunk {coord:?}: {e:#}");
            }
        }
    }

    /// Block contents of chunk `(cx, cy)`, alongside whether this call was what
    /// first brought the chunk into existence (see [`ServerWorld::ensure`]).
    fn chunk_blocks(&mut self, cx: i32, cy: i32) -> (Vec<BlockId>, bool) {
        let fresh = self.ensure(cx, cy);
        let blocks = self
            .world
            .get_chunk((cx, cy))
            .map(|c| c.blocks.to_vec())
            .unwrap_or_else(|| vec![0; CHUNK_AREA]);
        (blocks, fresh)
    }

    /// Read the block at world cell `(x, y)`, generating its chunk on demand so
    /// the value is consistent wherever it is queried.
    fn get(&mut self, x: i32, y: i32) -> BlockId {
        self.ensure(x.div_euclid(CHUNK_SIZE), y.div_euclid(CHUNK_SIZE));
        self.world.get_block(x, y)
    }

    fn set(&mut self, x: i32, y: i32, b: BlockId) -> bool {
        let (cx, cy) = (x.div_euclid(CHUNK_SIZE), y.div_euclid(CHUNK_SIZE));
        self.ensure(cx, cy);
        let changed = self.world.set_block(x, y, b);
        if changed {
            self.dirty.insert((cx, cy));
        }
        changed
    }

    /// Pour a water *source* at `(x, y)` (flow distance 0, so it spreads — see
    /// [`World::place_water_source`]), generating the chunk if needed and marking
    /// it dirty. Returns whether it was placed.
    fn place_water(&mut self, x: i32, y: i32) -> bool {
        let (cx, cy) = (x.div_euclid(CHUNK_SIZE), y.div_euclid(CHUNK_SIZE));
        self.ensure(cx, cy);
        let placed = self.world.place_water_source(x, y);
        if placed {
            self.dirty.insert((cx, cy));
        }
        placed
    }

    /// Surface row for a world column in this dimension (the overworld grass line
    /// or the underworld's charred ceiling), used to place ground-walking entities
    /// when they first spawn.
    fn surface(&self, world_x: i32) -> i32 {
        self.generator.surface_for(self.dim, world_x)
    }

    /// Which biome a world column belongs to, used to spawn the right creatures
    /// for the terrain a column sits in.
    fn biome(&self, world_x: i32) -> Biome {
        self.generator.biome_at(world_x)
    }

    /// Flow resident water one cell outward (see [`World::spread_water_once`]),
    /// marking every newly filled cell's chunk dirty so the flood is saved, and
    /// returning those cells for broadcast.
    fn spread_water(&mut self) -> Vec<(i32, i32)> {
        let filled = self.world.spread_water_once();
        for &(x, y) in &filled {
            self.dirty
                .insert((x.div_euclid(CHUNK_SIZE), y.div_euclid(CHUNK_SIZE)));
        }
        filled
    }

    /// Whether the block at world cell `(tx, ty)` collides with entities,
    /// generating the containing chunk on demand so collision is consistent
    /// wherever a slime wanders.
    fn solid(&mut self, tx: i32, ty: i32) -> bool {
        self.ensure(tx.div_euclid(CHUNK_SIZE), ty.div_euclid(CHUNK_SIZE));
        self.registry.is_solid(self.world.get_block(tx, ty))
    }
}

/// State shared across all connection tasks and the entity tick loop.
struct Shared {
    /// One terrain world per [`Dimension`], indexed by [`Dimension::index`].
    worlds_by_dim: [Mutex<ServerWorld>; NUM_DIMENSIONS],
    clients: Mutex<HashMap<u32, ClientHandle>>,
    /// Live entities split by dimension: players and server-simulated creatures
    /// alike, each in the collection for the dimension it currently occupies. A
    /// player avatar shares the id of its connection.
    entities_by_dim: [Mutex<Entities>; NUM_DIMENSIONS],
    /// Which dimension each connected player currently occupies, keyed by entity
    /// id. Drives where its messages are scoped and which world it interacts with.
    client_dim: Mutex<HashMap<EntityId, Dimension>>,
    next_id: AtomicU32,
    spawn: (f32, f32),
    /// Reference instant the day/night clock counts from. Offset back in time on
    /// load so a resumed world keeps the time of day it was saved at, and movable
    /// at runtime by creator mode's `SetTime` (hence the `Mutex`).
    start: Mutex<Instant>,
    /// Per-server admin secret. Handed only to the host's in-process client (via
    /// [`RunningServer::creator_token`]); a connection that presents it in `Hello`
    /// is the server admin. Never sent to other clients, so a remote joiner cannot
    /// guess it and grant itself admin powers.
    creator_token: u64,
    /// Whether this is a creator-type server: every player may enter creator mode.
    /// On a survival server (`false`), only the admin/host may. Set at world
    /// creation and persisted in [`WorldMeta::creator_server`].
    creator_world: bool,
    /// Entity ids of players currently in creator mode. Hostile creatures ignore
    /// them when picking a target, so monsters never attack a creator. Updated by
    /// [`ClientMessage::SetCreator`] and cleared on disconnect.
    creators: Mutex<HashSet<EntityId>>,
    /// Root columns of embedded worldgen structures whose creatures have already
    /// been spawned. Consulted the first time a structure's chunk is generated so
    /// its creatures appear exactly once. Persisted in
    /// [`WorldMeta::spawned_structure_roots`].
    spawned_structure_roots: Mutex<HashSet<i32>>,
    /// Saved state of every player who has joined, keyed by name. A player is
    /// moved out of here (into a live entity) while connected and folded back in
    /// on disconnect, so it survives both reconnects and restarts.
    saved_players: Mutex<HashMap<String, SavedPlayer>>,
    /// Account credentials keyed by player name: the encoded, salted password
    /// hash (see [`crate::auth`]) for everyone who has registered on this world.
    /// Consulted at handshake time to authenticate a `Hello`, and added to when a
    /// new name first joins. Unlike [`Self::saved_players`], an entry is *not*
    /// removed while the player is connected — it is the durable record of who
    /// owns a name. Persisted in [`WorldMeta::accounts`] so logins survive
    /// restarts.
    accounts: Mutex<HashMap<String, String>>,
    /// Slot inventory of every currently-connected player, keyed by entity id.
    /// Authoritative: placements consume from it and pickups add to it. Folded
    /// into [`SavedPlayer`] on disconnect so it persists.
    inventories: Mutex<HashMap<EntityId, Inventory>>,
    /// Banked mana of every currently-connected player, keyed by entity id. Mana is
    /// the magic resource: it is won by slaying monsters (see [`creature_mana`]) and
    /// spent casting spellbooks (see [`crate::block::SUMMONER_SPELL`]). Capped at
    /// [`MANA_MAX`]. Folded into [`SavedPlayer`] on disconnect so it persists.
    mana: Mutex<HashMap<EntityId, i32>>,
    /// Campfire cell each connected player last interacted with, keyed by entity
    /// id. Death returns the player to this campfire (instead of world
    /// [`spawn`](Self::spawn)) — but only if the campfire is still there; a broken
    /// one falls back to world spawn. Folded into [`SavedPlayer`] on disconnect so
    /// it survives reconnects.
    respawn_points: Mutex<HashMap<EntityId, (Dimension, i32, i32)>>,
    /// Personal map waypoints of every currently-connected player, keyed by
    /// entity id. Folded into [`SavedPlayer`] on disconnect so they persist.
    waypoints: Mutex<HashMap<EntityId, Vec<Waypoint>>>,
    /// Lit campfires, keyed by `(dimension, x, y)`, holding each one's remaining
    /// burn time in seconds. A cell is present only while its campfire is lit; the
    /// tick loop counts each down and extinguishes it (reverting the block) at
    /// zero. Persisted in [`WorldMeta`] so fires survive a save/reload.
    campfires: Mutex<HashMap<(Dimension, i32, i32), f32>>,
    /// Cells holding a log the player placed (rather than naturally grown), keyed
    /// by `(dimension, x, y)`. Tracked so an axe's tree-felling leaves player-built
    /// log structures standing — a placed log is neither chopped nor traversed by
    /// the fell. Persisted in [`WorldMeta`] so the distinction survives a reload.
    placed_logs: Mutex<HashSet<(Dimension, i32, i32)>>,
    /// Tongues of fire laid down by chasing charred skeletons, keyed by
    /// `(dimension, x, y)` with each one's remaining burn time. The tick loop
    /// counts them down and reverts the cell to air at zero. Runtime-only (these
    /// flames are fleeting, so they aren't persisted).
    trail_fires: Mutex<HashMap<(Dimension, i32, i32), f32>>,
    /// Seconds until each player can next take fire damage, keyed by entity id, so
    /// standing in flame burns at a steady interval rather than every tick.
    fire_cd: Mutex<HashMap<EntityId, f32>>,
    /// The enemy each player has most recently struck, keyed by player entity id, so
    /// that player's recruited [`EntityKind::Knight`] knows whom to charge. Set when
    /// a player lands a hit on a hostile creature; entity ids are never reused, so a
    /// stale entry (its enemy already dead) simply matches nothing. Runtime-only.
    knight_targets: Mutex<HashMap<EntityId, EntityId>>,
    /// The earliest instant each player may fire their musket again, keyed by entity
    /// id, so the server enforces the firearm's slow reload (a fast-firing client
    /// can't bypass [`MUSKET_COOLDOWN`]). An absent entry means "ready now". Runtime
    /// only, like the other transient combat timers.
    musket_cd: Mutex<HashMap<EntityId, Instant>>,
    /// Set when the owning [`RunningServer`] is dropped, to stop the autosave
    /// loop.
    shutdown: AtomicBool,
    /// The optional voice-chat relay's coordinates (port + certificate
    /// fingerprint), present only while the owner has enabled voice via
    /// [`RunningServer::enable_voice`]. Read when building each
    /// [`ServerMessage::Welcome`] so joining clients learn where the relay is and
    /// which certificate to trust. `None` means voice is off.
    voice: Mutex<Option<crate::voice::VoiceInfo>>,
    /// The optional webcam relay's coordinates, present only while the owner has
    /// enabled webcam via [`RunningServer::enable_webcam`]. Shares the relay with
    /// [`Self::voice`] (so when both are on the coordinates match), but is a
    /// separate toggle. `None` means webcam is off.
    webcam: Mutex<Option<crate::voice::VoiceInfo>>,
    /// The live-map relay's coordinates, present once the owner has enabled the
    /// map via [`RunningServer::enable_map`] (done for every hosted server).
    /// Shares the relay with [`Self::voice`]/[`Self::webcam`]. `None` means map
    /// sharing is off (e.g. before the relay starts). See [`crate::client::map`].
    map: Mutex<Option<crate::voice::VoiceInfo>>,
    /// Handle to the server's own tokio runtime, captured at startup. Lets
    /// [`RunningServer::enable_voice`] — called from the main thread, outside any
    /// runtime — spin up the MOQ relay's QUIC endpoint and accept loop on this
    /// runtime.
    rt: tokio::runtime::Handle,
    /// IP addresses barred from connecting. Checked at handshake time (a banned
    /// peer is closed out before it joins) and added to by an admin's `/ban`.
    /// Persisted to [`BANS_FILE`] under the save dir so bans outlive a restart.
    banned_ips: Mutex<HashSet<IpAddr>>,
    /// Path to the on-disk ban list, rewritten whenever [`Self::banned_ips`]
    /// changes.
    bans_path: PathBuf,
    /// Where each spectating admin's avatar was (dimension + pixel position) when
    /// it began spectating, keyed by admin entity id. Restored — and the entry
    /// removed — when the admin stops spectating, so `/spectate` always returns
    /// them whence they came. Empty for anyone not currently spectating.
    spectate_return: Mutex<HashMap<EntityId, (Dimension, f32, f32)>>,
    /// Where each connected player last *used the fire key* in each dimension, as a
    /// world-pixel position indexed by [`Dimension::index`] and keyed by entity id.
    /// Firing the key returns the player to the spot they last fired it from in the
    /// destination dimension, so the key behaves like a linked pair of portals (one
    /// anchor per dimension). `None` for a dimension the player has not yet keyed
    /// from — then a fresh landing spot is chosen instead. Runtime-only.
    fire_marks: Mutex<HashMap<EntityId, [Option<(f32, f32)>; NUM_DIMENSIONS]>>,
    /// Player-written text on signs and quest boards, keyed by `(dimension, x, y)`.
    /// A cell is present only while its board holds non-blank text; breaking the
    /// board drops the entry. Synced to clients as they stream the chunk and on
    /// every edit, and persisted in [`WorldMeta`] so the writing survives a reload.
    block_text: Mutex<HashMap<(Dimension, i32, i32), crate::protocol::BlockText>>,
    /// Contents of every chest (plain or locked) in the world, keyed by
    /// `(dimension, x, y)`, each a [`CHEST_SLOTS`]-long slot list. Created lazily on
    /// first open and dropped (its items spilled) when the chest is broken.
    /// Persisted in [`WorldMeta`].
    chest_store: Mutex<HashMap<(Dimension, i32, i32), Vec<Slot>>>,
    /// Passwords of locked chests, keyed by `(dimension, x, y)`. A cell is present
    /// only while its chest is locked. Persisted in [`WorldMeta`].
    chest_locks: Mutex<HashMap<(Dimension, i32, i32), String>>,
    /// Which chest each connected player currently has open, keyed by entity id, so
    /// content updates are streamed only to a chest's active viewers. Runtime-only.
    chest_viewers: Mutex<HashMap<EntityId, (Dimension, i32, i32)>>,
    /// Locked chests each player has unlocked this session (entered the right
    /// password for), keyed by entity id. A standing unlock lets them reopen and
    /// break that chest without re-entering the password. Runtime-only.
    chest_auth: Mutex<HashMap<EntityId, HashSet<(Dimension, i32, i32)>>>,
    /// Where each player was standing when they last used the [arena
    /// key](crate::block::ARENA_KEY) to enter the [`Dimension::Arena`], as
    /// `(dimension, x, y)` in world pixels, so using the key again from inside the
    /// arena returns them to exactly where they left. Runtime-only — a reconnecting
    /// player simply re-marks it on their next entry. Keyed by entity id.
    arena_return: Mutex<HashMap<EntityId, (Dimension, f32, f32)>>,
    /// Whether this world's single [`EntityKind::DemonKing`] has been slain. Set
    /// once the boss falls and persisted in [`WorldMeta`]; while it is `true` no new
    /// king is ever raised in the arena (one boss per world).
    demon_king_slain: AtomicBool,
    /// Whether a live [`EntityKind::DemonKing`] currently presides over the arena,
    /// so the spawn check doesn't have to scan the entity map every tick. Runtime
    /// mirror of "a king is in `entities(Arena)`". Starts `false`; set when one is
    /// raised, cleared when it dies or despawns.
    demon_king_alive: AtomicBool,
    /// Whether the reigning demon king has already summoned its host of dark knights
    /// (it does so once, the moment it enrages past two-thirds health).
    /// Reset when a fresh king is raised, so each king's second phase summons its own
    /// host exactly once. Runtime-only.
    demon_king_knights_summoned: AtomicBool,
    /// In-world time (matching [`Self::time_of_day`]'s elapsed-seconds clock) at which
    /// the demon king was slain, or `None` while it still lives. The post-game
    /// [`EntityKind::Twinscale`] boss is raised [`TWINSCALE_DELAY_SECS`] later.
    /// Persisted in [`WorldMeta`] so the countdown survives a restart.
    demon_king_slain_at: Mutex<Option<f32>>,
    /// Whether this world's single [`EntityKind::Twinscale`] post-game boss has been
    /// slain. Persisted; while `true` it is never raised again (one per world).
    twinscale_slain: AtomicBool,
    /// Whether a live [`EntityKind::Twinscale`] currently haunts the arena — the
    /// runtime mirror of "a Twinscale is in `entities(Arena)`", so the spawn check is
    /// cheap. Set when one is raised, cleared when it dies.
    twinscale_alive: AtomicBool,
    /// Whether the reigning Twinscale has already called down its flight of dragons (it
    /// does so once, the moment it drops to half health). Reset when a fresh Twinscale
    /// is raised. Runtime-only.
    twinscale_dragons_summoned: AtomicBool,
}

impl Shared {
    fn alloc_id(&self) -> EntityId {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// The terrain world for `dim`.
    fn world(&self, dim: Dimension) -> &Mutex<ServerWorld> {
        &self.worlds_by_dim[dim.index()]
    }

    /// The entity collection for `dim`.
    fn entities(&self, dim: Dimension) -> &Mutex<Entities> {
        &self.entities_by_dim[dim.index()]
    }

    /// The dimension player `id` currently occupies (defaulting to the overworld
    /// for an id the map hasn't recorded yet).
    fn dim_of(&self, id: EntityId) -> Dimension {
        self.client_dim.lock().get(&id).copied().unwrap_or_default()
    }

    /// Send `msg` to every client currently in dimension `dim`.
    fn broadcast_dim(&self, dim: Dimension, msg: ServerMessage) {
        let client_dim = self.client_dim.lock();
        for (cid, h) in self.clients.lock().iter() {
            if client_dim.get(cid).copied().unwrap_or_default() == dim {
                let _ = h.tx.send(msg.clone());
            }
        }
    }

    /// Record that the demon king has fallen: clear the "alive" mirror, latch the
    /// "slain" flag (so no new king is ever raised), and — the first time — stamp the
    /// in-world time, starting the [`TWINSCALE_DELAY_SECS`] countdown to the post-game
    /// [`EntityKind::Twinscale`] boss. Called from every site that can kill the king.
    fn record_demon_king_slain(&self) {
        self.demon_king_alive.store(false, Ordering::SeqCst);
        self.demon_king_slain.store(true, Ordering::SeqCst);
        let now = self.start.lock().elapsed().as_secs_f32();
        let mut at = self.demon_king_slain_at.lock();
        if at.is_none() {
            *at = Some(now);
        }
    }

    /// Record that Twinscale has fallen: clear its "alive" mirror and latch the "slain"
    /// flag so the post-game boss is never raised again. Called from every site that
    /// can kill it.
    fn record_twinscale_slain(&self) {
        self.twinscale_alive.store(false, Ordering::SeqCst);
        self.twinscale_slain.store(true, Ordering::SeqCst);
    }

    /// Send `msg` to every client in dimension `dim` except `except`.
    fn broadcast_dim_except(&self, dim: Dimension, except: EntityId, msg: ServerMessage) {
        let client_dim = self.client_dim.lock();
        for (cid, h) in self.clients.lock().iter() {
            if *cid != except && client_dim.get(cid).copied().unwrap_or_default() == dim {
                let _ = h.tx.send(msg.clone());
            }
        }
    }

    /// Current normalized time of day in `[0, 1)`.
    fn time_of_day(&self) -> f32 {
        daylight::time_of_day(self.start.lock().elapsed().as_secs_f32())
    }

    /// A pseudo-random value in `[0, 1)`, mixed from the entity counter and the
    /// sub-second clock so successive rolls (e.g. for randomized leaf drops) vary.
    /// Not for anything that needs to be reproducible.
    fn rand_unit(&self) -> f32 {
        let a = self.next_id.load(Ordering::Relaxed);
        let b = self.start.lock().elapsed().subsec_nanos();
        let mut h = a
            .wrapping_mul(2_654_435_761)
            .wrapping_add(b.wrapping_mul(40_503));
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 13;
        (h & 0x00FF_FFFF) as f32 / 16_777_216.0
    }

    /// Flush the whole world to disk: dirty chunks plus a fresh `world.dat`
    /// snapshot of the clock, entities, and players. Safe to call from any
    /// thread; logs (rather than propagates) IO errors so a failed save can
    /// never crash the server.
    fn save(&self) {
        // Flush each dimension's dirty chunks; read the seed from the overworld
        // (both dimensions share the same generator seed) and keep its store for
        // the metadata write.
        let seed = {
            let mut overworld = self.world(Dimension::Overworld).lock();
            overworld.flush_chunks();
            let seed = overworld.generator.seed();
            self.world(Dimension::Underworld).lock().flush_chunks();
            self.world(Dimension::Arena).lock().flush_chunks();
            seed
        };

        // Creatures save directly (split by dimension); players are gathered from
        // both the saved set and any currently-connected avatars (whose live state
        // is freshest), tagged with the dimension they are in.
        let mut players = self.saved_players.lock().clone();
        let invs = self.inventories.lock().clone();
        let manas = self.mana.lock().clone();
        let mut creatures: [Vec<Entity>; NUM_DIMENSIONS] = Default::default();
        for dim in Dimension::ALL {
            for e in self.entities(dim).lock().values() {
                match &e.kind {
                    EntityKind::Player { name } if !name.is_empty() => {
                        players.insert(
                            name.clone(),
                            SavedPlayer {
                                name: name.clone(),
                                x: e.x,
                                y: e.y,
                                health: e.health,
                                inventory: invs.get(&e.id).cloned().unwrap_or_default(),
                                dim,
                                respawn: self.respawn_points.lock().get(&e.id).copied(),
                                waypoints: self
                                    .waypoints
                                    .lock()
                                    .get(&e.id)
                                    .cloned()
                                    .unwrap_or_default(),
                                mana: manas.get(&e.id).copied().unwrap_or(0),
                            },
                        );
                    }
                    EntityKind::Player { .. } => {} // unnamed: not yet identified
                    _ => creatures[dim.index()].push(e.clone()),
                }
            }
        }
        let [overworld_entities, underworld_entities, arena_entities] = creatures;

        let campfires = self
            .campfires
            .lock()
            .iter()
            .map(|(&(dim, x, y), &secs)| (dim, x, y, secs))
            .collect();
        let placed_logs = self.placed_logs.lock().iter().copied().collect();
        let block_text = self
            .block_text
            .lock()
            .iter()
            .map(|(&(dim, x, y), text)| (dim, x, y, text.clone()))
            .collect();
        let chests = self
            .chest_store
            .lock()
            .iter()
            .map(|(&(dim, x, y), slots)| (dim, x, y, slots.clone()))
            .collect();
        let chest_locks = self
            .chest_locks
            .lock()
            .iter()
            .map(|(&(dim, x, y), pw)| (dim, x, y, pw.clone()))
            .collect();
        let spawned_structure_roots = self
            .spawned_structure_roots
            .lock()
            .iter()
            .copied()
            .collect();

        let meta = WorldMeta {
            seed,
            elapsed_secs: self.start.lock().elapsed().as_secs_f32(),
            next_id: self.next_id.load(Ordering::SeqCst),
            spawn: self.spawn,
            entities: overworld_entities,
            underworld_entities,
            arena_entities,
            demon_king_slain: self.demon_king_slain.load(Ordering::SeqCst),
            demon_king_slain_at: *self.demon_king_slain_at.lock(),
            twinscale_slain: self.twinscale_slain.load(Ordering::SeqCst),
            players: players.into_values().collect(),
            campfires,
            placed_logs,
            block_text,
            chests,
            chest_locks,
            accounts: self
                .accounts
                .lock()
                .iter()
                .map(|(name, hash)| (name.clone(), hash.clone()))
                .collect(),
            creator_server: self.creator_world,
            spawned_structure_roots,
        };
        if let Err(e) = self
            .world(Dimension::Overworld)
            .lock()
            .store
            .save_meta(&meta)
        {
            log::error!("failed to save world: {e:#}");
        }
    }
}

/// Normalize and validate a submitted login password. Trims surrounding
/// whitespace (matching what the client does) and rejects a blank or
/// whitespace-only password, so the "a password is required" rule can't be
/// bypassed by a modified client sending spaces. Returns the trimmed password to
/// hash or verify; the trim never affects a legitimate login, which the official
/// client already trims before sending.
fn normalize_password(password: &str) -> Result<&str, String> {
    let trimmed = password.trim();
    if trimmed.is_empty() {
        return Err("A password is required to join this server.".to_string());
    }
    Ok(trimmed)
}

impl Shared {
    fn broadcast_all(&self, msg: ServerMessage) {
        for h in self.clients.lock().values() {
            let _ = h.tx.send(msg.clone());
        }
    }

    /// Send a message to a single client by id, if still connected.
    fn send_to(&self, id: u32, msg: ServerMessage) {
        if let Some(h) = self.clients.lock().get(&id) {
            let _ = h.tx.send(msg);
        }
    }

    /// Send a `Server`-attributed chat line to one client — used to give an admin
    /// private feedback on a command (e.g. "Player not found").
    fn notify(&self, id: EntityId, text: impl Into<String>) {
        self.send_to(
            id,
            ServerMessage::Chat {
                from: SERVER_CHAT_FROM.to_string(),
                text: text.into(),
            },
        );
    }

    /// Broadcast a `Server`-attributed chat line to everyone — used to announce
    /// moderation actions (e.g. "Bob was banned").
    fn announce(&self, text: impl Into<String>) {
        self.broadcast_all(ServerMessage::Chat {
            from: SERVER_CHAT_FROM.to_string(),
            text: text.into(),
        });
    }

    /// Authenticate a `Hello`'s `name`/`password` before the player is admitted.
    /// Returns `Ok(())` to allow the join, or `Err(reason)` with a player-facing
    /// explanation to reject it. Enforces three things:
    ///
    /// 1. The name is non-empty and not absurdly long.
    /// 2. No other *connected* player is already using that name — so two people
    ///    can never be online under the same identity at once.
    /// 3. The password registers a brand-new account (first join under this name)
    ///    or matches the salted hash stored for an existing one — so nobody can
    ///    take over someone else's name without their password.
    ///
    /// A new account's hash is recorded here, so the credential exists from the
    /// moment the player joins (it is persisted on the next save).
    fn authenticate(&self, name: &str, password: &str) -> Result<(), String> {
        if name.is_empty() {
            return Err("Please choose a name.".to_string());
        }
        if name.chars().count() > MAX_NAME_LEN {
            return Err(format!("Name is too long (max {MAX_NAME_LEN} characters)."));
        }
        // Normalize and require a real password before doing anything with it.
        let password = normalize_password(password)?;
        // Refuse a name that someone is currently connected under.
        if self.find_player_by_name(name).is_some() {
            return Err("That name is already in use by another player.".to_string());
        }
        // Register a new account, or verify the password of an existing one.
        let mut accounts = self.accounts.lock();
        match accounts.get(name) {
            Some(stored) => {
                if auth::verify_password(password, stored) {
                    Ok(())
                } else {
                    Err("Incorrect password for that name.".to_string())
                }
            }
            None => {
                accounts.insert(name.to_string(), auth::hash_password(password));
                log::info!("registered new account '{name}'");
                Ok(())
            }
        }
    }

    /// Find the connected player whose name matches `name` (case-insensitively),
    /// returning their entity id. Names live on the player's [`Entity`], so this
    /// scans the live entities of every dimension.
    fn find_player_by_name(&self, name: &str) -> Option<EntityId> {
        for dim in Dimension::ALL {
            for e in self.entities(dim).lock().values() {
                if let EntityKind::Player { name: n } = &e.kind
                    && n.eq_ignore_ascii_case(name)
                {
                    return Some(e.id);
                }
            }
        }
        None
    }

    /// The display name of connected player `id`, or `Player {id}` if it hasn't
    /// identified yet (matching the chat attribution fallback).
    fn player_name(&self, id: EntityId) -> String {
        self.entities(self.dim_of(id))
            .lock()
            .get(id)
            .and_then(|e| match &e.kind {
                EntityKind::Player { name } if !name.is_empty() => Some(name.clone()),
                _ => None,
            })
            .unwrap_or_else(|| format!("Player {id}"))
    }

    /// The world position of player `id`, if it is a live entity.
    fn player_pos(&self, id: EntityId) -> Option<(f32, f32)> {
        self.entities(self.dim_of(id))
            .lock()
            .get(id)
            .map(|e| (e.x, e.y))
    }

    /// Forcibly disconnect client `id` with `reason`: closing its QUIC connection
    /// ends its reader loop, which runs the usual cleanup (saving and despawning
    /// the player). A no-op if it has already gone.
    fn kick(&self, id: EntityId, reason: &str) {
        if let Some(h) = self.clients.lock().get(&id) {
            h.conn.close(BANNED_CLOSE.into(), reason.as_bytes());
        }
    }

    /// Persist the current ban set to [`Self::bans_path`]. Best-effort: a write
    /// failure is logged but doesn't disturb the in-memory set (so the ban still
    /// holds for this session even if it can't be saved for the next).
    fn save_bans(&self) {
        let mut out = String::from("# survival-cubed banned IP addresses (one per line)\n");
        for ip in self.banned_ips.lock().iter() {
            out.push_str(&ip.to_string());
            out.push('\n');
        }
        if let Err(e) = std::fs::write(&self.bans_path, out) {
            log::error!("failed to save ban list: {e:#}");
        }
    }

    /// Handle a `/`-prefixed chat line from the admin (the host connection that
    /// presented the token). Returns nothing; all outcomes are reported to the admin
    /// (or broadcast) as `Server` chat. Unknown commands get a hint.
    fn handle_admin_command(&self, admin: EntityId, line: &str) {
        let mut parts = line[1..].split_whitespace();
        let Some(cmd) = parts.next() else {
            return; // a bare "/" — ignore.
        };
        let arg = parts.next().map(str::to_string);
        match cmd.to_ascii_lowercase().as_str() {
            "ban" => self.cmd_ban(admin, arg),
            "unban" => self.cmd_unban(admin, arg),
            "banlist" | "bans" => self.cmd_banlist(admin),
            "spectate" | "spec" => self.cmd_spectate(admin, arg),
            "help" => self.notify(
                admin,
                "Admin commands: /ban <name|ip>, /unban <ip>, /banlist, /spectate <name>, /spectate (to stop)",
            ),
            other => self.notify(admin, format!("Unknown command '/{other}'. Try /help.")),
        }
    }

    /// `/ban <name|ip>`: ban a connected player (resolved to their IP) or a raw
    /// IP address. The IP is recorded persistently and any connected client on it
    /// is kicked.
    fn cmd_ban(&self, admin: EntityId, arg: Option<String>) {
        let Some(arg) = arg else {
            self.notify(admin, "Usage: /ban <player name or IP>");
            return;
        };

        // Resolve the target IP: a literal address bans that address directly;
        // otherwise treat the argument as a connected player's name.
        let (ip, who) = if let Ok(ip) = arg.parse::<IpAddr>() {
            (ip, arg.clone())
        } else {
            match self.find_player_by_name(&arg) {
                Some(target) if target == admin => {
                    self.notify(admin, "You can't ban yourself.");
                    return;
                }
                Some(target) => {
                    let ip = self.clients.lock().get(&target).map(|h| h.addr);
                    match ip {
                        Some(ip) => (ip, self.player_name(target)),
                        None => {
                            self.notify(admin, format!("'{arg}' is not connected."));
                            return;
                        }
                    }
                }
                None => {
                    self.notify(admin, format!("No connected player named '{arg}'."));
                    return;
                }
            }
        };

        let newly = self.banned_ips.lock().insert(ip);
        if !newly {
            self.notify(admin, format!("{ip} is already banned."));
            return;
        }
        self.save_bans();

        // Kick everyone currently connected from that IP (an admin may ban an IP
        // shared by several players, or one they typed directly).
        let victims: Vec<EntityId> = self
            .clients
            .lock()
            .iter()
            .filter(|(_, h)| h.addr == ip)
            .map(|(id, _)| *id)
            .collect();
        for v in victims {
            self.kick(v, "You have been banned from this server.");
        }
        self.announce(format!("{who} ({ip}) was banned by an admin."));
        log::info!("admin {admin} banned {who} ({ip})");
    }

    /// `/unban <ip>`: lift a ban on an IP address.
    fn cmd_unban(&self, admin: EntityId, arg: Option<String>) {
        let Some(arg) = arg else {
            self.notify(admin, "Usage: /unban <IP>");
            return;
        };
        let Ok(ip) = arg.parse::<IpAddr>() else {
            self.notify(admin, format!("'{arg}' is not a valid IP address."));
            return;
        };
        if self.banned_ips.lock().remove(&ip) {
            self.save_bans();
            self.notify(admin, format!("{ip} has been unbanned."));
            log::info!("admin {admin} unbanned {ip}");
        } else {
            self.notify(admin, format!("{ip} is not banned."));
        }
    }

    /// `/banlist`: privately list the banned IPs to the admin.
    fn cmd_banlist(&self, admin: EntityId) {
        let bans = self.banned_ips.lock();
        if bans.is_empty() {
            self.notify(admin, "No IPs are banned.");
            return;
        }
        let list = bans
            .iter()
            .map(|ip| ip.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        self.notify(admin, format!("Banned IPs: {list}"));
    }

    /// `/spectate <name>` to view a player, or `/spectate` with no argument to
    /// stop. Starting moves the admin alongside the target (across dimensions if
    /// needed, after first remembering where the admin was) so the target streams
    /// in; the [`ServerMessage::Spectate`] then locks the admin's camera onto it.
    /// Stopping returns the admin to its remembered position.
    fn cmd_spectate(&self, admin: EntityId, arg: Option<String>) {
        match arg {
            None => self.stop_spectating(admin),
            Some(name) => {
                let Some(target) = self.find_player_by_name(&name) else {
                    self.notify(admin, format!("No connected player named '{name}'."));
                    return;
                };
                if target == admin {
                    self.notify(admin, "You can't spectate yourself.");
                    return;
                }
                let target_dim = self.dim_of(target);
                let Some((tx, ty)) = self.player_pos(target) else {
                    self.notify(admin, "That player just left.");
                    return;
                };

                // Remember where the admin was, the first time it starts
                // spectating, so a later /spectate returns it exactly there.
                if let Some((dim, x, y)) = self
                    .player_pos(admin)
                    .map(|(x, y)| (self.dim_of(admin), x, y))
                {
                    self.spectate_return
                        .lock()
                        .entry(admin)
                        .or_insert((dim, x, y));
                }

                // Move the admin alongside the target so the target (and the world
                // around it) streams to the admin. enter_dimension also handles the
                // same-dimension case (it re-streams the local entities).
                self.enter_dimension(admin, target_dim, tx, ty);
                self.send_to(
                    admin,
                    ServerMessage::Spectate {
                        target: Some(target),
                    },
                );
                self.notify(
                    admin,
                    format!(
                        "Now spectating {}. Type /spectate to stop.",
                        self.player_name(target)
                    ),
                );
            }
        }
    }

    /// End spectating for `admin`: release its camera and return it to the
    /// position it held before it began. A no-op if it wasn't spectating.
    fn stop_spectating(&self, admin: EntityId) {
        let Some((dim, x, y)) = self.spectate_return.lock().remove(&admin) else {
            self.notify(admin, "You aren't spectating anyone.");
            return;
        };
        self.enter_dimension(admin, dim, x, y);
        self.send_to(admin, ServerMessage::Spectate { target: None });
        self.notify(admin, "Stopped spectating.");
    }

    /// The suit of armor player `id` is currently protected by: the sturdiest one
    /// in their inventory (armor is worn simply by carrying it, so the best suit on
    /// hand applies). `None` if they carry none. The blunting it grants is
    /// [`crate::block::armor_defense`]; the suit named here is the one that takes the
    /// wear when it soaks a blow.
    fn worn_armor(&self, id: EntityId) -> Option<BlockId> {
        let invs = self.inventories.lock();
        invs.get(&id)?
            .slots()
            .iter()
            .filter_map(|s| s.map(|(b, ..)| b))
            .filter(|b| crate::block::armor_defense(*b) > 0)
            .max_by_key(|b| crate::block::armor_defense(*b))
    }

    /// Add one `block` to player `id`'s inventory, stacking into existing stacks
    /// first. Returns whether it fit (false only if the inventory is full).
    fn add_item(&self, id: EntityId, block: BlockId) -> bool {
        self.inventories.lock().entry(id).or_default().add(block, 1) == 0
    }

    /// Add a dropped stack (`count` of `block` carrying `durability`) to player
    /// `id`'s inventory, preserving a tool's wear. Returns the amount that did
    /// not fit (0 when all of it was stored).
    fn add_stack(&self, id: EntityId, block: BlockId, count: u32, durability: u16) -> u32 {
        self.inventories
            .lock()
            .entry(id)
            .or_default()
            .add_stack(block, count, durability)
    }

    /// Drop the contents of player `id`'s inventory `slot` onto the ground at
    /// their feet so it can be discarded or gifted. `all` drops the whole stack;
    /// otherwise a single item is dropped. `dir` is the player's facing
    /// (`-1.0` left, `+1.0` right) so the drop is tossed clear of them. The drop
    /// keeps a tool's durability. No-op if the slot is empty or the player has no
    /// position yet.
    fn drop_item(&self, id: EntityId, slot: usize, all: bool, dir: f32) {
        let taken = {
            let mut invs = self.inventories.lock();
            let Some(inv) = invs.get_mut(&id) else {
                return;
            };
            if all {
                inv.take_slot(slot)
            } else {
                inv.take_one_full(slot)
            }
        };
        let Some((block, count, durability)) = taken else {
            return;
        };
        // Spawn at the player's center so the toss reads as coming from them.
        let dim = self.dim_of(id);
        let origin = self.entities(dim).lock().get(id).map(|e| {
            let (pw, ph) = e.size();
            let (iw, ih) = ITEM_SIZE;
            (e.x + (pw - iw) * 0.5, e.y + (ph - ih) * 0.5)
        });
        let Some((x, y)) = origin else {
            return;
        };
        let vx = if dir < 0.0 {
            -ITEM_DROP_VELOCITY_X
        } else {
            ITEM_DROP_VELOCITY_X
        };
        spawn_item(
            self,
            dim,
            block,
            count,
            durability,
            x,
            y,
            vx,
            ITEM_POP_VELOCITY,
            ITEM_DROP_PICKUP_DELAY,
        );
    }

    /// Remove one item from hotbar/inventory `slot` of player `id`, returning the
    /// block taken (or `None` if the slot was empty). Used to pay for placement.
    fn take_from_slot(&self, id: EntityId, slot: usize) -> Option<BlockId> {
        self.inventories.lock().get_mut(&id)?.take_one(slot)
    }

    /// Read (without removing) the block in hotbar/inventory `slot` of player
    /// `id`. Used to validate placement before committing to spend the item.
    fn peek_slot(&self, id: EntityId, slot: usize) -> Option<BlockId> {
        self.inventories.lock().get(&id)?.get(slot).map(|(b, ..)| b)
    }

    /// Rearrange player `id`'s inventory by moving slot `from` onto slot `to`.
    fn move_item(&self, id: EntityId, from: usize, to: usize) {
        if let Some(inv) = self.inventories.lock().get_mut(&id) {
            inv.move_stack(from, to);
        }
    }

    /// Apply one execution of `recipe` to player `id`: if they hold every input,
    /// consume the inputs and grant the outputs, spilling any overflow at their
    /// feet. Returns whether the recipe was applied (false if materials were
    /// insufficient).
    fn apply_recipe(&self, id: EntityId, recipe: &crate::recipe::Recipe) -> bool {
        self.apply_recipe_with(id, recipe, &[])
    }

    /// As [`apply_recipe`](Self::apply_recipe), but requires and consumes `extra`
    /// inputs (as `(item, count)` pairs) on top of the recipe's own — used by the
    /// forge to burn a separately-chosen fuel alongside the smelt. The recipe is
    /// applied only if the player holds both the recipe inputs and every `extra`.
    fn apply_recipe_with(
        &self,
        id: EntityId,
        recipe: &crate::recipe::Recipe,
        extra: &[(BlockId, u32)],
    ) -> bool {
        let overflow = {
            let mut invs = self.inventories.lock();
            let inv = invs.entry(id).or_default();
            if !recipe.craftable(inv) || extra.iter().any(|(item, n)| inv.count(*item) < *n) {
                return false;
            }
            for (item, n) in recipe.inputs {
                inv.remove(*item, *n);
            }
            for (item, n) in extra {
                inv.remove(*item, *n);
            }
            let mut overflow = Vec::new();
            for (item, n) in recipe.outputs {
                let left = inv.add(*item, *n);
                if left > 0 {
                    overflow.push((*item, left));
                }
            }
            overflow
        };
        // Spill anything that didn't fit at the crafter's location.
        if !overflow.is_empty() {
            let dim = self.dim_of(id);
            let cell = self
                .entities(dim)
                .lock()
                .get(id)
                .map(|e| ((e.x / TILE_SIZE) as i32, (e.y / TILE_SIZE) as i32));
            if let Some((cx, cy)) = cell {
                for (item, n) in overflow {
                    for _ in 0..n {
                        spawn_drop(self, dim, cx, cy, item);
                    }
                }
            }
        }
        true
    }

    /// Craft `RECIPES[recipe_idx]` once for player `id`. No-op for an unknown
    /// recipe or insufficient materials.
    fn craft(&self, id: EntityId, recipe_idx: usize) {
        if let Some(recipe) = crate::recipe::RECIPES.get(recipe_idx) {
            self.apply_recipe(id, recipe);
        }
    }

    /// Smelt `SMELT_RECIPES[recipe_idx]` up to `count` times for player `id`,
    /// burning `fuel` (a [`forge_fuel_units`](crate::block::forge_fuel_units) charge
    /// per smelt) and stopping as soon as the raw material or fuel runs out. No-op
    /// for an unknown recipe or an item that can't fuel a forge.
    fn smelt(&self, id: EntityId, recipe_idx: usize, count: u32, fuel: BlockId) {
        let Some(recipe) = crate::recipe::SMELT_RECIPES.get(recipe_idx) else {
            return;
        };
        let Some(units) = crate::block::forge_fuel_units(fuel) else {
            return;
        };
        for _ in 0..count {
            if !self.apply_recipe_with(id, recipe, &[(fuel, units)]) {
                break;
            }
        }
    }

    /// Repair one worn `item` tool for player `id`, restoring
    /// [`repair_step`](crate::block::repair_step) durability in exchange for one
    /// unit of its [`repair_material`](crate::block::repair_material). No-op if
    /// the item isn't repairable, the player holds no such damaged tool, or they
    /// lack the material.
    fn repair(&self, id: EntityId, item: BlockId) {
        let Some(material) = crate::block::repair_material(item) else {
            return;
        };
        let mut invs = self.inventories.lock();
        let inv = invs.entry(id).or_default();
        if inv.count(material) == 0 {
            return;
        }
        // Only spend material if a damaged tool was actually mended.
        if inv.repair_tool(item, crate::block::repair_step(item)) {
            inv.remove(material, 1);
        }
    }

    /// Spend `wear` durability on player `id`'s held `tool`, broadcasting nothing
    /// but pushing the owner a fresh inventory snapshot so the durability bar (and
    /// a now-broken tool's empty slot) updates. No-op when `wear` is zero.
    fn wear_tool(&self, id: EntityId, tool: BlockId, wear: u16) {
        if wear == 0 {
            return;
        }
        {
            let mut invs = self.inventories.lock();
            invs.entry(id).or_default().damage_tool(tool, wear);
        }
        self.send_inventory(id);
    }

    /// Push the authoritative inventory snapshot to its owner.
    fn send_inventory(&self, id: EntityId) {
        let slots = self
            .inventories
            .lock()
            .get(&id)
            .map(Inventory::to_slots)
            .unwrap_or_default();
        self.send_to(id, ServerMessage::Inventory { slots });
    }

    /// Push the authoritative mana value to its owner.
    fn send_mana(&self, id: EntityId) {
        let mana = self.mana.lock().get(&id).copied().unwrap_or(0);
        self.send_to(
            id,
            ServerMessage::Mana {
                mana,
                max: MANA_MAX,
            },
        );
    }

    /// Grant player `id` `amount` mana (capped at [`MANA_MAX`], floored at `0`) and
    /// resync it to them. A non-positive net no-op still resends so the client never
    /// drifts. Does nothing for an id with no mana entry (not a connected player).
    fn add_mana(&self, id: EntityId, amount: i32) {
        {
            let mut mana = self.mana.lock();
            let Some(m) = mana.get_mut(&id) else {
                return;
            };
            *m = (*m + amount).clamp(0, MANA_MAX);
        }
        self.send_mana(id);
    }

    /// Eat the food in player `id`'s inventory `slot`: consume one and adjust the
    /// player's health by its [`food_heal`](crate::block::food_heal). A positive
    /// amount heals (capped at the player's maximum); a negative one (raw meat)
    /// damages and can even kill. Returns the health/hit messages to broadcast and
    /// a respawn target if the bite proved fatal. No-op (empty) if the slot holds
    /// no food.
    fn eat(&self, id: EntityId, slot: usize) -> (Vec<ServerMessage>, Option<RespawnInfo>) {
        let Some(item) = self.peek_slot(id, slot) else {
            return (Vec::new(), None);
        };
        let Some(delta) = crate::block::food_heal(item) else {
            return (Vec::new(), None);
        };
        // Spend one serving before applying its effect.
        {
            let mut invs = self.inventories.lock();
            let Some(inv) = invs.get_mut(&id) else {
                return (Vec::new(), None);
            };
            if inv.take_one(slot).is_none() {
                return (Vec::new(), None);
            }
        }
        let dim = self.dim_of(id);
        let respawn = self.respawn_target(id);
        let mut entities = self.entities(dim).lock();
        if delta < 0 {
            // Raw meat: route through the damage path so a fatal bite respawns.
            apply_damage(&mut entities, id, -delta, (0.0, 0.0), dim, respawn)
        } else {
            let Some(e) = entities.get_mut(id) else {
                return (Vec::new(), None);
            };
            e.health = (e.health + delta).min(e.max_health);
            (
                vec![ServerMessage::EntityHealth {
                    id,
                    health: e.health,
                    max_health: e.max_health,
                }],
                None,
            )
        }
    }

    /// React to player `id` swinging at the pet (cat or puppy) `target` while
    /// holding `held`. Players can never *hurt* a pet (what a monster that would
    /// be), so a swing's only possible effect is to feed it: one [`COOKED_MEAT`]
    /// tames a *wild* pet — stamping `id` as its owner and topping it up to full
    /// health — at the cost of that serving. Anything else (a different held item,
    /// an out-of-reach swing, an already-tamed pet, or an empty pantry) is a
    /// harmless no-op. Locks inventories before entities, matching [`Self::eat`].
    fn try_feed_pet(&self, id: EntityId, target: EntityId, held: BlockId, dim: Dimension) {
        if held != crate::block::COOKED_MEAT {
            return;
        }
        let tamed = {
            let mut invs = self.inventories.lock();
            let mut entities = self.entities(dim).lock();
            // The target must still be a wild pet within the feeder's reach, and
            // the feeder must have a name to stamp (an unidentified player can't
            // tame, since the bond is stored by name). Capture that name here.
            let owner_name = match (entities.get(id), entities.get(target)) {
                (Some(a), Some(b))
                    if matches!(
                        b.kind,
                        EntityKind::Cat { owner: None, .. } | EntityKind::Puppy { owner: None, .. }
                    ) && aabb_gap(
                        a.x,
                        a.y,
                        a.size().0,
                        a.size().1,
                        b.x,
                        b.y,
                        b.size().0,
                        b.size().1,
                    ) <= PLAYER_ATTACK_REACH =>
                {
                    match &a.kind {
                        EntityKind::Player { name } if !name.is_empty() => name.clone(),
                        _ => return,
                    }
                }
                _ => return,
            };
            // Spend one cooked meat; bail (without taming) if they hold none.
            let Some(inv) = invs.get_mut(&id) else {
                return;
            };
            if inv.count(crate::block::COOKED_MEAT) == 0 {
                return;
            }
            inv.remove(crate::block::COOKED_MEAT, 1);
            // Stamp the feeder as owner and send the now-happy pet to full health,
            // preserving which kind of pet it is.
            entities.get_mut(target).map(|e| {
                match &mut e.kind {
                    EntityKind::Cat { owner, sitting } | EntityKind::Puppy { owner, sitting } => {
                        *owner = Some(owner_name);
                        *sitting = false;
                    }
                    _ => {}
                }
                e.health = e.max_health;
                e.clone()
            })
        };
        // Resync the pet's full (now owned) description and the player's pantry.
        if let Some(entity) = tamed {
            self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity });
        }
        self.send_inventory(id);
    }

    /// React to player `id` feeding the wild horse `target`: a horse is tamed not
    /// with cooked meat like the other pets but with an **apple**. Mirrors
    /// [`Self::try_feed_pet`] — the target must still be a wild horse within reach,
    /// the feeder must be named (the bond is stored by name) and hold an apple — and
    /// on success stamps the feeder as owner, heals the horse, spends the apple, and
    /// resyncs both the horse and the feeder's inventory.
    fn try_feed_horse(&self, id: EntityId, target: EntityId, held: BlockId, dim: Dimension) {
        if held != crate::block::APPLE {
            return;
        }
        let tamed = {
            let mut invs = self.inventories.lock();
            let mut entities = self.entities(dim).lock();
            let owner_name = match (entities.get(id), entities.get(target)) {
                (Some(a), Some(b))
                    if matches!(b.kind, EntityKind::Horse { owner: None })
                        && aabb_gap(
                            a.x,
                            a.y,
                            a.size().0,
                            a.size().1,
                            b.x,
                            b.y,
                            b.size().0,
                            b.size().1,
                        ) <= PLAYER_ATTACK_REACH =>
                {
                    match &a.kind {
                        EntityKind::Player { name } if !name.is_empty() => name.clone(),
                        _ => return,
                    }
                }
                _ => return,
            };
            // Spend one apple; bail (without taming) if they hold none.
            let Some(inv) = invs.get_mut(&id) else {
                return;
            };
            if inv.count(crate::block::APPLE) == 0 {
                return;
            }
            inv.remove(crate::block::APPLE, 1);
            entities.get_mut(target).map(|e| {
                if let EntityKind::Horse { owner } = &mut e.kind {
                    *owner = Some(owner_name);
                }
                e.health = e.max_health;
                e.clone()
            })
        };
        if let Some(entity) = tamed {
            self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity });
        }
        self.send_inventory(id);
    }

    /// React to player `id` offering held item `held` to the wild knight `target`: if
    /// it is a tungsten ingot and the player is in reach, spend one ingot and recruit
    /// the knight, stamping the giver's name into its `owner`. Mirrors
    /// [`Self::try_feed_horse`]. A no-op (no ingot spent) otherwise.
    fn try_recruit_knight(&self, id: EntityId, target: EntityId, held: BlockId, dim: Dimension) {
        if held != crate::block::TUNGSTEN_INGOT {
            return;
        }
        let recruited = {
            let mut invs = self.inventories.lock();
            let mut entities = self.entities(dim).lock();
            let owner_name = match (entities.get(id), entities.get(target)) {
                (Some(a), Some(b))
                    if matches!(b.kind, EntityKind::Knight { owner: None })
                        && aabb_gap(
                            a.x,
                            a.y,
                            a.size().0,
                            a.size().1,
                            b.x,
                            b.y,
                            b.size().0,
                            b.size().1,
                        ) <= PLAYER_ATTACK_REACH =>
                {
                    match &a.kind {
                        EntityKind::Player { name } if !name.is_empty() => name.clone(),
                        _ => return,
                    }
                }
                _ => return,
            };
            // Spend one tungsten ingot; bail (without recruiting) if they hold none.
            let Some(inv) = invs.get_mut(&id) else {
                return;
            };
            if inv.count(crate::block::TUNGSTEN_INGOT) == 0 {
                return;
            }
            inv.remove(crate::block::TUNGSTEN_INGOT, 1);
            entities.get_mut(target).map(|e| {
                if let EntityKind::Knight { owner } = &mut e.kind {
                    *owner = Some(owner_name);
                }
                e.health = e.max_health;
                e.clone()
            })
        };
        if let Some(entity) = recruited {
            // The serialized entity drops the runtime-only mount pose, so if this
            // knight was found already on horseback, restore it with the dedicated
            // riding message after the spawn resync.
            let mount = entity.riding;
            self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity });
            if let Some(horse) = mount {
                self.broadcast_dim(
                    dim,
                    ServerMessage::EntityRiding {
                        id: target,
                        horse: Some(horse),
                    },
                );
            }
        }
        self.send_inventory(id);
    }

    /// Recruit the wild musketeer `target` for player `id` by offering it a tungsten
    /// ingot, exactly as [`try_recruit_knight`](Self::try_recruit_knight) recruits a
    /// knight: the marksman must be wild and within reach, the player must hold a
    /// [`TUNGSTEN_INGOT`](crate::block::TUNGSTEN_INGOT) (spent on success), and the
    /// giver's name is stamped into the musketeer's `owner`. A no-op otherwise.
    fn try_recruit_musketeer(&self, id: EntityId, target: EntityId, held: BlockId, dim: Dimension) {
        if held != crate::block::TUNGSTEN_INGOT {
            return;
        }
        let recruited = {
            let mut invs = self.inventories.lock();
            let mut entities = self.entities(dim).lock();
            let owner_name = match (entities.get(id), entities.get(target)) {
                (Some(a), Some(b))
                    if matches!(b.kind, EntityKind::Musketeer { owner: None })
                        && aabb_gap(
                            a.x,
                            a.y,
                            a.size().0,
                            a.size().1,
                            b.x,
                            b.y,
                            b.size().0,
                            b.size().1,
                        ) <= PLAYER_ATTACK_REACH =>
                {
                    match &a.kind {
                        EntityKind::Player { name } if !name.is_empty() => name.clone(),
                        _ => return,
                    }
                }
                _ => return,
            };
            let Some(inv) = invs.get_mut(&id) else {
                return;
            };
            if inv.count(crate::block::TUNGSTEN_INGOT) == 0 {
                return;
            }
            inv.remove(crate::block::TUNGSTEN_INGOT, 1);
            entities.get_mut(target).map(|e| {
                if let EntityKind::Musketeer { owner } = &mut e.kind {
                    *owner = Some(owner_name);
                }
                e.health = e.max_health;
                e.clone()
            })
        };
        if let Some(entity) = recruited {
            self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity });
        }
        self.send_inventory(id);
    }

    /// React to player `id` offering held item `held` to the friendly farmer `target`:
    /// if it is an [`IRON_INGOT`](crate::block::IRON_INGOT) and the player is in reach,
    /// spend one ingot and trade back a windfall of food — either **eight**
    /// [apples](crate::block::APPLE) or **four** [cooked meat](crate::block::COOKED_MEAT),
    /// picked at random (the farmer hands over whatever it feels like parting with). A
    /// no-op (no ingot spent) if the target isn't a farmer in reach or the player holds
    /// no ingot. Anything that doesn't fit the player's inventory is left behind.
    fn try_trade_farmer(&self, id: EntityId, target: EntityId, held: BlockId, dim: Dimension) {
        if held != crate::block::IRON_INGOT {
            return;
        }
        let traded = {
            let mut invs = self.inventories.lock();
            let entities = self.entities(dim).lock();
            // The target must still be a farmer within the trader's reach.
            let in_reach = match (entities.get(id), entities.get(target)) {
                (Some(a), Some(b)) => {
                    b.kind.is_farmer()
                        && aabb_gap(
                            a.x,
                            a.y,
                            a.size().0,
                            a.size().1,
                            b.x,
                            b.y,
                            b.size().0,
                            b.size().1,
                        ) <= PLAYER_ATTACK_REACH
                }
                _ => false,
            };
            if !in_reach {
                return;
            }
            // Spend one iron ingot; bail (without trading) if they hold none.
            let Some(inv) = invs.get_mut(&id) else {
                return;
            };
            if inv.count(crate::block::IRON_INGOT) == 0 {
                return;
            }
            inv.remove(crate::block::IRON_INGOT, 1);
            // Pick the farmer's offer at random. Determinism doesn't matter here, only
            // cheap variety from trade to trade — so mix a freshly allocated id (which
            // marches on with every spawn) rather than keep a live rng handle.
            let mut r = self.alloc_id().wrapping_mul(2_654_435_761);
            r ^= r >> 15;
            let (item, count) = if r & 1 == 0 {
                (crate::block::APPLE, 8)
            } else {
                (crate::block::COOKED_MEAT, 4)
            };
            inv.add(item, count);
            true
        };
        if traded {
            self.send_inventory(id);
        }
    }

    /// React to player `id` clicking the *tamed* pet `target`: if `id` is the pet's
    /// owner and within reach, flip whether it's sitting (sit it down, or stand it
    /// back up). A sitting pet stays put — its tick stops wandering and stops
    /// follow-teleporting it — until it's stood up again. A click from anyone but
    /// the owner, or from out of reach, is a harmless no-op. The bond is stored by
    /// player *name* (see [`EntityKind::Cat`]), so ownership is checked by name.
    fn toggle_pet_sit(&self, id: EntityId, target: EntityId, dim: Dimension) {
        let updated = {
            let mut entities = self.entities(dim).lock();
            // Capture the clicker's name and reach box; an unnamed player can't own.
            let (name, ax, ay, aw, ah) = match entities.get(id) {
                Some(a) => match &a.kind {
                    EntityKind::Player { name } if !name.is_empty() => {
                        let (aw, ah) = a.size();
                        (name.clone(), a.x, a.y, aw, ah)
                    }
                    _ => return,
                },
                None => return,
            };
            match entities.get_mut(target) {
                Some(b) => {
                    let (bw, bh) = b.size();
                    let owns = matches!(
                        &b.kind,
                        EntityKind::Cat { owner: Some(o), .. }
                            | EntityKind::Puppy { owner: Some(o), .. } if *o == name
                    );
                    if !owns || aabb_gap(ax, ay, aw, ah, b.x, b.y, bw, bh) > PLAYER_ATTACK_REACH {
                        return;
                    }
                    if let EntityKind::Cat { sitting, .. } | EntityKind::Puppy { sitting, .. } =
                        &mut b.kind
                    {
                        *sitting = !*sitting;
                    }
                    // Settle it in place so it doesn't keep its last walk velocity.
                    b.vx = 0.0;
                    b.vy = 0.0;
                    b.clone()
                }
                None => return,
            }
        };
        // Resync the cat's full description (the flipped `sitting` flag rides along
        // in its `kind`) so every client renders the new pose and stance.
        self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity: updated });
    }

    /// Feed one unit of `fuel` to the campfire at cell `(x, y)` for player `id`:
    /// light it (if unlit) and extend its burn time. No-op unless the cell is a
    /// campfire, `fuel` is valid fuel, and the player holds some.
    fn fuel_campfire(&self, id: EntityId, x: i32, y: i32, fuel: BlockId) {
        let Some(secs) = crate::block::fuel_seconds(fuel) else {
            return;
        };
        let dim = self.dim_of(id);
        if !crate::block::is_campfire(self.world(dim).lock().get(x, y)) {
            return;
        }
        // Spend one unit of fuel.
        {
            let mut invs = self.inventories.lock();
            let inv = invs.entry(id).or_default();
            if inv.count(fuel) == 0 {
                return;
            }
            inv.remove(fuel, 1);
        }
        // Stoke the fire (extending an already-burning one).
        *self.campfires.lock().entry((dim, x, y)).or_insert(0.0) += secs;
        // Light it if it wasn't already, telling everyone its new lit look.
        let lit = {
            let mut world = self.world(dim).lock();
            world.get(x, y) == crate::block::CAMPFIRE && world.set(x, y, crate::block::CAMPFIRE_LIT)
        };
        if lit {
            self.broadcast_dim(
                dim,
                ServerMessage::BlockUpdate {
                    dim,
                    x,
                    y,
                    block: crate::block::CAMPFIRE_LIT,
                },
            );
        }
        self.send_inventory(id);
    }

    /// Whether player `id` (in dimension `dim`) is close enough to interact with
    /// world cell `(x, y)` — the same melee reach that governs mining and placement.
    fn cell_in_reach(&self, id: EntityId, dim: Dimension, x: i32, y: i32) -> bool {
        let entities = self.entities(dim).lock();
        entities.get(id).is_some_and(|p| {
            let (pw, ph) = p.size();
            aabb_gap(
                p.x,
                p.y,
                pw,
                ph,
                x as f32 * TILE_SIZE,
                y as f32 * TILE_SIZE,
                TILE_SIZE,
                TILE_SIZE,
            ) <= PLAYER_ATTACK_REACH
        })
    }

    /// Write `text` onto the sign or quest board at cell `(x, y)` for player `id`.
    /// No-op unless the cell really holds the matching block type and the player is
    /// within reach. The text is clamped to the line/row/note limits; a blank write
    /// clears the cell's stored text. The (sanitized) result is broadcast to
    /// everyone in the dimension so each client mirrors the change.
    fn write_block_text(&self, id: EntityId, x: i32, y: i32, text: crate::protocol::BlockText) {
        let dim = self.dim_of(id);
        let block = self.world(dim).lock().get(x, y);
        // The text kind has to suit the block actually standing there (a sign's note
        // on a sign, a board's notes on a quest board).
        if !text.matches_block(block) {
            return;
        }
        if !self.cell_in_reach(id, dim, x, y) {
            return;
        }
        let text = text.sanitized();
        if text.is_blank() {
            self.block_text.lock().remove(&(dim, x, y));
        } else {
            self.block_text.lock().insert((dim, x, y), text.clone());
        }
        self.broadcast_dim(dim, ServerMessage::BlockText { dim, x, y, text });
    }

    /// Whether player `id` has unlocked the locked chest at `(dim, x, y)` this
    /// session (entered its password, or reinforced it), so they may open or break it.
    fn chest_authed(&self, id: EntityId, dim: Dimension, x: i32, y: i32) -> bool {
        self.chest_auth
            .lock()
            .get(&id)
            .is_some_and(|s| s.contains(&(dim, x, y)))
    }

    /// Push the current contents of the chest at `(dim, x, y)` to every player who
    /// has it open. Called after any change so all viewers stay in sync.
    fn send_chest_contents(&self, dim: Dimension, x: i32, y: i32) {
        let Some(slots) = self.chest_store.lock().get(&(dim, x, y)).cloned() else {
            return;
        };
        let viewers: Vec<EntityId> = self
            .chest_viewers
            .lock()
            .iter()
            .filter(|(_, cell)| **cell == (dim, x, y))
            .map(|(vid, _)| *vid)
            .collect();
        for vid in viewers {
            self.send_to(
                vid,
                ServerMessage::ChestContents {
                    x,
                    y,
                    slots: slots.clone(),
                },
            );
        }
    }

    /// Open the chest at `(x, y)` for player `id`. A plain chest opens to anyone in
    /// reach; a locked chest needs a standing session unlock or the right
    /// `password`. On success the player becomes a viewer and is sent the contents;
    /// on a failed unlock they get [`ServerMessage::ChestLocked`] instead.
    fn open_chest(&self, id: EntityId, x: i32, y: i32, password: Option<String>) {
        let dim = self.dim_of(id);
        let block = self.world(dim).lock().get(x, y);
        if !crate::block::is_any_chest(block) || !self.cell_in_reach(id, dim, x, y) {
            return;
        }
        if crate::block::is_locked_chest(block) {
            let ok = self.chest_authed(id, dim, x, y) || {
                match (self.chest_locks.lock().get(&(dim, x, y)), &password) {
                    (Some(stored), Some(p)) => stored == p,
                    _ => false,
                }
            };
            if !ok {
                self.send_to(id, ServerMessage::ChestLocked { x, y });
                return;
            }
            self.chest_auth
                .lock()
                .entry(id)
                .or_default()
                .insert((dim, x, y));
        }
        let slots = self
            .chest_store
            .lock()
            .entry((dim, x, y))
            .or_insert_with(|| vec![None; CHEST_SLOTS])
            .clone();
        self.chest_viewers.lock().insert(id, (dim, x, y));
        self.send_to(id, ServerMessage::ChestContents { x, y, slots });
    }

    /// Player `id` stopped viewing whatever chest they had open.
    fn close_chest(&self, id: EntityId) {
        self.chest_viewers.lock().remove(&id);
    }

    /// Move a stack between player `id`'s inventory and/or the chest at `(x, y)`.
    /// Validates the player is viewing that chest, in reach, and (for a locked
    /// chest) unlocked, then resyncs both the player's inventory and every viewer.
    fn move_chest_item(&self, id: EntityId, x: i32, y: i32, from: SlotRef, to: SlotRef) {
        let dim = self.dim_of(id);
        if self.chest_viewers.lock().get(&id) != Some(&(dim, x, y)) {
            return;
        }
        let block = self.world(dim).lock().get(x, y);
        if !crate::block::is_any_chest(block) || !self.cell_in_reach(id, dim, x, y) {
            return;
        }
        if crate::block::is_locked_chest(block) && !self.chest_authed(id, dim, x, y) {
            return;
        }
        {
            let mut invs = self.inventories.lock();
            let inv = invs.entry(id).or_default();
            let mut store = self.chest_store.lock();
            let chest = store
                .entry((dim, x, y))
                .or_insert_with(|| vec![None; CHEST_SLOTS]);
            match (from, to) {
                (SlotRef::Player(a), SlotRef::Player(b)) => inv.move_stack(a as usize, b as usize),
                (SlotRef::Chest(a), SlotRef::Chest(b)) => {
                    move_within(chest, a as usize, b as usize)
                }
                (SlotRef::Player(a), SlotRef::Chest(b)) => {
                    move_between(inv.slots_mut(), a as usize, chest, b as usize)
                }
                (SlotRef::Chest(a), SlotRef::Player(b)) => {
                    move_between(chest, a as usize, inv.slots_mut(), b as usize)
                }
            }
        }
        self.send_inventory(id);
        self.send_chest_contents(dim, x, y);
    }

    /// Reinforce the plain chest at `(x, y)` into a locked chest sealed with
    /// `password` for player `id`. Requires a plain chest in reach and
    /// [`crate::block::LOCKED_CHEST_GOLD_COST`] gold ingots, which are consumed. The chest keeps
    /// its contents; the reinforcing player is left unlocked for it.
    fn reinforce_chest(&self, id: EntityId, x: i32, y: i32, password: String) {
        let dim = self.dim_of(id);
        if !crate::block::is_chest(self.world(dim).lock().get(x, y))
            || !self.cell_in_reach(id, dim, x, y)
        {
            return;
        }
        let password: String = password.chars().take(PASSWORD_MAX_LEN).collect();
        if password.trim().is_empty() {
            return;
        }
        {
            let mut invs = self.inventories.lock();
            let inv = invs.entry(id).or_default();
            if inv.count(crate::block::GOLD_INGOT) < crate::block::LOCKED_CHEST_GOLD_COST {
                return;
            }
            inv.remove(
                crate::block::GOLD_INGOT,
                crate::block::LOCKED_CHEST_GOLD_COST,
            );
        }
        self.world(dim).lock().set(x, y, crate::block::LOCKED_CHEST);
        self.chest_locks.lock().insert((dim, x, y), password);
        self.chest_auth
            .lock()
            .entry(id)
            .or_default()
            .insert((dim, x, y));
        self.broadcast_dim(
            dim,
            ServerMessage::BlockUpdate {
                dim,
                x,
                y,
                block: crate::block::LOCKED_CHEST,
            },
        );
        self.send_inventory(id);
    }

    /// Remove the chest at `(dim, x, y)` from the world's bookkeeping and spill its
    /// contents onto the ground as dropped items. Called when a chest block is
    /// broken (its block drop and removal are handled by the caller).
    fn spill_chest(&self, dim: Dimension, x: i32, y: i32) {
        let slots = self.chest_store.lock().remove(&(dim, x, y));
        self.chest_locks.lock().remove(&(dim, x, y));
        self.chest_viewers
            .lock()
            .retain(|_, &mut cell| cell != (dim, x, y));
        for set in self.chest_auth.lock().values_mut() {
            set.remove(&(dim, x, y));
        }
        if let Some(slots) = slots {
            for (block, count, dur) in slots.into_iter().flatten() {
                spawn_item(
                    self,
                    dim,
                    block,
                    count,
                    dur,
                    x as f32 * TILE_SIZE,
                    y as f32 * TILE_SIZE,
                    0.0,
                    ITEM_POP_VELOCITY,
                    ITEM_PICKUP_DELAY,
                );
            }
        }
    }

    /// Use the bucket in player `id`'s inventory `slot` on world cell `(x, y)`: an
    /// empty [`BUCKET`](crate::block::BUCKET) scoops up a [`WATER`](crate::block::WATER)
    /// cell (becoming a [`WATER_BUCKET`](crate::block::WATER_BUCKET)), and a water
    /// bucket pours its load into an empty cell (becoming empty again). The held
    /// item, the target cell, and inventory room are all validated; on any mismatch
    /// the cell and inventory are resynced so a client's optimistic guess is undone.
    fn use_bucket(&self, id: EntityId, x: i32, y: i32, slot: usize) {
        let held = self.peek_slot(id, slot);
        let dim = self.dim_of(id);
        // Read the target cell and whether it has an orthogonal neighbour to rest
        // against — the same support a normal block placement requires, so water
        // can't be poured into open midair.
        let (cell, supported) = {
            let mut world = self.world(dim).lock();
            let cell = world.get(x, y);
            let supported = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                .iter()
                .any(|(dx, dy)| world.get(x + dx, y + dy) != crate::block::AIR);
            (cell, supported)
        };
        let changed = match held {
            // Scoop: an empty bucket fills from a water cell.
            Some(crate::block::BUCKET) if crate::block::is_water(cell) => {
                self.take_from_slot(id, slot);
                self.world(dim).lock().set(x, y, crate::block::AIR);
                // Hand back a water bucket; if the inventory is somehow full, undo.
                if self.add_item(id, crate::block::WATER_BUCKET) {
                    Some(crate::block::AIR)
                } else {
                    self.world(dim).lock().set(x, y, crate::block::WATER);
                    self.add_item(id, crate::block::BUCKET);
                    None
                }
            }
            // Pour: a water bucket empties into an open, supported cell (never
            // into midair, mirroring normal block placement).
            Some(crate::block::WATER_BUCKET) if cell == crate::block::AIR && supported => {
                self.take_from_slot(id, slot);
                if self.world(dim).lock().place_water(x, y) {
                    // Return the now-empty bucket; if it can't fit, spill it.
                    if !self.add_item(id, crate::block::BUCKET) {
                        spawn_drop(self, dim, x, y, crate::block::BUCKET);
                    }
                    Some(crate::block::WATER)
                } else {
                    self.add_item(id, crate::block::WATER_BUCKET); // refund
                    None
                }
            }
            _ => None,
        };
        match changed {
            Some(block) => self.broadcast_dim(dim, ServerMessage::BlockUpdate { dim, x, y, block }),
            // Nothing happened: correct the client's optimistic cell guess.
            None => {
                let actual = self.world(dim).lock().get(x, y);
                self.send_to(
                    id,
                    ServerMessage::BlockUpdate {
                        dim,
                        x,
                        y,
                        block: actual,
                    },
                );
            }
        }
        self.send_inventory(id);
    }

    /// Record the campfire at cell `(x, y)` as player `id`'s respawn point. No-op
    /// unless that cell really holds a campfire.
    fn set_respawn(&self, id: EntityId, x: i32, y: i32) {
        let dim = self.dim_of(id);
        if !crate::block::is_campfire(self.world(dim).lock().get(x, y)) {
            return;
        }
        self.respawn_points.lock().insert(id, (dim, x, y));
    }

    /// Where player `id` should respawn: their last campfire (in whichever
    /// dimension they set it) if they've set one and it's still standing, otherwise
    /// the overworld [`spawn`](Self::spawn) (so a broken campfire sends them back to
    /// the surface). Returns the destination dimension and world-pixel position.
    fn respawn_target(&self, id: EntityId) -> (Dimension, f32, f32) {
        let Some((dim, cx, cy)) = self.respawn_points.lock().get(&id).copied() else {
            return (Dimension::Overworld, self.spawn.0, self.spawn.1);
        };
        if !crate::block::is_campfire(self.world(dim).lock().get(cx, cy)) {
            return (Dimension::Overworld, self.spawn.0, self.spawn.1);
        }
        // Centre the player's 11px-wide body in the campfire's tile; its top sits
        // a tile up so its feet rest on the ground the campfire stands on.
        let px = cx as f32 * TILE_SIZE + (TILE_SIZE - crate::entity::PLAYER_SIZE.0) / 2.0;
        let py = cy as f32 * TILE_SIZE;
        (dim, px, py)
    }

    /// Send player `id` the authoritative snapshot of their waypoints and current
    /// home (respawn) point, so the client can redraw its markers.
    fn send_waypoints(&self, id: EntityId) {
        let list = self.waypoints.lock().get(&id).cloned().unwrap_or_default();
        let (hdim, hx, hy) = self.respawn_target(id);
        self.send_to(
            id,
            ServerMessage::Waypoints {
                list,
                home: (hdim, hx, hy),
            },
        );
    }

    /// Record a personal waypoint for player `id`, then resync their list.
    fn add_waypoint(&self, id: EntityId, wp: Waypoint) {
        self.waypoints.lock().entry(id).or_default().push(wp);
        self.send_waypoints(id);
    }

    /// Drop player `id`'s waypoint nearest to world pixel `(x, y)`, then resync.
    /// No-op if they have none.
    fn remove_waypoint(&self, id: EntityId, x: f32, y: f32) {
        {
            let mut all = self.waypoints.lock();
            let Some(list) = all.get_mut(&id) else {
                return;
            };
            let nearest = list
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let da = (a.x - x).powi(2) + (a.y - y).powi(2);
                    let db = (b.x - x).powi(2) + (b.y - y).powi(2);
                    da.total_cmp(&db)
                })
                .map(|(i, _)| i);
            if let Some(i) = nearest {
                list.remove(i);
            }
        }
        self.send_waypoints(id);
    }

    /// Cook `COOK_RECIPES[recipe_idx]` up to `count` times for player `id` on the
    /// campfire at cell `(x, y)`, stopping when the inputs run out. No-op unless
    /// that campfire is lit.
    fn cook(&self, id: EntityId, x: i32, y: i32, recipe_idx: usize, count: u32) {
        let dim = self.dim_of(id);
        if self.world(dim).lock().get(x, y) != crate::block::CAMPFIRE_LIT {
            return;
        }
        if let Some(recipe) = crate::recipe::COOK_RECIPES.get(recipe_idx) {
            for _ in 0..count {
                if !self.apply_recipe(id, recipe) {
                    break;
                }
            }
        }
        self.send_inventory(id);
    }

    /// Flow spreading water one cell outward in dimension `dim` and return the
    /// resulting block updates to broadcast. Each freshly flooded cell becomes a
    /// [`WATER`] block.
    fn tick_water(&self, dim: Dimension) -> Vec<ServerMessage> {
        self.world(dim)
            .lock()
            .spread_water()
            .into_iter()
            .map(|(x, y)| ServerMessage::BlockUpdate {
                dim,
                x,
                y,
                block: crate::block::WATER,
            })
            .collect()
    }

    /// Advance every lit campfire (across all dimensions) by `dt`, dropping any
    /// whose underlying block is gone and extinguishing any whose fuel has run out
    /// (reverting the cell to an unlit campfire), broadcasting the change to that
    /// dimension's clients. Locks are taken one at a time so the world and campfire
    /// mutexes are never held together.
    fn tick_campfires(&self, dt: f32) {
        // Snapshot which campfires' blocks are gone (read world without holding the
        // campfires lock), then update the timers, then revert any that expired.
        let keys: Vec<(Dimension, i32, i32)> = self.campfires.lock().keys().copied().collect();
        let mut gone: HashSet<(Dimension, i32, i32)> = HashSet::new();
        for &(dim, x, y) in &keys {
            if !crate::block::is_campfire(self.world(dim).lock().get(x, y)) {
                gone.insert((dim, x, y));
            }
        }
        let mut expired = Vec::new();
        {
            let mut fires = self.campfires.lock();
            fires.retain(|key, secs| {
                if gone.contains(key) {
                    return false; // the campfire was mined or overwritten
                }
                *secs -= dt;
                if *secs <= 0.0 {
                    expired.push(*key);
                    false
                } else {
                    true
                }
            });
        }
        for (dim, x, y) in expired {
            let reverted = {
                let mut world = self.world(dim).lock();
                world.get(x, y) == crate::block::CAMPFIRE_LIT
                    && world.set(x, y, crate::block::CAMPFIRE)
            };
            if reverted {
                self.broadcast_dim(
                    dim,
                    ServerMessage::BlockUpdate {
                        dim,
                        x,
                        y,
                        block: crate::block::CAMPFIRE,
                    },
                );
            }
        }
    }

    /// Advance every charred-skeleton trail fire by `dt`, snuffing out (reverting to
    /// air) any whose brief life has run out and broadcasting the change. A cell
    /// that has since been overwritten is simply dropped from tracking.
    fn tick_trail_fires(&self, dt: f32) {
        let mut expired = Vec::new();
        {
            let mut fires = self.trail_fires.lock();
            fires.retain(|key, secs| {
                *secs -= dt;
                if *secs <= 0.0 {
                    expired.push(*key);
                    false
                } else {
                    true
                }
            });
        }
        for (dim, x, y) in expired {
            let cleared = {
                let mut world = self.world(dim).lock();
                world.get(x, y) == crate::block::FIRE && world.set(x, y, crate::block::AIR)
            };
            if cleared {
                self.broadcast_dim(
                    dim,
                    ServerMessage::BlockUpdate {
                        dim,
                        x,
                        y,
                        block: crate::block::AIR,
                    },
                );
            }
        }
    }

    /// Fell the rest of a tree: flood-fill the run of logs 4-connected to the
    /// (already-cleared) cell `(x, y)`, clearing each and returning their cells so
    /// the caller can drop them. Naturally-grown logs only — a player-placed log
    /// (tracked in [`Shared::placed_logs`]) is left standing and also blocks the
    /// spread, so building with logs never collapses an adjacent tree and felling
    /// a tree never eats a player's log structure.
    fn chop_connected_logs(&self, dim: Dimension, x: i32, y: i32) -> Vec<(i32, i32)> {
        let mut world = self.world(dim).lock();
        let placed = self.placed_logs.lock();
        let mut removed = Vec::new();
        let mut seen: HashSet<(i32, i32)> = HashSet::new();
        seen.insert((x, y)); // origin already cleared by the caller
        let mut stack: Vec<(i32, i32)> = [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .iter()
            .map(|(dx, dy)| (x + dx, y + dy))
            .collect();
        while let Some((cx, cy)) = stack.pop() {
            if !seen.insert((cx, cy)) {
                continue;
            }
            if world.get(cx, cy) != crate::block::LOG || placed.contains(&(dim, cx, cy)) {
                continue;
            }
            world.set(cx, cy, crate::block::AIR);
            removed.push((cx, cy));
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                stack.push((cx + dx, cy + dy));
            }
        }
        removed
    }

    /// Decay leaves left unsupported after the logs at `removed_logs` were
    /// cleared: any leaf within [`LEAF_SUPPORT_RANGE`] of a removed log that no
    /// longer has a log in range is broken. Returns each broken leaf's cell and
    /// its rolled drop (leaves still shed their items as they decay).
    fn decay_unsupported_leaves(
        &self,
        dim: Dimension,
        removed_logs: &[(i32, i32)],
    ) -> Vec<(i32, i32, Option<BlockId>)> {
        let r = LEAF_SUPPORT_RANGE;
        // Every leaf that *could* have lost its only support sits within range of
        // a removed log; gather that neighbourhood (deduped) as the candidates.
        let mut candidates: HashSet<(i32, i32)> = HashSet::new();
        for &(lx, ly) in removed_logs {
            for dy in -r..=r {
                for dx in -r..=r {
                    candidates.insert((lx + dx, ly + dy));
                }
            }
        }
        let mut world = self.world(dim).lock();
        let mut decayed = Vec::new();
        for (cx, cy) in candidates {
            if world.get(cx, cy) != crate::block::LEAVES || leaf_supported(&mut world, cx, cy) {
                continue;
            }
            world.set(cx, cy, crate::block::AIR);
            let drop = crate::block::mined_drop_rolled(crate::block::LEAVES, self.rand_unit());
            decayed.push((cx, cy, drop));
        }
        decayed
    }

    /// Unroll a rope ladder downward from `(x, y)`: fill that cell and each open
    /// cell directly beneath it with rope ladder, stopping at the first
    /// obstruction (the cave floor), the world's bottom, or after
    /// [`ROPE_LADDER_MAX_DROP`] cells (the rope running out). Returns the filled
    /// cells, top-first, so the caller can broadcast them. `(x, y)` is assumed to
    /// already be air and validated for support.
    fn roll_rope_ladder(&self, dim: Dimension, x: i32, y: i32) -> Vec<(i32, i32)> {
        let mut world = self.world(dim).lock();
        let mut filled = Vec::new();
        let mut ty = y;
        while (filled.len() as i32) < ROPE_LADDER_MAX_DROP {
            if !crate::world::in_bounds(x, ty) || world.get(x, ty) != crate::block::AIR {
                break;
            }
            if !world.set(x, ty, crate::block::ROPE_LADDER) {
                break;
            }
            filled.push((x, ty));
            ty += 1;
        }
        filled
    }

    /// Reel in a whole rope ladder: clear every rope ladder cell vertically
    /// connected to `(x, y)` (the run runs up and down from there), returning the
    /// cells cleared so the caller can broadcast them. `(x, y)` itself has already
    /// been broken by the caller; this gathers the rest of the dangling run, which
    /// collapses as a unit rather than leaving floating rungs (and so a multi-cell
    /// run yields a single dropped rope ladder, not one per cell).
    fn collapse_rope_ladder(&self, dim: Dimension, x: i32, y: i32) -> Vec<(i32, i32)> {
        let mut world = self.world(dim).lock();
        let mut cleared = Vec::new();
        for dir in [-1, 1] {
            let mut ty = y + dir;
            while crate::world::in_bounds(x, ty)
                && world.get(x, ty) == crate::block::ROPE_LADDER
                && world.set(x, ty, crate::block::AIR)
            {
                cleared.push((x, ty));
                ty += dir;
            }
        }
        cleared
    }

    /// Swing the door touching cell `(x, y)` open or shut. A door spans two cells:
    /// a lower half and the upper half directly above it. Whichever half `(x, y)`
    /// names, this flips both between their closed
    /// ([`DOOR`](crate::block::DOOR)/[`DOOR_TOP`](crate::block::DOOR_TOP)) and open
    /// ([`DOOR_OPEN`](crate::block::DOOR_OPEN)/[`DOOR_OPEN_TOP`](crate::block::DOOR_OPEN_TOP))
    /// states. Returns the changed cells as `(x, y, block)` so the caller can
    /// broadcast them, or empty if `(x, y)` is not a door.
    fn toggle_door(&self, dim: Dimension, x: i32, y: i32) -> Vec<(i32, i32, BlockId)> {
        let mut world = self.world(dim).lock();
        let here = world.get(x, y);
        if !crate::block::is_door(here) {
            return Vec::new();
        }
        // Anchor on the lower half: if `(x, y)` is a top, the lower half is below.
        let by = if crate::block::is_door_bottom(here) {
            y
        } else {
            y + 1
        };
        let ty = by - 1; // the upper half sits directly above the lower half
        // Closed exactly when the lower cell is the solid `DOOR`; otherwise open.
        let opening = world.get(x, by) == crate::block::DOOR;
        let (lower, upper) = if opening {
            (crate::block::DOOR_OPEN, crate::block::DOOR_OPEN_TOP)
        } else {
            (crate::block::DOOR, crate::block::DOOR_TOP)
        };
        let mut changed = Vec::new();
        if world.set(x, by, lower) {
            changed.push((x, by, lower));
        }
        if world.set(x, ty, upper) {
            changed.push((x, ty, upper));
        }
        changed
    }
}

/// Number of overworld rows (counting up from the very bottom) that the descent
/// into the underworld treats as the world's floor: a player who falls past this
/// drops through into the underworld below.
const OVERWORLD_BOTTOM_ROWS: i32 = 1;

impl Shared {
    /// Move player `id` into dimension `to` at world pixel `(x, y)`: lift its
    /// avatar out of its current dimension's entity collection, re-home it in the
    /// destination, fix up the dimension bookkeeping, and tell every affected
    /// client. The owning client clears its mirrored world and re-streams the new
    /// dimension's chunks itself on receiving [`ServerMessage::EnterDimension`].
    fn enter_dimension(&self, id: EntityId, to: Dimension, x: f32, y: f32) {
        let from = self.dim_of(id);
        let mut player = match self.entities(from).lock().remove(id) {
            Some(e) => e,
            None => return, // not a live player
        };
        // Their name (used below to bring along any knights they've recruited).
        let player_name = match &player.kind {
            EntityKind::Player { name } if !name.is_empty() => Some(name.clone()),
            _ => None,
        };
        player.x = x;
        player.y = y;
        player.vx = 0.0;
        player.vy = 0.0;
        // Arriving by fire key or by falling between worlds always lands on foot,
        // never in a boat or on a horse — clear both rider states so the new
        // dimension draws them dismounted (the client clears its local state to
        // match, and the horse stays behind in the dimension they left).
        player.boating = false;
        player.riding = None;
        // A steed stays behind in the dimension you left, so any telepathic pilot link
        // snaps too — the freed dragon resumes its own AI back there.
        player.controlling = None;
        // Record the new dimension before announcing, so dimension-scoped
        // broadcasts route correctly.
        self.client_dim.lock().insert(id, to);
        // Any chest they had open belongs to the dimension they're leaving; stop
        // viewing it so its updates don't follow them across.
        self.chest_viewers.lock().remove(&id);
        // The avatar vanishes from the dimension it left...
        self.broadcast_dim_except(from, id, ServerMessage::EntityDespawn { id });
        // ...and appears in the one it entered.
        self.entities(to).lock().insert(player.clone());
        self.broadcast_dim_except(to, id, ServerMessage::EntitySpawn { entity: player });
        // Switch the owning client over: it clears its world/entities and begins
        // requesting the new dimension's chunks.
        self.send_to(id, ServerMessage::EnterDimension { dim: to, x, y });
        // Stream it the entities already present in the new dimension.
        let snapshot: Vec<Entity> = self
            .entities(to)
            .lock()
            .values()
            .filter(|e| e.id != id)
            .cloned()
            .collect();
        for entity in snapshot {
            // The serialized entity doesn't carry the (runtime-only) boat/horse pose,
            // so follow up for anyone already afloat or mounted to draw their boat or
            // ride them on their horse from the start.
            let afloat = entity.boating.then_some(entity.id);
            let mount = entity.riding.map(|h| (entity.id, h));
            self.send_to(id, ServerMessage::EntitySpawn { entity });
            if let Some(eid) = afloat {
                self.send_to(id, ServerMessage::EntityBoating { id: eid, on: true });
            }
            if let Some((rider, horse)) = mount {
                self.send_to(
                    id,
                    ServerMessage::EntityRiding {
                        id: rider,
                        horse: Some(horse),
                    },
                );
            }
        }
        // Resync the home marker (its dimension may differ from the new one) and
        // the inventory, which travels with the player across dimensions.
        self.send_waypoints(id);
        self.send_inventory(id);
        // A recruited knight or mage follows its owner across the divide: move every
        // companion this player owns from the old dimension into the new one, beside them.
        self.transfer_knights(player_name.as_deref(), from, to, x, y);
    }

    /// Carry player `owner`'s recruited companions — [knights](EntityKind::Knight),
    /// [mages](EntityKind::Mage) and a [white-dragon steed](EntityKind::WhiteDragon) —
    /// from dimension `from` to `to`, landing them next to the player at `(x, y)` on foot
    /// (any mount is left behind, like the player's own horse — a ridden steed crosses
    /// over unridden). Broadcasts the despawn/spawn so both dimensions' onlookers see
    /// them leave and arrive. A no-op if `owner` is `None` (an unnamed player owns nothing).
    fn transfer_knights(
        &self,
        owner: Option<&str>,
        from: Dimension,
        to: Dimension,
        x: f32,
        y: f32,
    ) {
        let Some(owner) = owner else { return };
        // Pull the owner's recruited companions — knights and mages alike — out of the
        // old dimension first, so they cross over with their owner.
        let mut moving: Vec<Entity> = {
            let mut src = self.entities(from).lock();
            let ids: Vec<EntityId> = src
                .values()
                .filter(|e| {
                    matches!(&e.kind, EntityKind::Knight { owner: Some(o) } if o == owner)
                        || matches!(&e.kind, EntityKind::Musketeer { owner: Some(o) } if o == owner)
                        || matches!(&e.kind, EntityKind::Mage { owner: Some(o) } if o == owner)
                        || matches!(&e.kind, EntityKind::WhiteDragon { owner: Some(o) } if o == owner)
                })
                .map(|e| e.id)
                .collect();
            ids.into_iter().filter_map(|kid| src.remove(kid)).collect()
        };
        if moving.is_empty() {
            return;
        }
        for k in &moving {
            self.broadcast_dim(from, ServerMessage::EntityDespawn { id: k.id });
        }
        // Re-home them beside the player in the new dimension, dismounted.
        {
            let mut dst = self.entities(to).lock();
            for k in &mut moving {
                k.x = x;
                k.y = y;
                k.vx = 0.0;
                k.vy = 0.0;
                k.home_x = Some(x);
                k.riding = None;
                k.mount_health = 0;
                dst.insert(k.clone());
            }
        }
        for k in moving {
            self.broadcast_dim(to, ServerMessage::EntitySpawn { entity: k });
        }
    }

    /// Use the fire key in hotbar `slot`: if it really holds a
    /// [`fire_key`](crate::block::FIRE_KEY), warp player `id` to the *other*
    /// dimension. The key remembers where it was last fired from in each dimension
    /// (see [`Shared::fire_marks`]), so it drops the player back at the spot they
    /// last keyed out from on the far side — the two anchors act like a linked pair
    /// of portals. The very first crossing into a dimension (no anchor yet) falls
    /// back to a fresh landing spot via [`default_landing`](Self::default_landing).
    /// The key is a reusable artifact — it is not consumed. A no-op (resyncing the
    /// client's inventory) if the slot no longer holds the key, e.g. after an
    /// inventory move raced with the use.
    fn use_fire_key(&self, id: EntityId, slot: usize) {
        if self.peek_slot(id, slot) != Some(crate::block::FIRE_KEY) {
            self.send_inventory(id);
            return;
        }
        let from = self.dim_of(id);
        let to = match from {
            Dimension::Overworld => Dimension::Underworld,
            Dimension::Underworld => Dimension::Overworld,
            // The fire key isn't the way into the arena, but if one is used there it
            // simply surfaces the bearer back in the overworld.
            Dimension::Arena => Dimension::Overworld,
        };
        // Where the player is standing now — recorded as this dimension's anchor so
        // a later key from the far side returns them to exactly here.
        let (x, y) = match self.entities(from).lock().get(id) {
            Some(p) => (p.x, p.y),
            None => return,
        };
        let dest = {
            let mut marks = self.fire_marks.lock();
            let anchors = marks.entry(id).or_default();
            anchors[from.index()] = Some((x, y));
            anchors[to.index()]
        };
        let (nx, ny) = match dest {
            // Return to where the player last fired the key on the far side.
            Some(pos) => pos,
            // Never keyed out of there before: pick a fresh spot in their column.
            None => self.default_landing(to, x),
        };
        self.enter_dimension(id, to, nx, ny);
    }

    /// A fresh landing position in dimension `to` for a player crossing from world
    /// x-pixel `x`, used when the fire key has no remembered anchor there yet.
    /// Entering the underworld drops onto its charred floor in the same column;
    /// surfacing in the overworld carves an air pocket at its floor to stand in.
    fn default_landing(&self, to: Dimension, x: f32) -> (f32, f32) {
        let cell_x = (x / TILE_SIZE).floor() as i32;
        match to {
            Dimension::Underworld => {
                let surface = self.world(Dimension::Underworld).lock().surface(cell_x);
                (x, ((surface - 3).max(0) as f32) * TILE_SIZE)
            }
            Dimension::Overworld => self.carve_overworld_landing(cell_x),
            // The arena is a fixed hall-and-room, so every challenger lands at the same
            // spot — just inside the mouth of the entrance hall — and walks in toward
            // the room, rather than dropping into their old overworld column.
            Dimension::Arena => arena_player_spawn(),
        }
    }

    /// Use the [arena key](crate::block::ARENA_KEY) held in hotbar `slot`: warp
    /// player `id` into the [`Dimension::Arena`], or — if they are already in the
    /// arena — back to wherever they last entered from (the overworld, by default).
    /// The key is a reusable artifact, never consumed. A no-op (resyncing the
    /// client's inventory) if the slot no longer holds the key, e.g. after an
    /// inventory move raced with the use.
    fn use_arena_key(&self, id: EntityId, slot: usize) {
        if self.peek_slot(id, slot) != Some(crate::block::ARENA_KEY) {
            self.send_inventory(id);
            return;
        }
        let from = self.dim_of(id);
        let (x, y) = match self.entities(from).lock().get(id) {
            Some(p) => (p.x, p.y),
            None => return,
        };
        if from == Dimension::Arena {
            // Leaving: return to the recorded entry point, falling back to a fresh
            // overworld landing if we somehow have none (e.g. arrived another way).
            let dest = self.arena_return.lock().remove(&id);
            let (to, nx, ny) = match dest {
                Some((d, rx, ry)) => (d, rx, ry),
                None => {
                    let (lx, ly) = self.default_landing(Dimension::Overworld, x);
                    (Dimension::Overworld, lx, ly)
                }
            };
            self.enter_dimension(id, to, nx, ny);
        } else {
            // Entering: remember exactly where we left from so the key brings us
            // back here, then drop onto the arena floor in the same column.
            self.arena_return.lock().insert(id, (from, x, y));
            let (nx, ny) = self.default_landing(Dimension::Arena, x);
            self.enter_dimension(id, Dimension::Arena, nx, ny);
        }
    }

    /// Cast the spellbook held in hotbar `slot` toward world pixel `(tx, ty)`. The
    /// book must really be a [spellbook](crate::block::is_spellbook) and the caster
    /// must hold at least its [mana cost](crate::block::spell_mana_cost); on success
    /// the mana is spent and the spell's effect is loosed (dispatched by which book
    /// it is). The book is never consumed. A no-op (resyncing the client's
    /// mana/inventory) if the slot doesn't hold a spellbook or mana is short — e.g.
    /// after an inventory move raced the cast.
    fn cast_spell(&self, id: EntityId, slot: usize, tx: f32, ty: f32) {
        let Some(item) = self
            .peek_slot(id, slot)
            .filter(|i| crate::block::is_spellbook(*i))
        else {
            self.send_inventory(id);
            return;
        };
        let Some(cost) = crate::block::spell_mana_cost(item) else {
            return;
        };
        // Charge the mana up front; bail (resyncing) if the caster can't afford it.
        {
            let mut mana = self.mana.lock();
            let Some(m) = mana.get_mut(&id) else {
                return;
            };
            if *m < cost {
                drop(mana);
                self.send_mana(id);
                return;
            }
            *m -= cost;
        }
        self.send_mana(id);

        let dim = self.dim_of(id);
        // Resolve the caster's name so a restored knight/mage can be recruited to them.
        let caster = self
            .entities(dim)
            .lock()
            .get(id)
            .and_then(|e| match &e.kind {
                EntityKind::Player { name } if !name.is_empty() => Some(name.clone()),
                _ => None,
            });
        let did = match item {
            crate::block::SUMMONER_SPELL => {
                self.cast_summoner(id, dim, tx, ty);
                true
            }
            crate::block::SUNBURST_SPELL => {
                self.cast_sunburst(id, dim);
                true
            }
            crate::block::RESTORE_SPELL => self.cast_restore(dim, tx, ty, caster),
            crate::block::DRAGONIAN_STEED => {
                self.cast_dragonian_steed(id, dim, caster);
                true
            }
            crate::block::DRAGON_PLATE_SPELL => {
                self.cast_dragon_plate(id, dim);
                true
            }
            _ => false,
        };
        // The restore spell does nothing if the cursor isn't on a restorable creature;
        // refund its mana rather than burning it on a miss (the area/projectile spells
        // always "happen", so they keep their cost).
        if !did {
            self.add_mana(id, cost);
        }
    }

    /// Cast the restore spell at world pixel `(tx, ty)`: find the nearest
    /// [restorable](is_restorable) creature to the cursor (within
    /// [`RESTORE_RANGE`]) and turn it (see [`restored_kind`]). A restored knight/mage
    /// is recruited to `owner` (the caster's name) — `None` leaves it wild. Returns
    /// whether anything was actually restored, so the caller can refund a miss.
    fn cast_restore(&self, dim: Dimension, tx: f32, ty: f32, owner: Option<String>) -> bool {
        // Pick the restorable creature whose center is nearest the cursor, in range.
        let target = {
            let entities = self.entities(dim).lock();
            entities
                .values()
                .filter(|e| is_restorable(&e.kind))
                .map(|e| {
                    let (w, h) = e.size();
                    let cx = e.x + w * 0.5;
                    let cy = e.y + h * 0.5;
                    (e.id, (cx - tx).powi(2) + (cy - ty).powi(2))
                })
                .filter(|&(_, d2)| d2 <= RESTORE_RANGE * RESTORE_RANGE)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(id, _)| id)
        };
        let Some(tid) = target else {
            return false;
        };
        // Re-read the kind under the lock and compute its restored form.
        let new_kind = {
            let entities = self.entities(dim).lock();
            entities
                .get(tid)
                .and_then(|e| restored_kind(&e.kind, &owner))
        };
        let Some(new_kind) = new_kind else {
            return false;
        };
        let msgs = {
            let mut entities = self.entities(dim).lock();
            transform_entity(&mut entities, tid, new_kind)
        };
        if msgs.is_empty() {
            return false;
        }
        for m in msgs {
            self.broadcast_dim(dim, m);
        }
        true
    }

    /// Loose the summoner spell: a friendly summoner fireball from the caster's upper
    /// body toward the cursor `(tx, ty)`, which bursts into a monster-hunting
    /// [`EntityKind::FriendlySkull`] (see the tick loop). Mana is already charged.
    fn cast_summoner(&self, id: EntityId, dim: Dimension, tx: f32, ty: f32) {
        let (fw, fh) = crate::entity::FRIENDLY_SUMMONER_FIREBALL_SIZE;
        let bolt = {
            let entities = self.entities(dim).lock();
            let Some(p) = entities.get(id) else {
                return;
            };
            let (pw, ph) = p.size();
            let sx = p.x + pw * 0.5 - fw * 0.5;
            let sy = p.y + ph * 0.3 - fh * 0.5;
            let dx = tx - (sx + fw * 0.5);
            let dy = ty - (sy + fh * 0.5);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let mut bolt = Entity::new(
                self.alloc_id(),
                EntityKind::FriendlySummonerFireball,
                sx,
                sy,
            );
            bolt.vx = dx / len * SUMMONER_FIREBALL_SPEED;
            bolt.vy = dy / len * SUMMONER_FIREBALL_SPEED;
            bolt.attack_cd = SUMMONER_FIREBALL_LIFETIME; // reused as the airborne life timer
            bolt
        };
        self.entities(dim).lock().insert(bolt.clone());
        self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity: bolt });
    }

    /// Summon the dragonian steed: a friendly white [`EntityKind::WhiteDragon`] at the
    /// caster's side, owned by `owner` (the caster's name, so it follows them and
    /// survives a restart). Any white dragon this player already owns — anywhere in the
    /// world — is dismissed first, so the spell never accumulates steeds: a second cast
    /// raises a fresh one beside the caster (the only way a fallen steed returns). Mana
    /// is already charged; an unnamed caster (no name) can't be bonded to, so this is a
    /// no-op for them (their mana is refunded by the caller, since `did` stays false —
    /// but in practice every connected player is named).
    fn cast_dragonian_steed(&self, id: EntityId, dim: Dimension, owner: Option<String>) {
        let Some(owner) = owner else { return };
        // Dismiss any existing steed this caster owns, in every dimension, so a recast
        // replaces rather than stacks. Broadcast each despawn to its dimension's
        // onlookers.
        for d in Dimension::ALL {
            let stale: Vec<EntityId> = {
                let entities = self.entities(d).lock();
                entities
                    .values()
                    .filter(|e| {
                        matches!(&e.kind, EntityKind::WhiteDragon { owner: Some(o) } if *o == owner)
                    })
                    .map(|e| e.id)
                    .collect()
            };
            for sid in stale {
                self.entities(d).lock().remove(sid);
                self.broadcast_dim(d, ServerMessage::EntityDespawn { id: sid });
            }
        }
        // Raise the new steed beside the caster, hovering just above them.
        let steed = {
            let entities = self.entities(dim).lock();
            let Some(p) = entities.get(id) else {
                return;
            };
            let (pw, _) = p.size();
            let (dw, dh) = crate::entity::WHITE_DRAGON_SIZE;
            let sx = p.x + pw * 0.5 - dw * 0.5;
            let sy = p.y - dh - 4.0;
            let mut steed = Entity::new(
                self.alloc_id(),
                EntityKind::WhiteDragon { owner: Some(owner) },
                sx,
                sy,
            );
            steed.home_x = Some(sx);
            steed
        };
        self.entities(dim).lock().insert(steed.clone());
        self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity: steed });
    }

    /// Cast the dragon plate spell: wreath player `id` in a scaled ward, raising their
    /// defense to [`crate::block::DRAGON_PLATE_DEFENSE`] for
    /// [`crate::block::DRAGON_PLATE_BUFF_DURATION`] seconds (overriding any worn armor;
    /// see the damage step in [`step_entities`]). Recasting simply refreshes the timer.
    /// The ward is server-only state, so there is nothing to broadcast.
    fn cast_dragon_plate(&self, id: EntityId, dim: Dimension) {
        if let Some(p) = self.entities(dim).lock().get_mut(id) {
            p.dragon_plate_timer = crate::block::DRAGON_PLATE_BUFF_DURATION;
        }
    }

    /// Spit a fireball from player `id` toward world pixel `(tx, ty)` (the cursor) —
    /// the offensive half of the [dragon plate](crate::block::DRAGON_PLATE_SPELL) ward.
    /// A no-op unless the player's ward is active and their breath cadence (the player's
    /// `attack_cd`, counted down in [`step_entities`]) is ready; the client cannot know
    /// the ward state, so it always asks and the server gates here. On success it looses
    /// a [`EntityKind::FriendlyDragonFireball`] — the same scorching bolt a white-dragon
    /// steed breathes, harming monsters but never the player — costing no mana.
    fn empty_hand_breath(&self, id: EntityId, tx: f32, ty: f32) {
        let dim = self.dim_of(id);
        let (fw, fh) = crate::entity::FRIENDLY_DRAGON_FIREBALL_SIZE;
        let bolt = {
            let mut entities = self.entities(dim).lock();
            let Some(p) = entities.get_mut(id) else {
                return;
            };
            if p.dragon_plate_timer <= 0.0 || p.attack_cd > 0.0 {
                return;
            }
            p.attack_cd = DRAGON_PLATE_BREATH_INTERVAL;
            let (pw, ph) = p.size();
            let sx = p.x + pw * 0.5 - fw * 0.5;
            let sy = p.y + ph * 0.3 - fh * 0.5;
            let dx = tx - (sx + fw * 0.5);
            let dy = ty - (sy + fh * 0.5);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let mut bolt = Entity::new(self.alloc_id(), EntityKind::FriendlyDragonFireball, sx, sy);
            bolt.vx = dx / len * FIREBALL_SPEED;
            bolt.vy = dy / len * FIREBALL_SPEED;
            bolt.attack_cd = FRIENDLY_DRAGON_FIREBALL_LIFETIME; // reused as the airborne timer
            bolt
        };
        self.entities(dim).lock().insert(bolt.clone());
        self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity: bolt });
    }

    /// Breathe a fireball from player `id`'s white-dragon steed toward world pixel
    /// `(tx, ty)` (the player's cursor). Works whether the player is *riding* the steed
    /// or *remotely piloting* it (see [`ClientMessage::SetControlling`](crate::protocol::ClientMessage::SetControlling))
    /// — a no-op unless one of those holds, the steed is their own
    /// [white dragon](EntityKind::WhiteDragon), and its breath (its `attack_cd`) is off
    /// cooldown. On success it looses a friendly dragon fireball from the steed's maw,
    /// resets the steed's breath cadence, and kicks off its fire-breathing animation —
    /// costing no mana, only the steed's own cooldown.
    fn dragon_breath(&self, id: EntityId, tx: f32, ty: f32) {
        let dim = self.dim_of(id);
        // Resolve which steed the player commands — the one they ride, or failing that the
        // one they pilot telepathically — confirm it is their own white dragon, and that
        // its breath (its `attack_cd`) is ready. Stamp the cooldown and the breath pose
        // while we hold the lock, and read its maw.
        let result = {
            let mut entities = self.entities(dim).lock();
            let Some((steed_id, name)) = entities.get(id).and_then(|p| match &p.kind {
                EntityKind::Player { name } => {
                    p.riding.or(p.controlling).map(|r| (r, name.clone()))
                }
                _ => None,
            }) else {
                return;
            };
            let Some(steed) = entities.get_mut(steed_id) else {
                return;
            };
            if !matches!(&steed.kind, EntityKind::WhiteDragon { owner: Some(o) } if *o == name)
                || steed.attack_cd > 0.0
            {
                return;
            }
            steed.attack_cd = WHITE_DRAGON_BREATH_INTERVAL;
            steed.lunge = crate::entity::DRAGON_ATTACK_TIME;
            let (w, h) = steed.size();
            let (fw, fh) = crate::entity::FRIENDLY_DRAGON_FIREBALL_SIZE;
            let sx = steed.x + w * 0.5 - fw * 0.5;
            let sy = steed.y + h * 0.3 - fh * 0.5;
            let dx = tx - (sx + fw * 0.5);
            let dy = ty - (sy + fh * 0.5);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let mut bolt = Entity::new(self.alloc_id(), EntityKind::FriendlyDragonFireball, sx, sy);
            bolt.vx = dx / len * FIREBALL_SPEED;
            bolt.vy = dy / len * FIREBALL_SPEED;
            bolt.attack_cd = FRIENDLY_DRAGON_FIREBALL_LIFETIME; // reused as the airborne timer
            entities.insert(bolt.clone());
            (steed_id, bolt)
        };
        let (steed_id, bolt) = result;
        // Announce the steed's breath pose and the new bolt to every onlooker.
        self.broadcast_dim(dim, ServerMessage::EntityLunging { id: steed_id });
        self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity: bolt });
    }

    /// Fire the musket held in hotbar `slot` toward world pixel `(tx, ty)`: spend one
    /// [`bullet`](crate::block::BULLET) and loose a [`EntityKind::FriendlyBullet`] from
    /// the player's body at the cursor (it damages the monster it strikes; see the tick
    /// loop). A no-op (resyncing the client's inventory) if the slot doesn't hold a
    /// [`musket`](crate::block::is_musket) or the player is out of bullets. The musket is
    /// reusable — firing consumes a bullet, never the gun.
    fn fire_musket(&self, id: EntityId, slot: usize, tx: f32, ty: f32) {
        if !self
            .peek_slot(id, slot)
            .is_some_and(crate::block::is_musket)
        {
            self.send_inventory(id);
            return;
        }
        // Enforce the slow reload server-side: reject a shot that arrives before the
        // musket is ready, so a fast-firing client can't bypass the cooldown. On a
        // good shot, stamp the next-ready instant. (`Instant::now` is monotonic.)
        {
            let now = Instant::now();
            let mut cds = self.musket_cd.lock();
            if cds.get(&id).is_some_and(|&ready| now < ready) {
                drop(cds);
                self.send_inventory(id);
                return;
            }
            cds.insert(id, now + MUSKET_COOLDOWN);
        }
        // Spend one bullet up front; bail (resyncing) if the shooter carries none.
        {
            let mut invs = self.inventories.lock();
            let Some(inv) = invs.get_mut(&id) else {
                return;
            };
            if inv.count(crate::block::BULLET) == 0 {
                drop(invs);
                self.send_inventory(id);
                return;
            }
            inv.remove(crate::block::BULLET, 1);
        }
        self.send_inventory(id);

        let dim = self.dim_of(id);
        let (bw, bh) = crate::entity::BULLET_SIZE;
        let bullet = {
            let entities = self.entities(dim).lock();
            let Some(p) = entities.get(id) else {
                return;
            };
            let (pw, ph) = p.size();
            let sx = p.x + pw * 0.5 - bw * 0.5;
            let sy = p.y + ph * 0.3 - bh * 0.5;
            let dx = tx - (sx + bw * 0.5);
            let dy = ty - (sy + bh * 0.5);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            let mut bullet = Entity::new(self.alloc_id(), EntityKind::FriendlyBullet, sx, sy);
            bullet.vx = dx / len * BULLET_SPEED;
            bullet.vy = dy / len * BULLET_SPEED;
            bullet.attack_cd = BULLET_LIFETIME; // reused as the airborne life timer
            bullet
        };
        self.entities(dim).lock().insert(bullet.clone());
        self.broadcast_dim(dim, ServerMessage::EntitySpawn { entity: bullet });
    }

    /// Loose the sunburst spell: a burst of sunlight centered on the caster that
    /// **instantly slays every creature that [burns in daylight](burns_in_daylight)**
    /// — zombies, skeletons, dark knights, necromancers and their skulls — within
    /// [`SUNBURST_RADIUS`]. It works at any hour and in any dimension (the burst
    /// supplies its own sunlight), but spares the caster's own friendly summons and
    /// everything that doesn't fear the sun. Mana is already charged.
    fn cast_sunburst(&self, id: EntityId, dim: Dimension) {
        // The caster's center, to measure the blast radius from.
        let Some((pcx, pcy)) = ({
            let entities = self.entities(dim).lock();
            entities.get(id).map(|p| {
                let (pw, ph) = p.size();
                (p.x + pw * 0.5, p.y + ph * 0.5)
            })
        }) else {
            return;
        };
        let r2 = SUNBURST_RADIUS * SUNBURST_RADIUS;
        let targets: Vec<EntityId> = {
            let entities = self.entities(dim).lock();
            entities
                .values()
                .filter(|e| burns_in_daylight(&e.kind))
                .filter(|e| {
                    let (w, h) = e.size();
                    let ex = e.x + w * 0.5;
                    let ey = e.y + h * 0.5;
                    (ex - pcx).powi(2) + (ey - pcy).powi(2) <= r2
                })
                .map(|e| e.id)
                .collect()
        };
        if targets.is_empty() {
            return;
        }
        let mut msgs = Vec::new();
        {
            let mut entities = self.entities(dim).lock();
            for tid in targets {
                // A killing blow — far above any creature's health, but well clear
                // of `i32::MAX` so the damage subtraction can't overflow. The respawn
                // arg is unused for these non-player foes.
                let (m, _) = apply_damage(
                    &mut entities,
                    tid,
                    1_000_000,
                    (0.0, 0.0),
                    dim,
                    (dim, 0.0, 0.0),
                );
                msgs.extend(m);
            }
        }
        for m in msgs {
            self.broadcast_dim(dim, m);
        }
    }

    /// Enact a death respawn returned by [`apply_damage`]: a same-dimension respawn
    /// just teleports the avatar (already repositioned server-side) and drops a
    /// death marker; a cross-dimension one routes through [`enter_dimension`].
    fn finish_respawn(&self, info: RespawnInfo, current_dim: Dimension) {
        if info.dim == current_dim {
            self.send_to(
                info.id,
                ServerMessage::Respawn {
                    x: info.x,
                    y: info.y,
                    died: true,
                },
            );
        } else {
            self.enter_dimension(info.id, info.dim, info.x, info.y);
        }
    }

    /// Check whether player `id`, now at world pixel `(x, y)` in dimension `dim`,
    /// has crossed a dimension boundary, and if so move them across. Returns
    /// whether a transition happened (so the caller can skip a now-stale move
    /// broadcast). Falling past the overworld's floor drops the player into the
    /// underworld; rising above the underworld's ceiling returns them to the
    /// overworld's depths.
    fn maybe_transition(&self, id: EntityId, dim: Dimension, x: f32, y: f32) -> bool {
        match dim {
            // Fell off the bottom of the overworld: arrive at the top of the
            // underworld and drop the short way onto its charred surface.
            Dimension::Overworld
                if y >= ((WORLD_HEIGHT - OVERWORLD_BOTTOM_ROWS) as f32) * TILE_SIZE =>
            {
                let cell_x = (x / TILE_SIZE).floor() as i32;
                let surface = self.world(Dimension::Underworld).lock().surface(cell_x);
                // Land a few tiles above the ceiling so the player drops onto it.
                let ny = ((surface - 3).max(0) as f32) * TILE_SIZE;
                self.enter_dimension(id, Dimension::Underworld, x, ny);
                true
            }
            // Climbed above the underworld's ceiling: surface back in the overworld
            // near its bottom, where the player originally broke through.
            Dimension::Underworld if y <= 0.0 => {
                let cell_x = (x / TILE_SIZE).floor() as i32;
                let (nx, ny) = self.carve_overworld_landing(cell_x);
                self.enter_dimension(id, Dimension::Overworld, nx, ny);
                true
            }
            _ => false,
        }
    }

    /// Carve a small air pocket at the overworld's floor in column `cell_x` so a
    /// player surfacing from the underworld lands in open space (rather than
    /// suffocating in solid rock), and return the world-pixel position to drop them
    /// at. A solid floor is guaranteed beneath them. Broadcasts the carved cells to
    /// the overworld's other clients.
    fn carve_overworld_landing(&self, cell_x: i32) -> (f32, f32) {
        let b = WORLD_HEIGHT;
        let mut updates = Vec::new();
        {
            let mut world = self.world(Dimension::Overworld).lock();
            // Headroom + body: two cells of air to stand in.
            for ty in [b - 3, b - 2] {
                if world.get(cell_x, ty) != crate::block::AIR
                    && world.set(cell_x, ty, crate::block::AIR)
                {
                    updates.push((cell_x, ty, crate::block::AIR));
                }
            }
            // Make sure there is solid footing directly beneath them.
            let below = world.get(cell_x, b - 1);
            if !world.registry.is_solid(below) {
                world.set(cell_x, b - 1, crate::block::STONE);
                updates.push((cell_x, b - 1, crate::block::STONE));
            }
        }
        for (x, y, block) in updates {
            self.broadcast_dim(
                Dimension::Overworld,
                ServerMessage::BlockUpdate {
                    dim: Dimension::Overworld,
                    x,
                    y,
                    block,
                },
            );
        }
        // Centre the 11px-wide body in the column; top sits on row b-2 so the feet
        // rest on the solid floor at row b-1.
        let px = cell_x as f32 * TILE_SIZE + (TILE_SIZE - PLAYER_SIZE.0) / 2.0;
        let py = (b - 2) as f32 * TILE_SIZE;
        (px, py)
    }
}

/// Whether a log lies within [`LEAF_SUPPORT_RANGE`] (Chebyshev) of cell
/// `(x, y)` — i.e. whether a leaf there still has nearby wood to cling to.
fn leaf_supported(world: &mut ServerWorld, x: i32, y: i32) -> bool {
    let r = LEAF_SUPPORT_RANGE;
    for dy in -r..=r {
        for dx in -r..=r {
            if world.get(x + dx, y + dy) == crate::block::LOG {
                return true;
            }
        }
    }
    false
}

/// The most mana a player can bank at once. Mana over this is wasted (a kill never
/// pushes it past the cap). Shared with the client only as the `max` in a
/// [`ServerMessage::Mana`] so its bar knows its denominator.
const MANA_MAX: i32 = 1000;

/// How much mana slaying a creature of `kind` rewards the killer. Tougher and rarer
/// monsters give more; peaceable animals and companions give none (mana is won by
/// fighting *monsters*). A non-hostile kind always returns `0`.
fn creature_mana(kind: &EntityKind) -> i32 {
    match kind {
        EntityKind::Slime => 20,
        EntityKind::Spider => 30,
        EntityKind::Snake => 40,
        EntityKind::Skeleton => 50,
        EntityKind::Zombie => 70,
        EntityKind::AshTwister => 60,
        EntityKind::CharredSkeleton => 90,
        EntityKind::Demon => 90,
        EntityKind::Gargoyle => 100,
        EntityKind::EnchantedDemon => 120,
        EntityKind::Necromancer => 110,
        EntityKind::OrcMage => 110,
        EntityKind::Orc => 130,
        EntityKind::Skull => 10,
        EntityKind::DarkKnight => 160,
        EntityKind::DarkMusketeer => 150,
        EntityKind::Dragon => 600,
        EntityKind::Minotaur => 600,
        EntityKind::DemonKing => 1000,
        EntityKind::Twinscale => 2000,
        // Animals, companions, projectiles, items, players: no mana.
        _ => 0,
    }
}

/// The hoard a slain [`EntityKind::Twinscale`] spills where it falls — a generous
/// post-game reward (it flies, so it has no chest like the demon king): a clutch of
/// dragon scales, a purse of refined metal, and — found nowhere else — the
/// [dragon plate spellbook](crate::block::DRAGON_PLATE_SPELL) (cloneable thereafter).
fn twinscale_loot() -> Vec<(BlockId, u32)> {
    vec![
        (crate::block::DRAGON_SCALE, 10),
        (crate::block::GOLD_INGOT, 6),
        (crate::block::TUNGSTEN_INGOT, 6),
        (crate::block::DRAGON_PLATE_SPELL, 1),
    ]
}

/// The loot a slain [`EntityKind::Necromancer`] spills. Nothing most kills, but
/// *rarely* it yields its [`crate::block::SUMMONER_SPELL`] spellbook — the seed of
/// the world's magic system and (for now) its only source. `seed` (the victim's
/// entity id) varies the roll from kill to kill without needing a live rng handle at
/// the spill site, mirroring [`dark_knight_loot`].
fn necromancer_loot(seed: u32) -> Vec<(BlockId, u32)> {
    let h = seed.wrapping_mul(2_654_435_761) ^ (seed >> 15);
    if h % 100 < NECROMANCER_SPELLBOOK_DROP_PERCENT {
        vec![(crate::block::SUMMONER_SPELL, 1)]
    } else {
        Vec::new()
    }
}

/// Items a slain creature of `kind` drops, as `(item, count)` pairs. Animals
/// (chickens, goats) drop raw meat; everything else drops nothing.
fn creature_loot(kind: &EntityKind) -> &'static [(BlockId, u32)] {
    match kind {
        EntityKind::Chicken => &[(crate::block::RAW_MEAT, 1)],
        EntityKind::Goat => &[(crate::block::RAW_MEAT, 2)],
        EntityKind::Dragon => &[(crate::block::DRAGON_SCALE, 1)],
        EntityKind::Minotaur => &[(crate::block::MINOTAUR_HORN, 1)],
        _ => &[],
    }
}

/// The tungsten haul a slain [`EntityKind::DarkKnight`] spills — the only way to win
/// tungsten without braving the underworld. Always a clutch of ingots, and often a
/// finished tungsten tool or weapon on top. `seed` (the victim's entity id) varies the
/// drop from kill to kill without needing a live rng handle at the spill site.
fn dark_knight_loot(seed: u32) -> Vec<(BlockId, u32)> {
    let mut h = seed.wrapping_mul(2_654_435_761) ^ (seed >> 15);
    // 1..=3 tungsten ingots, always.
    let ingots = 1 + h % 3;
    let mut loot = vec![(crate::block::TUNGSTEN_INGOT, ingots)];
    // Often also a finished tungsten tool or weapon (full durability via spawn_drop).
    h = h.wrapping_mul(40_503) ^ (h >> 13);
    if h % 100 < DARK_KNIGHT_TOOL_DROP_PERCENT {
        h = h.wrapping_mul(2_246_822_519) ^ (h >> 16);
        let tool = match h % 3 {
            0 => crate::block::TUNGSTEN_PICKAXE,
            1 => crate::block::TUNGSTEN_SWORD,
            _ => crate::block::TUNGSTEN_AXE,
        };
        loot.push((tool, 1));
    }
    // Rarely, a full suit of tungsten armor on top of everything else.
    h = h.wrapping_mul(2_654_435_761) ^ (h >> 12);
    if h % 100 < DARK_KNIGHT_ARMOR_DROP_PERCENT {
        loot.push((crate::block::TUNGSTEN_ARMOR, 1));
    }
    loot
}

/// The chest of spoils a slain demon king leaves behind: the full tungsten arsenal
/// at fresh durability, a hoard of tungsten ingots and gold, and a meal for the road.
/// Returned as a [`CHEST_SLOTS`]-long slot list ready to drop into [`Shared::chest_store`].
fn demon_king_loot() -> Vec<Slot> {
    let tool = |b| Some((b, 1u32, crate::block::max_durability(b)));
    let stack = |b, n| Some((b, n, 0u16));
    let mut slots = vec![None; CHEST_SLOTS];
    slots[0] = tool(crate::block::TUNGSTEN_SWORD);
    slots[1] = tool(crate::block::TUNGSTEN_PICKAXE);
    slots[2] = tool(crate::block::TUNGSTEN_AXE);
    slots[3] = stack(crate::block::TUNGSTEN_INGOT, 16);
    slots[4] = stack(crate::block::GOLD_INGOT, 12);
    slots[5] = stack(crate::block::COOKED_MEAT, 8);
    // The king's own armor: a full suit of tungsten at fresh durability (use `tool`,
    // not `stack`, so it arrives undamaged), the spoils of the final fight.
    slots[6] = tool(crate::block::TUNGSTEN_ARMOR);
    // The restore spellbook: the king's deepest secret — the magic to turn its own
    // brutes and shades into loyal knights and mages.
    slots[7] = Some((crate::block::RESTORE_SPELL, 1u32, 0u16));
    // The dragonian steed spellbook: too potent to leave to a miniboss's spoils, the
    // magic to raise a friendly dragon rests with the king who once kept them as pets.
    slots[8] = Some((crate::block::DRAGONIAN_STEED, 1u32, 0u16));
    slots
}

/// The contents of the loot chest a ruin generates with: its prize is the
/// [sunburst spellbook](crate::block::SUNBURST_SPELL). Returned as a
/// [`CHEST_SLOTS`]-long slot list ready to drop into [`Shared::chest_store`].
fn ruin_chest_loot() -> Vec<Slot> {
    let mut slots = vec![None; CHEST_SLOTS];
    slots[0] = Some((crate::block::SUNBURST_SPELL, 1u32, 0u16));
    slots
}

/// Place the demon king's loot chest on the arena floor in the column it died over,
/// fill it with [`demon_king_loot`], and announce the new block to the arena. The
/// chest rests in the open cell just above the floor so a fighter can walk up and
/// open it. `boss_x` is the slain king's left edge (its size centers the column).
fn spawn_boss_chest(shared: &Shared, dim: Dimension, boss_x: f32) {
    let (w, _) = EntityKind::DemonKing.size();
    let cell_x = ((boss_x + w * 0.5) / TILE_SIZE).floor() as i32;
    let cy = {
        let mut world = shared.world(dim).lock();
        let floor = world.surface(cell_x);
        let cy = (floor - 1).max(0);
        world.set(cell_x, cy, crate::block::CHEST);
        cy
    };
    shared
        .chest_store
        .lock()
        .insert((dim, cell_x, cy), demon_king_loot());
    shared.broadcast_dim(
        dim,
        ServerMessage::BlockUpdate {
            dim,
            x: cell_x,
            y: cy,
            block: crate::block::CHEST,
        },
    );
}

/// Start a server on a background thread. `bind` of port 0 picks an ephemeral
/// port (used for the embedded singleplayer server). Returns once the endpoint
/// is listening, with its actual address and certificate fingerprint.
pub fn start_server(
    bind: SocketAddr,
    seed: i32,
    save_dir: PathBuf,
    creator_world: bool,
) -> Result<RunningServer> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Ready>();

    std::thread::Builder::new()
        .name("game-server".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.into()));
                    return;
                }
            };
            rt.block_on(async move {
                let shared = match setup(bind, seed, save_dir, creator_world, &ready_tx).await {
                    Some(s) => s,
                    None => return,
                };
                // The endpoint is owned by RunningServer on the main thread; we
                // re-fetch via the shared accept loop below.
                accept_loop(shared).await;
            });
        })
        .context("spawning server thread")?;

    match ready_rx.recv().context("server failed to start")? {
        Ok((addr, fp, endpoint, shared)) => Ok(RunningServer {
            addr,
            fingerprint: fp,
            creator_token: shared.creator_token,
            _endpoint: endpoint,
            shared,
            _discovery: None,
            _upnp: None,
            _relay: None,
            relay_info: None,
            _voice_upnp: None,
        }),
        Err(e) => Err(e),
    }
}

/// State needed by the accept loop.
struct AcceptCtx {
    endpoint: Endpoint,
    shared: Arc<Shared>,
}

async fn setup(
    bind: SocketAddr,
    seed: i32,
    save_dir: PathBuf,
    creator_world: bool,
    ready_tx: &std::sync::mpsc::Sender<Ready>,
) -> Option<AcceptCtx> {
    match build_endpoint(bind, seed, save_dir, creator_world) {
        Ok((endpoint, fp, shared)) => {
            let addr = match endpoint.local_addr() {
                Ok(a) => a,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.into()));
                    return None;
                }
            };
            // Hand a clone of the endpoint to the caller (keeps it alive there),
            // keep our own for the accept loop.
            let _ = ready_tx.send(Ok((addr, fp, endpoint.clone(), shared.clone())));
            Some(AcceptCtx { endpoint, shared })
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            None
        }
    }
}

/// File names for the persisted TLS identity, stored per-machine.
const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.der";

/// Directory holding the per-machine TLS identity, under the user config dir
/// (alongside `known_hosts`/`credentials`). Keeping the identity per-machine
/// rather than per-world means the certificate fingerprint stays stable across
/// every world this machine hosts, so inviting someone into a freshly created
/// world doesn't trip a (false) "certificate changed" rejection on clients that
/// pinned the fingerprint via TOFU.
fn identity_dir() -> Result<PathBuf> {
    let mut p = dirs::config_dir().ok_or_else(|| anyhow!("no config dir"))?;
    p.push("survival-cubed");
    Ok(p)
}

/// Load the server's certificate and private key, generating and persisting a
/// fresh self-signed pair on first run (or if the saved pair is
/// missing/unreadable). The pair is stored per-machine so the certificate
/// fingerprint is stable across restarts and across worlds, so clients that
/// pinned it via TOFU don't see a (false) "certificate changed" alarm.
fn load_or_create_identity() -> Result<(CertificateDer<'static>, PrivatePkcs8KeyDer<'static>)> {
    let dir = identity_dir()?;
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);

    // Reuse the saved pair when both files are present and readable.
    if let (Ok(cert), Ok(key)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
        return Ok((CertificateDer::from(cert), PrivatePkcs8KeyDer::from(key)));
    }

    // First launch (or unreadable): mint a fresh pair and persist it.
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .context("generating self-signed certificate")?;
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();

    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    write_private_key(&key_path, &key_der)?;
    std::fs::write(&cert_path, &cert_der)
        .with_context(|| format!("writing {}", cert_path.display()))?;

    Ok((
        CertificateDer::from(cert_der),
        PrivatePkcs8KeyDer::from(key_der),
    ))
}

/// Write a private key to disk, restricting it to the owner on Unix.
fn write_private_key(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn build_endpoint(
    bind: SocketAddr,
    seed: i32,
    save_dir: PathBuf,
    creator_world: bool,
) -> Result<(Endpoint, [u8; 32], Arc<Shared>)> {
    // Self-signed certificate for "localhost", persisted per-machine so the
    // fingerprint is stable across restarts and across worlds.
    let (cert_der, key_der) = load_or_create_identity()?;
    let fp = fingerprint(cert_der.as_ref());

    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())
        .context("building rustls server config")?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let qsc = quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
        .context("building QUIC server config")?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(qsc));

    let endpoint = Endpoint::server(server_config, bind).context("binding server endpoint")?;

    let bans_path = save_dir.join(BANS_FILE);
    let banned_ips = load_bans(&bans_path);

    let store = WorldStore::new(save_dir.clone());
    // Resume a previous save if one exists, otherwise create a fresh world.
    let saved = match store.load_meta() {
        Ok(meta) => meta,
        Err(e) => {
            log::error!("failed to load world save; starting fresh: {e:#}");
            None
        }
    };

    let world_seed = saved.as_ref().map(|m| m.seed).unwrap_or(seed);
    let generator = WorldGen::new(world_seed);
    let spawn = saved
        .as_ref()
        .map(|m| m.spawn)
        .unwrap_or_else(|| spawn_point(&generator, generator.land_spawn_x()));

    // Offset the clock so a loaded world resumes at the time of day it was saved.
    let start = match saved.as_ref() {
        Some(m) => Instant::now()
            .checked_sub(Duration::from_secs_f32(m.elapsed_secs))
            .unwrap_or_else(Instant::now),
        None => Instant::now(),
    };
    let next_id = saved.as_ref().map(|m| m.next_id.max(1)).unwrap_or(1);

    // Mint a per-server admin secret from the wall clock (mixed with next_id so
    // two servers started in the same instant still differ). It is never
    // broadcast, so a remote client can't learn it; only the host's in-process
    // client is handed it via RunningServer.
    let creator_token = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xD157_C0DE)
        ^ (next_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);

    // A creator-type world is fixed at creation; a resumed world keeps whatever it
    // was saved as, ignoring the freshly requested flag.
    let creator_world = saved
        .as_ref()
        .map(|m| m.creator_server)
        .unwrap_or(creator_world);

    // Restore saved creatures into each dimension's live world; players go to the
    // saved set.
    let mut overworld_entities = Entities::new();
    let mut underworld_entities = Entities::new();
    let mut arena_entities = Entities::new();
    let mut demon_king_slain = false;
    let mut demon_king_slain_at: Option<f32> = None;
    let mut twinscale_slain = false;
    let mut saved_players = HashMap::new();
    let mut accounts = HashMap::new();
    let mut campfires = HashMap::new();
    let mut placed_logs = HashSet::new();
    let mut spawned_structure_roots = HashSet::new();
    let mut block_text = HashMap::new();
    let mut chest_store = HashMap::new();
    let mut chest_locks = HashMap::new();
    if let Some(m) = &saved {
        for e in &m.entities {
            overworld_entities.insert(e.clone());
        }
        for e in &m.underworld_entities {
            underworld_entities.insert(e.clone());
        }
        for e in &m.arena_entities {
            arena_entities.insert(e.clone());
        }
        demon_king_slain = m.demon_king_slain;
        demon_king_slain_at = m.demon_king_slain_at;
        twinscale_slain = m.twinscale_slain;
        for p in &m.players {
            saved_players.insert(p.name.clone(), p.clone());
        }
        for (name, hash) in &m.accounts {
            accounts.insert(name.clone(), hash.clone());
        }
        for &(dim, x, y, secs) in &m.campfires {
            campfires.insert((dim, x, y), secs);
        }
        for &(dim, x, y) in &m.placed_logs {
            placed_logs.insert((dim, x, y));
        }
        for (dim, x, y, text) in &m.block_text {
            block_text.insert((*dim, *x, *y), text.clone());
        }
        for (dim, x, y, slots) in &m.chests {
            let mut slots = slots.clone();
            slots.resize(CHEST_SLOTS, None);
            chest_store.insert((*dim, *x, *y), slots);
        }
        for (dim, x, y, pw) in &m.chest_locks {
            chest_locks.insert((*dim, *x, *y), pw.clone());
        }
        spawned_structure_roots.extend(m.spawned_structure_roots.iter().copied());
    }

    // One terrain world per dimension, each with its own generator (same seed) and
    // a store pointed at the shared save directory (dimensions write to separate
    // chunk subdirectories).
    let make_world = |dim: Dimension| {
        Mutex::new(ServerWorld {
            dim,
            world: World::new(),
            generator: WorldGen::new(world_seed),
            registry: BlockRegistry::new(),
            store: WorldStore::new(save_dir.clone()),
            dirty: HashSet::new(),
        })
    };
    // Keep `generator` alive for the spawn helpers below by storing it in the
    // overworld; build the underworld fresh.
    let overworld = Mutex::new(ServerWorld {
        dim: Dimension::Overworld,
        world: World::new(),
        generator,
        registry: BlockRegistry::new(),
        store,
        dirty: HashSet::new(),
    });

    // Whether a king (or Twinscale) is already presiding (e.g. a save captured
    // mid-fight), so the runtime "alive" mirrors start in agreement with the restored
    // entities.
    let king_present = arena_entities
        .values()
        .any(|e| matches!(e.kind, EntityKind::DemonKing));
    let twinscale_present = arena_entities
        .values()
        .any(|e| matches!(e.kind, EntityKind::Twinscale));
    // A restored Twinscale already at or below half health has already called down its
    // flight of dragons, so re-latch that to avoid summoning a fresh flight on reload.
    let twinscale_dragons_done = arena_entities
        .values()
        .find(|e| matches!(e.kind, EntityKind::Twinscale))
        .is_some_and(|e| e.health * 2 <= e.max_health);
    // If the king is recorded slain but we have no timestamp (a pre-Twinscale save),
    // start the five-day countdown from the world's current time, so Twinscale still
    // eventually comes rather than never appearing.
    if demon_king_slain && demon_king_slain_at.is_none() {
        demon_king_slain_at = Some(start.elapsed().as_secs_f32());
    }

    let shared = Arc::new(Shared {
        worlds_by_dim: [
            overworld,
            make_world(Dimension::Underworld),
            make_world(Dimension::Arena),
        ],
        clients: Mutex::new(HashMap::new()),
        entities_by_dim: [
            Mutex::new(overworld_entities),
            Mutex::new(underworld_entities),
            Mutex::new(arena_entities),
        ],
        client_dim: Mutex::new(HashMap::new()),
        next_id: AtomicU32::new(next_id),
        spawn,
        start: Mutex::new(start),
        creator_token,
        creator_world,
        creators: Mutex::new(HashSet::new()),
        spawned_structure_roots: Mutex::new(spawned_structure_roots),
        saved_players: Mutex::new(saved_players),
        accounts: Mutex::new(accounts),
        inventories: Mutex::new(HashMap::new()),
        mana: Mutex::new(HashMap::new()),
        respawn_points: Mutex::new(HashMap::new()),
        waypoints: Mutex::new(HashMap::new()),
        campfires: Mutex::new(campfires),
        placed_logs: Mutex::new(placed_logs),
        trail_fires: Mutex::new(HashMap::new()),
        fire_cd: Mutex::new(HashMap::new()),
        knight_targets: Mutex::new(HashMap::new()),
        musket_cd: Mutex::new(HashMap::new()),
        shutdown: AtomicBool::new(false),
        voice: Mutex::new(None),
        webcam: Mutex::new(None),
        map: Mutex::new(None),
        // Safe: build_endpoint runs inside the server's tokio runtime (via
        // `setup`'s `block_on`), so a runtime handle is current here.
        rt: tokio::runtime::Handle::current(),
        banned_ips: Mutex::new(banned_ips),
        bans_path,
        spectate_return: Mutex::new(HashMap::new()),
        fire_marks: Mutex::new(HashMap::new()),
        block_text: Mutex::new(block_text),
        chest_store: Mutex::new(chest_store),
        chest_locks: Mutex::new(chest_locks),
        chest_viewers: Mutex::new(HashMap::new()),
        chest_auth: Mutex::new(HashMap::new()),
        arena_return: Mutex::new(HashMap::new()),
        demon_king_slain: AtomicBool::new(demon_king_slain),
        demon_king_alive: AtomicBool::new(king_present),
        demon_king_knights_summoned: AtomicBool::new(false),
        demon_king_slain_at: Mutex::new(demon_king_slain_at),
        twinscale_slain: AtomicBool::new(twinscale_slain),
        twinscale_alive: AtomicBool::new(twinscale_present),
        twinscale_dragons_summoned: AtomicBool::new(twinscale_dragons_done),
    });

    // Only seed fresh creatures for a brand-new world; a loaded one keeps its own.
    if saved.is_none() {
        spawn_creatures(&shared);
    }

    Ok((endpoint, fp, shared))
}

async fn accept_loop(ctx: AcceptCtx) {
    // Drive server-simulated entities on their own task.
    tokio::spawn(entity_tick_loop(ctx.shared.clone()));

    while let Some(incoming) = ctx.endpoint.accept().await {
        let shared = ctx.shared.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(incoming, shared).await {
                log::debug!("connection ended: {e:#}");
            }
        });
    }
}

/// Populate the world with biome-appropriate creatures scattered around the
/// origin: chickens roam the plains, while slimes and goats inhabit the
/// mountains. Each spawn slot looks up its column's biome and spawns whatever
/// belongs there.
fn spawn_creatures(shared: &Shared) {
    let world = shared.world(Dimension::Overworld).lock();
    let mut entities = shared.entities(Dimension::Overworld).lock();
    for i in 0..SPAWN_SLOTS {
        let cell_x = (i - SPAWN_SLOTS / 2) * SPAWN_SPACING;
        // Alternate hostile/placid creatures in mountains; `i` parity keeps the
        // original mix around the origin. Biomes with no life (ocean) seed nothing.
        let Some(kind) = creature_for_biome(world.biome(cell_x), i % 2 == 0) else {
            continue;
        };
        let (_, h) = kind.size();
        let surface = world.surface(cell_x);
        let id = shared.alloc_id();
        let x = cell_x as f32 * TILE_SIZE;
        let y = surface as f32 * TILE_SIZE - h;
        entities.insert(Entity::new(id, kind, x, y));
    }
}

/// Which creature a column's biome supports, or `None` for a biome with no
/// ambient life. `hostile_slot` selects between the two mountain dwellers (slime
/// when true, goat when false) so callers can mix them; plains always yield a
/// chicken.
fn creature_for_biome(biome: Biome, hostile_slot: bool) -> Option<EntityKind> {
    match biome {
        // Plains and forest: peaceful chickens beneath the trees. Deserts, too —
        // a lone chicken pecking the dunes is the only life out there.
        Biome::Plains | Biome::Forest | Biome::Desert => Some(EntityKind::Chicken),
        // Mountains: hostile slimes interspersed with placid goats.
        Biome::Mountains => Some(if hostile_slot {
            EntityKind::Slime
        } else {
            EntityKind::Goat
        }),
        // The open ocean has no creatures for now (sea life may come later).
        Biome::Ocean => None,
    }
}

/// Deterministic pseudo-random value for chunk `(cx, cy)` mixed with `salt`,
/// derived from the world seed so the same chunk always makes the same spawn
/// decision regardless of who explores it or when.
fn chunk_hash(seed: i32, cx: i32, cy: i32, salt: u32) -> u32 {
    let mut h = (seed as u32)
        .wrapping_mul(374_761_393)
        .wrapping_add((cx as u32).wrapping_mul(668_265_263))
        .wrapping_add((cy as u32).wrapping_mul(2_246_822_519));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h = h.wrapping_add(salt.wrapping_mul(0x9E37_79B9));
    h ^ (h >> 16)
}

/// Percent chance that a freshly generated surface chunk seeds creatures.
const CHUNK_SPAWN_CHANCE: u32 = 40;
/// Most creatures a single chunk can seed at once.
const CHUNK_SPAWN_MAX: u32 = 3;

/// Possibly seed biome-appropriate creatures into a chunk that has just come
/// into existence, broadcasting any spawns to connected clients. Only surface
/// chunks (those the terrain's grass line passes through) are eligible, and the
/// decision is deterministic per chunk via [`chunk_hash`], so exploring the same
/// terrain never double-spawns. Mirrors [`spawn_creatures`] for placement.
fn maybe_spawn_in_chunk(shared: &Shared, cx: i32, cy: i32) {
    let world = shared.world(Dimension::Overworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 0) % 100 >= CHUNK_SPAWN_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let count = 1 + chunk_hash(seed, cx, cy, 1) % CHUNK_SPAWN_MAX;

    let mut spawned = Vec::new();
    {
        let mut entities = shared.entities(Dimension::Overworld).lock();
        for n in 0..count {
            // Scatter spawns across the chunk's columns.
            let lx = chunk_hash(seed, cx, cy, 2 + n) % CHUNK_SIZE as u32;
            let cell_x = base_x + lx as i32;
            let surface = world.surface(cell_x);
            // Only spawn where this chunk actually contains the ground surface,
            // so creatures never appear buried in stone or floating in the sky.
            if surface < chunk_top || surface >= chunk_bottom {
                continue;
            }
            let Some(kind) = creature_for_biome(world.biome(cell_x), n % 2 == 0) else {
                continue;
            };
            let (_, h) = kind.size();
            let id = shared.alloc_id();
            let x = cell_x as f32 * TILE_SIZE;
            let y = surface as f32 * TILE_SIZE - h;
            let entity = Entity::new(id, kind, x, y);
            entities.insert(entity.clone());
            spawned.push(entity);
        }
    }
    drop(world);

    for entity in spawned {
        shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
    }
}

/// Realize a worldgen structure when its overworld chunk first comes into
/// existence — but only once per structure, ever. This both **spawns the creatures**
/// the structure embeds *and* **drops its loot chest** (a ruin's holds the
/// [sunburst spellbook](crate::block::SUNBURST_SPELL)) onto the structure's floor.
/// The root column is recorded in [`Shared::spawned_structure_roots`] (persisted),
/// so a re-streamed chunk or a reloaded world never duplicates them.
fn maybe_realize_structures(shared: &Shared, cx: i32, cy: i32) {
    let (roots, entities, chest_off) = {
        let world = shared.world(Dimension::Overworld).lock();
        (
            world.generator.structure_roots_in_chunk(cx, cy),
            world.generator.structure_entities_in_chunk(cx, cy),
            world.generator.structure_chest_offset(),
        )
    };
    if roots.is_empty() {
        return;
    }

    // Realize each root not yet done: spawn its creatures and remember it so we can
    // drop its loot chest below (after releasing the entities lock).
    let mut spawned = Vec::new();
    let mut newly_done: Vec<(i32, i32)> = Vec::new(); // (root_x, top_row)
    {
        let mut done = shared.spawned_structure_roots.lock();
        let mut ents = shared.entities(Dimension::Overworld).lock();
        for &(root_x, top) in &roots {
            if done.contains(&root_x) {
                continue;
            }
            for (x, y, _, kind) in entities.iter().filter(|(.., rx, _)| *rx == root_x) {
                let id = shared.alloc_id();
                let entity = Entity::new(id, kind.clone(), *x, *y);
                ents.insert(entity.clone());
                spawned.push(entity);
            }
            done.insert(root_x);
            newly_done.push((root_x, top));
        }
    }

    for entity in spawned {
        shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
    }

    // Drop each freshly-realized structure's loot chest onto its floor.
    if let Some((odx, ody)) = chest_off {
        for (root_x, top) in newly_done {
            let (cell_x, cell_y) = (root_x + odx, top + ody);
            let placed =
                shared
                    .world(Dimension::Overworld)
                    .lock()
                    .set(cell_x, cell_y, crate::block::CHEST);
            if placed {
                shared
                    .chest_store
                    .lock()
                    .insert((Dimension::Overworld, cell_x, cell_y), ruin_chest_loot());
                shared.broadcast_dim(
                    Dimension::Overworld,
                    ServerMessage::BlockUpdate {
                        dim: Dimension::Overworld,
                        x: cell_x,
                        y: cell_y,
                        block: crate::block::CHEST,
                    },
                );
            }
        }
    }
}

/// Possibly seed a single wild cat into a freshly generated forest surface chunk.
/// Cats are a rare forest find, so the per-chunk chance is deliberately tiny and
/// at most one appears. Like the other surface critters it only spawns in the
/// chunk that actually holds this column's grass line, and the decision is
/// deterministic per chunk via [`chunk_hash`] on its own salt range so exploring
/// the same terrain never double-spawns. A fresh cat is wild (`owner: None`).
fn maybe_spawn_cats(shared: &Shared, cx: i32, cy: i32) {
    let world = shared.world(Dimension::Overworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 300) % 100 >= CAT_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Cat {
        owner: None,
        sitting: false,
    }
    .size();

    // Drop the cat onto the grass of one scattered column, but only where this
    // chunk holds the surface and the column is forest.
    let lx = chunk_hash(seed, cx, cy, 301) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    let surface = world.surface(cell_x);
    if world.biome(cell_x) != Biome::Forest || surface < chunk_top || surface >= chunk_bottom {
        return;
    }
    let id = shared.alloc_id();
    let entity = Entity::new(
        id,
        EntityKind::Cat {
            owner: None,
            sitting: false,
        },
        cell_x as f32 * TILE_SIZE,
        surface as f32 * TILE_SIZE - h,
    );
    drop(world);

    shared
        .entities(Dimension::Overworld)
        .lock()
        .insert(entity.clone());
    shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed a single wild puppy into a freshly generated forest surface
/// chunk. Like the cat, puppies are a rare forest find, so the per-chunk chance is
/// deliberately tiny and at most one appears, only in the chunk that holds this
/// column's grass line. The decision is deterministic per chunk via [`chunk_hash`]
/// on its own salt range (distinct from the cat's) so exploring the same terrain
/// never double-spawns and the two run independently. A fresh puppy is wild
/// (`owner: None`).
fn maybe_spawn_puppies(shared: &Shared, cx: i32, cy: i32) {
    let world = shared.world(Dimension::Overworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 400) % 100 >= PUPPY_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Puppy {
        owner: None,
        sitting: false,
    }
    .size();

    // Drop the puppy onto the grass of one scattered column, but only where this
    // chunk holds the surface and the column is forest.
    let lx = chunk_hash(seed, cx, cy, 401) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    let surface = world.surface(cell_x);
    if world.biome(cell_x) != Biome::Forest || surface < chunk_top || surface >= chunk_bottom {
        return;
    }
    let id = shared.alloc_id();
    let entity = Entity::new(
        id,
        EntityKind::Puppy {
            owner: None,
            sitting: false,
        },
        cell_x as f32 * TILE_SIZE,
        surface as f32 * TILE_SIZE - h,
    );
    drop(world);

    shared
        .entities(Dimension::Overworld)
        .lock()
        .insert(entity.clone());
    shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed a wild horse into a freshly generated chunk. Horses are a plains
/// find: tameable with apples and, once tamed, rideable. Mirrors the cat/puppy
/// spawners (its own `chunk_hash` salts keep it independent of theirs) but seeds
/// the open grassland rather than the forest.
fn maybe_spawn_horses(shared: &Shared, cx: i32, cy: i32) {
    let world = shared.world(Dimension::Overworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 500) % 100 >= HORSE_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Horse { owner: None }.size();

    // Drop the horse onto the grass of one scattered column, but only where this
    // chunk holds the surface and the column is plains.
    let lx = chunk_hash(seed, cx, cy, 501) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    let surface = world.surface(cell_x);
    if world.biome(cell_x) != Biome::Plains || surface < chunk_top || surface >= chunk_bottom {
        return;
    }
    let id = shared.alloc_id();
    let entity = Entity::new(
        id,
        EntityKind::Horse { owner: None },
        cell_x as f32 * TILE_SIZE,
        surface as f32 * TILE_SIZE - h,
    );
    drop(world);

    shared
        .entities(Dimension::Overworld)
        .lock()
        .insert(entity.clone());
    shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed a wandering knight onto a freshly generated **plains** chunk, like
/// the horses that share the plains but rarer (see [`KNIGHT_CHUNK_CHANCE`]). A fresh
/// knight is wild (`owner: None`) until a player recruits it with a tungsten ingot.
/// Deterministic per chunk via [`chunk_hash`] on its own salt range, so re-exploring
/// the same terrain never double-spawns, and it runs independently of the other
/// surface spawners.
fn maybe_spawn_knights(shared: &Shared, cx: i32, cy: i32) {
    let world = shared.world(Dimension::Overworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 600) % 100 >= KNIGHT_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Knight { owner: None }.size();

    let lx = chunk_hash(seed, cx, cy, 601) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    let surface = world.surface(cell_x);
    if world.biome(cell_x) != Biome::Plains || surface < chunk_top || surface >= chunk_bottom {
        return;
    }
    let id = shared.alloc_id();
    let mut entity = Entity::new(
        id,
        EntityKind::Knight { owner: None },
        cell_x as f32 * TILE_SIZE,
        surface as f32 * TILE_SIZE - h,
    );
    // Some knights are already on horseback when found. A spawn-mounted knight just
    // begins with a horse's worth of mount shield and the "is mounted" marker — it
    // hasn't absorbed a real horse entity, so the marker is a throwaway id no live
    // entity holds (so it harmlessly does nothing in the client's hide-ridden set).
    let mounted = chunk_hash(seed, cx, cy, 602) % 100 < KNIGHT_SPAWN_MOUNTED_CHANCE;
    let mount_marker = if mounted {
        let marker = shared.alloc_id();
        entity.mount_health = crate::entity::HORSE_MAX_HEALTH;
        entity.riding = Some(marker);
        Some(marker)
    } else {
        None
    };
    drop(world);

    shared
        .entities(Dimension::Overworld)
        .lock()
        .insert(entity.clone());
    shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
    // The mount pose rides on the runtime-only `riding` field (not the serialized
    // entity), so follow up with the dedicated message to draw it on horseback.
    if let Some(marker) = mount_marker {
        shared.broadcast_dim(
            Dimension::Overworld,
            ServerMessage::EntityRiding {
                id,
                horse: Some(marker),
            },
        );
    }
}

/// Possibly seed a wandering musketeer onto a freshly generated **plains** chunk, the
/// ranged twin of [`maybe_spawn_knights`]. A fresh musketeer is wild (`owner: None`)
/// until a player recruits it with a tungsten ingot. Deterministic per chunk via
/// [`chunk_hash`] on its own salt range, so re-exploring never double-spawns.
fn maybe_spawn_musketeers(shared: &Shared, cx: i32, cy: i32) {
    let world = shared.world(Dimension::Overworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 640) % 100 >= KNIGHT_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Musketeer { owner: None }.size();

    let lx = chunk_hash(seed, cx, cy, 641) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    let surface = world.surface(cell_x);
    if world.biome(cell_x) != Biome::Plains || surface < chunk_top || surface >= chunk_bottom {
        return;
    }
    let id = shared.alloc_id();
    let entity = Entity::new(
        id,
        EntityKind::Musketeer { owner: None },
        cell_x as f32 * TILE_SIZE,
        surface as f32 * TILE_SIZE - h,
    );
    drop(world);

    shared
        .entities(Dimension::Overworld)
        .lock()
        .insert(entity.clone());
    shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed a wandering farmer onto a freshly generated **plains** chunk, the
/// friendly folk that share the plains with the knights and horses but cull the
/// livestock for a living (see [`FARMER_CHUNK_CHANCE`]). Deterministic per chunk via
/// [`chunk_hash`] on its own salt range, so re-exploring the same terrain never
/// double-spawns, and it runs independently of the other surface spawners.
fn maybe_spawn_farmers(shared: &Shared, cx: i32, cy: i32) {
    let world = shared.world(Dimension::Overworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 680) % 100 >= FARMER_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Farmer.size();

    let lx = chunk_hash(seed, cx, cy, 681) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    let surface = world.surface(cell_x);
    if world.biome(cell_x) != Biome::Plains || surface < chunk_top || surface >= chunk_bottom {
        return;
    }
    let id = shared.alloc_id();
    let entity = Entity::new(
        id,
        EntityKind::Farmer,
        cell_x as f32 * TILE_SIZE,
        surface as f32 * TILE_SIZE - h,
    );
    drop(world);

    shared
        .entities(Dimension::Overworld)
        .lock()
        .insert(entity.clone());
    shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed snakes into a freshly generated chunk. Snakes are the desert's
/// predators: they drop onto the sand of a **desert** surface chunk that actually
/// holds this column's surface line, like the other surface critters. The
/// per-chunk decision is deterministic via [`chunk_hash`] on its own salt range,
/// so exploring the same terrain never double-spawns, and it runs independently of
/// [`maybe_spawn_in_chunk`].
fn maybe_spawn_snakes(shared: &Shared, cx: i32, cy: i32) {
    let world = shared.world(Dimension::Overworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 400) % 100 >= SNAKE_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let count = 1 + chunk_hash(seed, cx, cy, 401) % SNAKE_CHUNK_MAX;
    let (_, h) = EntityKind::Snake.size();

    let mut spawned = Vec::new();
    {
        let mut entities = shared.entities(Dimension::Overworld).lock();
        for n in 0..count {
            let lx = chunk_hash(seed, cx, cy, 402 + n) % CHUNK_SIZE as u32;
            let cell_x = base_x + lx as i32;
            let surface = world.surface(cell_x);
            // Only the desert, and only where this chunk holds the surface line.
            if world.biome(cell_x) != Biome::Desert
                || surface < chunk_top
                || surface >= chunk_bottom
            {
                continue;
            }
            let id = shared.alloc_id();
            let entity = Entity::new(
                id,
                EntityKind::Snake,
                cell_x as f32 * TILE_SIZE,
                surface as f32 * TILE_SIZE - h,
            );
            entities.insert(entity.clone());
            spawned.push(entity);
        }
    }
    drop(world);

    for entity in spawned {
        shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
    }
}

/// Possibly seed charred skeletons into a freshly generated underworld chunk.
/// Like the ash twister they are native to the **ash valleys** alone, so a
/// candidate spawn column is skipped unless it belongs to one (see
/// [`crate::worldgen::WorldGen::underworld_biome_at`]). They drop onto any
/// charred-rock (or ash) floor (open cell above, solid below) the chunk contains.
/// Deterministic per chunk via [`chunk_hash`] on its own salt range, so
/// re-exploring the same terrain never double-spawns.
fn maybe_spawn_charred_skeletons(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Underworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 200) % 100 >= CHARRED_SKELETON_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let count = 1 + chunk_hash(seed, cx, cy, 201) % CHARRED_SKELETON_CHUNK_MAX;
    let (_, h) = EntityKind::CharredSkeleton.size();

    let mut spawned = Vec::new();
    {
        let mut entities = shared.entities(Dimension::Underworld).lock();
        for n in 0..count {
            let lx = chunk_hash(seed, cx, cy, 202 + n) % CHUNK_SIZE as u32;
            let cell_x = base_x + lx as i32;
            // Charred skeletons only haunt the ash valleys — skip a column that
            // isn't one.
            if world.generator.underworld_biome_at(cell_x)
                != crate::worldgen::UnderworldBiome::AshValley
            {
                continue;
            }
            // Find an open cell in this column with solid charred rock beneath —
            // a floor to stand on — scanning from the chunk's bottom upward.
            let mut placed = None;
            for ty in (chunk_top..chunk_bottom).rev() {
                if !world.solid(cell_x, ty) && world.solid(cell_x, ty + 1) {
                    placed = Some((ty + 1) as f32 * TILE_SIZE - h);
                    break;
                }
            }
            let Some(y) = placed else { continue };
            let id = shared.alloc_id();
            let entity = Entity::new(
                id,
                EntityKind::CharredSkeleton,
                cell_x as f32 * TILE_SIZE,
                y,
            );
            entities.insert(entity.clone());
            spawned.push(entity);
        }
    }
    drop(world);

    for entity in spawned {
        shared.broadcast_dim(Dimension::Underworld, ServerMessage::EntitySpawn { entity });
    }
}

/// Possibly seed demons into a freshly generated underworld chunk. They keep to
/// the main **charred** expanse (the ash valleys belong to the twisters and
/// charred skeletons), dropping onto any charred-rock floor (open cell above,
/// solid below) the chunk contains, but they appear more rarely (see
/// [`DEMON_CHUNK_CHANCE`]). Deterministic per chunk via [`chunk_hash`] on its own
/// salt range, so re-exploring the same terrain never double-spawns, and it runs
/// independently of [`maybe_spawn_charred_skeletons`].
fn maybe_spawn_demons(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Underworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 210) % 100 >= DEMON_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Demon.size();

    let lx = chunk_hash(seed, cx, cy, 211) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    // Demons keep to the main charred expanse, ceding the ash valleys to the
    // twisters and charred skeletons — bail if this column is an ash valley.
    if world.generator.underworld_biome_at(cell_x) != crate::worldgen::UnderworldBiome::Charred {
        return;
    }
    // Find an open cell in this column with solid charred rock beneath — a floor to
    // stand on — scanning from the chunk's bottom upward.
    let mut placed = None;
    for ty in (chunk_top..chunk_bottom).rev() {
        if !world.solid(cell_x, ty) && world.solid(cell_x, ty + 1) {
            placed = Some((ty + 1) as f32 * TILE_SIZE - h);
            break;
        }
    }
    let Some(y) = placed else { return };

    let id = shared.alloc_id();
    let entity = Entity::new(id, EntityKind::Demon, cell_x as f32 * TILE_SIZE, y);
    shared
        .entities(Dimension::Underworld)
        .lock()
        .insert(entity.clone());
    drop(world);

    shared.broadcast_dim(Dimension::Underworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed a gargoyle into a freshly generated underworld chunk. Like the
/// demons they once guarded a king beside, they keep to the main **charred** expanse
/// (the ash valleys belong to the twisters and charred skeletons), dropping onto any
/// charred-rock floor (open cell above, solid below) the chunk contains — but they
/// appear a touch more rarely than demons (see [`GARGOYLE_CHUNK_CHANCE`]).
/// Deterministic per chunk via [`chunk_hash`] on its own salt range, so re-exploring
/// the same terrain never double-spawns, and it runs independently of the other
/// underworld spawners.
fn maybe_spawn_gargoyles(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Underworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 260) % 100 >= GARGOYLE_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Gargoyle.size();

    let lx = chunk_hash(seed, cx, cy, 261) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    // Gargoyles keep to the main charred expanse, ceding the ash valleys to the
    // twisters and charred skeletons — bail if this column is an ash valley.
    if world.generator.underworld_biome_at(cell_x) != crate::worldgen::UnderworldBiome::Charred {
        return;
    }
    // Find an open cell in this column with solid charred rock beneath — a floor to
    // stand on — scanning from the chunk's bottom upward.
    let mut placed = None;
    for ty in (chunk_top..chunk_bottom).rev() {
        if !world.solid(cell_x, ty) && world.solid(cell_x, ty + 1) {
            placed = Some((ty + 1) as f32 * TILE_SIZE - h);
            break;
        }
    }
    let Some(y) = placed else { return };

    let id = shared.alloc_id();
    let entity = Entity::new(id, EntityKind::Gargoyle, cell_x as f32 * TILE_SIZE, y);
    shared
        .entities(Dimension::Underworld)
        .lock()
        .insert(entity.clone());
    drop(world);

    shared.broadcast_dim(Dimension::Underworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed orcs into a freshly generated underworld chunk. They keep to the
/// main **charred** expanse (the ash valleys belong to the twisters and charred
/// skeletons), dropping onto any charred-rock floor (open cell above, solid below)
/// the chunk contains. Deterministic per chunk via [`chunk_hash`] on its own salt
/// range, so re-exploring the same terrain never double-spawns, and it runs
/// independently of the other underworld spawners.
fn maybe_spawn_orcs(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Underworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 220) % 100 >= ORC_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let count = 1 + chunk_hash(seed, cx, cy, 221) % ORC_CHUNK_MAX;
    let (_, h) = EntityKind::Orc.size();

    let mut spawned = Vec::new();
    {
        let mut entities = shared.entities(Dimension::Underworld).lock();
        for n in 0..count {
            let lx = chunk_hash(seed, cx, cy, 222 + n) % CHUNK_SIZE as u32;
            let cell_x = base_x + lx as i32;
            // Orcs keep to the main charred expanse, ceding the ash valleys to the
            // twisters and charred skeletons — skip an ash-valley column.
            if world.generator.underworld_biome_at(cell_x)
                != crate::worldgen::UnderworldBiome::Charred
            {
                continue;
            }
            // Find an open cell in this column with solid charred rock beneath —
            // a floor to stand on — scanning from the chunk's bottom upward.
            let mut placed = None;
            for ty in (chunk_top..chunk_bottom).rev() {
                if !world.solid(cell_x, ty) && world.solid(cell_x, ty + 1) {
                    placed = Some((ty + 1) as f32 * TILE_SIZE - h);
                    break;
                }
            }
            let Some(y) = placed else { continue };
            let id = shared.alloc_id();
            let entity = Entity::new(id, EntityKind::Orc, cell_x as f32 * TILE_SIZE, y);
            entities.insert(entity.clone());
            spawned.push(entity);
        }
    }
    drop(world);

    for entity in spawned {
        shared.broadcast_dim(Dimension::Underworld, ServerMessage::EntitySpawn { entity });
    }
}

/// Possibly seed an orc mage into a freshly generated underworld chunk. Like the
/// demons it shepherds it keeps to the main **charred** expanse (ceding the ash
/// valleys to the twisters and charred skeletons), dropping onto a charred-rock floor
/// (open cell above, solid below). Rarer than the demons themselves. A seeded mage
/// often arrives having already empowered a demon offscreen, in which case a partner
/// [`EntityKind::EnchantedDemon`] spawns alongside it on a nearby floor — such demons
/// never spawn on their own. Deterministic per chunk via [`chunk_hash`] on its own
/// salt range, so re-exploring the same terrain never double-spawns, and it runs
/// independently of the other underworld spawners.
fn maybe_spawn_orc_mages(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Underworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 230) % 100 >= ORC_MAGE_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;

    // Place the mage: pick a charred-rock floor in its column (open cell above, solid
    // below), scanning from the chunk's bottom upward. Bail if its column is an ash
    // valley or has no floor.
    let lx = chunk_hash(seed, cx, cy, 231) % CHUNK_SIZE as u32;
    let mage_x = base_x + lx as i32;
    if world.generator.underworld_biome_at(mage_x) != crate::worldgen::UnderworldBiome::Charred {
        return;
    }
    let (_, mage_h) = EntityKind::OrcMage.size();
    let mut mage_y = None;
    for ty in (chunk_top..chunk_bottom).rev() {
        if !world.solid(mage_x, ty) && world.solid(mage_x, ty + 1) {
            mage_y = Some((ty + 1) as f32 * TILE_SIZE - mage_h);
            break;
        }
    }
    let Some(mage_y) = mage_y else { return };

    // The mage often arrives having already empowered a demon: try to place a partner
    // enchanted demon on a charred floor a few columns over.
    let mut partner = None;
    if chunk_hash(seed, cx, cy, 232) % 100 < ORC_MAGE_PREENCHANTED_CHANCE {
        let dlx = chunk_hash(seed, cx, cy, 233) % CHUNK_SIZE as u32;
        let demon_x = base_x + dlx as i32;
        if world.generator.underworld_biome_at(demon_x) == crate::worldgen::UnderworldBiome::Charred
        {
            let (_, dh) = EntityKind::EnchantedDemon.size();
            for ty in (chunk_top..chunk_bottom).rev() {
                if !world.solid(demon_x, ty) && world.solid(demon_x, ty + 1) {
                    partner = Some((demon_x, (ty + 1) as f32 * TILE_SIZE - dh));
                    break;
                }
            }
        }
    }

    let mut spawned = Vec::new();
    {
        let mut entities = shared.entities(Dimension::Underworld).lock();
        let id = shared.alloc_id();
        let mage = Entity::new(id, EntityKind::OrcMage, mage_x as f32 * TILE_SIZE, mage_y);
        entities.insert(mage.clone());
        spawned.push(mage);
        if let Some((demon_x, demon_y)) = partner {
            let did = shared.alloc_id();
            let demon = Entity::new(
                did,
                EntityKind::EnchantedDemon,
                demon_x as f32 * TILE_SIZE,
                demon_y,
            );
            entities.insert(demon.clone());
            spawned.push(demon);
        }
    }
    drop(world);

    for entity in spawned {
        shared.broadcast_dim(Dimension::Underworld, ServerMessage::EntitySpawn { entity });
    }
}

/// Possibly seed a dragon — the underworld's rare flying miniboss — into a freshly
/// generated underworld chunk. Dragons keep to the main **charred** expanse (never
/// the ash valleys) and are *extremely* rare: only a few chunks in a thousand seed
/// one. Crucially, a dragon is placed only where there's a tall open cavern above the
/// floor (at least [`DRAGON_SPAWN_CLEARANCE`] open cells), and it spawns hovering
/// [`DRAGON_SPAWN_HOVER`] above that floor — so it is always aloft and in plain sight,
/// never walled into rock where the player would hear its music without seeing it.
/// Deterministic per chunk via [`chunk_hash`] on its own salt range, so re-exploring
/// the same terrain never double-spawns, and it runs independently of the other
/// underworld spawners.
fn maybe_spawn_dragon(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Underworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 240) % 1000 >= DRAGON_CHUNK_PERMILLE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Dragon.size();

    let lx = chunk_hash(seed, cx, cy, 241) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    // Dragons keep to the main charred expanse — bail if this column is an ash valley.
    if world.generator.underworld_biome_at(cell_x) != crate::worldgen::UnderworldBiome::Charred {
        return;
    }
    // Find a floor (open cell above solid rock) that has a tall open cavern over it,
    // scanning from the chunk's bottom upward. Only such an airy spot will do, so the
    // dragon flies in the open rather than being buried in a cramped tunnel.
    let mut floor_ty = None;
    for ty in (chunk_top..chunk_bottom).rev() {
        if !world.solid(cell_x, ty) && world.solid(cell_x, ty + 1) {
            // Count the open cells stacked directly above this floor.
            let clear = (1..=DRAGON_SPAWN_CLEARANCE)
                .take_while(|&n| !world.solid(cell_x, ty - n + 1))
                .count() as i32;
            if clear >= DRAGON_SPAWN_CLEARANCE {
                floor_ty = Some(ty);
                break;
            }
        }
    }
    let Some(ty) = floor_ty else { return };
    // Hover above the floor, in the open air of the cavern we just cleared.
    let y = (ty + 1) as f32 * TILE_SIZE - h - DRAGON_SPAWN_HOVER;

    let id = shared.alloc_id();
    let entity = Entity::new(id, EntityKind::Dragon, cell_x as f32 * TILE_SIZE, y);
    shared
        .entities(Dimension::Underworld)
        .lock()
        .insert(entity.clone());
    drop(world);

    shared.broadcast_dim(Dimension::Underworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed a minotaur — the underworld's rare horned miniboss — into a freshly
/// generated underworld chunk. Like the dragon it keeps to the main **charred** expanse
/// (never the ash valleys) and is *extremely* rare: only about a dozen chunks in a
/// thousand seed one. Being a **ground** brute (not a flier), it is placed standing on a
/// charred-rock floor that has some open headroom above it (so it isn't wedged into a
/// cramped tunnel and has room for its great height and its jump slam). Deterministic per
/// chunk via [`chunk_hash`] on its own salt range, so re-exploring the same terrain never
/// double-spawns, and it runs independently of the other underworld spawners.
fn maybe_spawn_minotaur(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Underworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 250) % 1000 >= MINOTAUR_CHUNK_PERMILLE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Minotaur.size();

    let lx = chunk_hash(seed, cx, cy, 251) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    // Minotaurs keep to the main charred expanse — bail if this column is an ash valley.
    if world.generator.underworld_biome_at(cell_x) != crate::worldgen::UnderworldBiome::Charred {
        return;
    }
    // Find a floor (open cell above solid rock) with open headroom above it, scanning
    // from the chunk's bottom upward, so the brute stands in a roomy cavern.
    let mut placed = None;
    for ty in (chunk_top..chunk_bottom).rev() {
        if !world.solid(cell_x, ty) && world.solid(cell_x, ty + 1) {
            let clear = (1..=MINOTAUR_SPAWN_CLEARANCE)
                .take_while(|&n| !world.solid(cell_x, ty - n + 1))
                .count() as i32;
            if clear >= MINOTAUR_SPAWN_CLEARANCE {
                placed = Some((ty + 1) as f32 * TILE_SIZE - h);
                break;
            }
        }
    }
    let Some(y) = placed else { return };

    let id = shared.alloc_id();
    let entity = Entity::new(id, EntityKind::Minotaur, cell_x as f32 * TILE_SIZE, y);
    shared
        .entities(Dimension::Underworld)
        .lock()
        .insert(entity.clone());
    drop(world);

    shared.broadcast_dim(Dimension::Underworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed ash twisters into a freshly generated underworld chunk. Unlike the
/// other underworld monsters they are native to the **ash valleys** alone, so a
/// candidate spawn column is skipped unless it belongs to one (see
/// [`crate::worldgen::WorldGen::underworld_biome_at`]). They drop onto any charred
/// (or ash) floor — open cell above, solid below — the chunk contains. Deterministic
/// per chunk via [`chunk_hash`] on its own salt range, so re-exploring the same
/// terrain never double-spawns, and it runs independently of the other spawners.
fn maybe_spawn_ash_twisters(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Underworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 230) % 100 >= ASH_TWISTER_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let count = 1 + chunk_hash(seed, cx, cy, 231) % ASH_TWISTER_CHUNK_MAX;
    let (_, h) = EntityKind::AshTwister.size();

    let mut spawned = Vec::new();
    {
        let mut entities = shared.entities(Dimension::Underworld).lock();
        for n in 0..count {
            let lx = chunk_hash(seed, cx, cy, 232 + n) % CHUNK_SIZE as u32;
            let cell_x = base_x + lx as i32;
            // Twisters only haunt the ash valleys — skip a column that isn't one.
            if world.generator.underworld_biome_at(cell_x)
                != crate::worldgen::UnderworldBiome::AshValley
            {
                continue;
            }
            // Find an open cell in this column with a solid floor beneath, scanning
            // from the chunk's bottom upward.
            let mut placed = None;
            for ty in (chunk_top..chunk_bottom).rev() {
                if !world.solid(cell_x, ty) && world.solid(cell_x, ty + 1) {
                    placed = Some((ty + 1) as f32 * TILE_SIZE - h);
                    break;
                }
            }
            let Some(y) = placed else { continue };
            let id = shared.alloc_id();
            let entity = Entity::new(id, EntityKind::AshTwister, cell_x as f32 * TILE_SIZE, y);
            entities.insert(entity.clone());
            spawned.push(entity);
        }
    }
    drop(world);

    for entity in spawned {
        shared.broadcast_dim(Dimension::Underworld, ServerMessage::EntitySpawn { entity });
    }
}

/// Possibly seed a necromancer into a freshly generated **ash valley** (underworld)
/// chunk, like the ash twister — skipping any column that isn't an ash valley, and
/// dropping onto the first floor (open cell above, solid below) the column holds.
/// Deterministic per chunk via [`chunk_hash`] on its own salt range. The underworld is
/// always dark, so these necromancers roam around the clock.
fn maybe_spawn_necromancers_ash(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Underworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 430) % 100 >= NECROMANCER_ASH_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let (_, h) = EntityKind::Necromancer.size();

    let lx = chunk_hash(seed, cx, cy, 431) % CHUNK_SIZE as u32;
    let cell_x = base_x + lx as i32;
    if world.generator.underworld_biome_at(cell_x) != crate::worldgen::UnderworldBiome::AshValley {
        return;
    }
    let mut placed = None;
    for ty in (chunk_top..chunk_bottom).rev() {
        if !world.solid(cell_x, ty) && world.solid(cell_x, ty + 1) {
            placed = Some((ty + 1) as f32 * TILE_SIZE - h);
            break;
        }
    }
    let Some(y) = placed else { return };

    let id = shared.alloc_id();
    let entity = Entity::new(id, EntityKind::Necromancer, cell_x as f32 * TILE_SIZE, y);
    shared
        .entities(Dimension::Underworld)
        .lock()
        .insert(entity.clone());
    drop(world);

    shared.broadcast_dim(Dimension::Underworld, ServerMessage::EntitySpawn { entity });
}

/// Possibly seed spiders into a freshly generated chunk. Spiders keep to two
/// haunts: the tree-shadowed **forest** surface and the **caverns** deep
/// underground. A forest chunk that holds the grass line drops them onto the
/// ground like the other surface critters; any chunk below [`SPIDER_CAVERN_MIN_Y`]
/// drops them into a carved-out pocket with a solid floor. The per-chunk decision
/// is deterministic via [`chunk_hash`] on its own salt range, so exploring the
/// same terrain never double-spawns, and it runs independently of
/// [`maybe_spawn_in_chunk`].
fn maybe_spawn_spiders(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world(Dimension::Overworld).lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 100) % 100 >= SPIDER_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let count = 1 + chunk_hash(seed, cx, cy, 101) % SPIDER_CHUNK_MAX;
    let (_, sh) = EntityKind::Spider.size();

    let mut spawned = Vec::new();
    {
        let mut entities = shared.entities(Dimension::Overworld).lock();
        for n in 0..count {
            // Scatter spawns across the chunk's columns.
            let lx = chunk_hash(seed, cx, cy, 102 + n) % CHUNK_SIZE as u32;
            let cell_x = base_x + lx as i32;
            let surface = world.surface(cell_x);

            // Forest surface: settle a spider on the grass, but only in the chunk
            // that actually holds this column's surface line.
            let forest_surface = world.biome(cell_x) == Biome::Forest
                && surface >= chunk_top
                && surface < chunk_bottom;
            let pos = if forest_surface {
                Some((cell_x as f32 * TILE_SIZE, surface as f32 * TILE_SIZE - sh))
            } else if chunk_bottom > SPIDER_CAVERN_MIN_Y {
                // Deep underground: look for an open cavern floor in this column.
                cavern_floor_y(&mut world, cell_x, chunk_top, chunk_bottom, sh)
                    .map(|y| (cell_x as f32 * TILE_SIZE, y))
            } else {
                None
            };
            let Some((x, y)) = pos else { continue };

            let id = shared.alloc_id();
            let entity = Entity::new(id, EntityKind::Spider, x, y);
            entities.insert(entity.clone());
            spawned.push(entity);
        }
    }
    drop(world);

    for entity in spawned {
        shared.broadcast_dim(Dimension::Overworld, ServerMessage::EntitySpawn { entity });
    }
}

/// Find a spawn y (top-left px) for a spider standing on a cavern floor within
/// column `cell_x`, searching only rows `[chunk_top, chunk_bottom)` that lie at
/// or below [`SPIDER_CAVERN_MIN_Y`]. A valid spot is an air cell with headroom
/// above and solid rock directly beneath — the floor of an open pocket — so a
/// spider never spawns embedded in stone. Returns `None` if the column's slice
/// holds no such pocket.
fn cavern_floor_y(
    world: &mut ServerWorld,
    cell_x: i32,
    chunk_top: i32,
    chunk_bottom: i32,
    sh: f32,
) -> Option<f32> {
    // Scan from the chunk's floor upward; stop once rows climb above cavern depth.
    for ty in (chunk_top..chunk_bottom).rev() {
        if ty < SPIDER_CAVERN_MIN_Y {
            break;
        }
        if !world.solid(cell_x, ty) && !world.solid(cell_x, ty - 1) && world.solid(cell_x, ty + 1) {
            // Rest the spider's feet on that floor cell.
            return Some((ty + 1) as f32 * TILE_SIZE - sh);
        }
    }
    None
}

/// Advance a small xorshift RNG state and return the new value. Used to scatter
/// night-zombie spawns; determinism isn't important here, only cheap variety.
fn next_rng(state: &mut u32) {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
}

/// At night, try to spawn a zombie just off-screen from a random player, up to a
/// per-player cap. Zombies appear in any biome — they belong to the dark, not to
/// the terrain — so unlike the biome critters this ignores the column's biome.
/// Does nothing during the day or when no players are connected.
fn maybe_spawn_zombies(shared: &Shared, rng: &mut u32) {
    if !daylight::is_night(shared.time_of_day()) {
        return;
    }

    // Zombies and skeletons belong to the overworld night. world then entities,
    // matching the lock order used elsewhere.
    let world = shared.world(Dimension::Overworld).lock();
    let mut entities = shared.entities(Dimension::Overworld).lock();

    let players: Vec<(f32, f32)> = entities
        .values()
        .filter(|e| e.kind.is_player())
        .map(|e| (e.x, e.y))
        .collect();
    if players.is_empty() {
        return;
    }
    // Skeletons, desert necromancers and dark knights share the zombie's nightly
    // budget, so the night mobs together stay capped per player rather than each
    // filling the cap on their own.
    let undead = entities
        .values()
        .filter(|e| {
            matches!(
                e.kind,
                EntityKind::Zombie
                    | EntityKind::Skeleton
                    | EntityKind::Necromancer
                    | EntityKind::DarkKnight
            )
        })
        .count();
    if undead >= players.len() * ZOMBIE_MAX_PER_PLAYER {
        return;
    }

    // Pick a player, a side, and a distance to drop the mob at.
    next_rng(rng);
    let (px, py) = players[(*rng as usize) % players.len()];
    next_rng(rng);
    let side = if *rng & 1 == 0 { -1.0 } else { 1.0 };
    next_rng(rng);
    let span = (ZOMBIE_SPAWN_MAX_DIST - ZOMBIE_SPAWN_MIN_DIST) as u32;
    let dist = ZOMBIE_SPAWN_MIN_DIST + (*rng % span.max(1)) as f32;
    let cell_x = ((px + side * dist) / TILE_SIZE).floor() as i32;
    let surface = world.surface(cell_x);

    // Pick what arrives. A rare roll (in any biome) raises a dark knight; otherwise the
    // desert raises necromancers, and elsewhere (and on the rest of the desert rolls)
    // the night yields ranged skeletons or plain zombies. The dark-knight roll is its
    // own draw so it doesn't perturb the existing necromancer/skeleton/zombie split.
    next_rng(rng);
    let dark_knight = *rng % 100 < DARK_KNIGHT_NIGHT_PERCENT;
    next_rng(rng);
    let kind = if dark_knight {
        EntityKind::DarkKnight
    } else if world.biome(cell_x) == Biome::Desert && *rng % 100 < NECROMANCER_NIGHT_PERCENT {
        EntityKind::Necromancer
    } else if *rng % 100 < SKELETON_SPAWN_PERCENT {
        EntityKind::Skeleton
    } else {
        EntityKind::Zombie
    };
    let (_, h) = kind.size();
    let x = cell_x as f32 * TILE_SIZE;
    let y = surface as f32 * TILE_SIZE - h;
    // Skip spawns that land jarringly far above the player (e.g. across a deep
    // ravine), so a zombie never materializes hanging in open sky beside them.
    if (y - py).abs() > 400.0 {
        return;
    }

    let id = shared.alloc_id();
    let mob = Entity::new(id, kind, x, y);
    entities.insert(mob.clone());
    drop(entities);
    drop(world);
    shared.broadcast_dim(
        Dimension::Overworld,
        ServerMessage::EntitySpawn { entity: mob },
    );
}

/// Raise the arena's lone [`EntityKind::DemonKing`] when the time is right: the
/// boss hasn't already been slain in this world, none currently reigns, and a
/// player is present in the arena to fight it. The king appears across the arena
/// from a player, hovering above the stone-brick floor, flanked by
/// [`DEMON_KING_GUARD_COUNT`] **orc-mage guardians** scattered across the floor —
/// the king is invulnerable until every last one is cut down. A no-op otherwise, so
/// the arena stays empty until a challenger arrives — and stays empty forever once
/// the king has fallen (one boss per world).
fn ensure_arena_boss(shared: &Shared) {
    if shared.demon_king_slain.load(Ordering::SeqCst)
        || shared.demon_king_alive.load(Ordering::SeqCst)
    {
        return;
    }
    let dim = Dimension::Arena;
    // world then entities, matching the lock order step_entities uses.
    let spawned: Vec<Entity> = {
        let world = shared.world(dim).lock();
        let mut entities = shared.entities(dim).lock();
        // A king restored from a save (mid-fight) means none need be raised; just
        // bring the runtime mirror into agreement.
        if let Some(king) = entities
            .values()
            .find(|e| matches!(e.kind, EntityKind::DemonKing))
        {
            shared.demon_king_alive.store(true, Ordering::SeqCst);
            // The "already summoned its host" latch isn't persisted, so re-derive it
            // from the restored king's health: a king at or below the two-thirds
            // enrage threshold has already called down its dark knights. Without this
            // it would re-summon a fresh host of four every reload while enraged.
            let already_enraged = king.health * 3 <= king.max_health * 2;
            shared
                .demon_king_knights_summoned
                .store(already_enraged, Ordering::SeqCst);
            Vec::new()
        } else {
            // Stage the fight only once a challenger has walked out of the entrance
            // hall and into the room — so the long hall is a calm approach and the
            // king never charges down it toward a hall-bound player.
            let challenger_in_room = entities
                .values()
                .any(|e| e.kind.is_player() && e.x >= arena_room_left_x());
            if !challenger_in_room {
                Vec::new()
            } else {
                let mut spawned = Vec::new();
                // The fight is staged in the arena **room**, not the hall the
                // challenger enters through, so the king and its guardians are
                // raised around the room's center — wherever the challenger is.
                let anchor_x = arena_room_center_x();
                // The king itself, standing on the floor deeper in the room (it
                // begins the fight on foot, taking wing only later), so the
                // challenger crosses the room to reach it.
                let (_, kh) = EntityKind::DemonKing.size();
                let bx = anchor_x + DEMON_KING_SPAWN_DIST;
                let cell_x = (bx / TILE_SIZE).floor() as i32;
                let surface = world.surface(cell_x);
                let by = surface as f32 * TILE_SIZE - kh;
                let king = Entity::new(shared.alloc_id(), EntityKind::DemonKing, bx, by);
                entities.insert(king.clone());
                spawned.push(king);
                // A fresh king hasn't yet called its host of dark knights.
                shared
                    .demon_king_knights_summoned
                    .store(false, Ordering::SeqCst);

                // The orc-mage guardians, scattered at intervals across the room
                // floor on both sides of its center so they fan out across the field.
                let (_, mh) = EntityKind::OrcMage.size();
                for i in 0..DEMON_KING_GUARD_COUNT {
                    // Offsets stride out alternately left and right of the room
                    // center, each a little farther, so the mages spread across it.
                    let rank = (i / 2) as f32 + 1.0;
                    let dir = if i % 2 == 0 { -1.0 } else { 1.0 };
                    let gx = anchor_x + dir * rank * DEMON_KING_GUARD_SPACING;
                    let gcx = (gx / TILE_SIZE).floor() as i32;
                    let gsurface = world.surface(gcx);
                    let gy = gsurface as f32 * TILE_SIZE - mh;
                    let mage = Entity::new(shared.alloc_id(), EntityKind::OrcMage, gx, gy);
                    entities.insert(mage.clone());
                    spawned.push(mage);
                }
                shared.demon_king_alive.store(true, Ordering::SeqCst);
                spawned
            }
        }
    };
    // A non-empty `spawned` means a fresh king was just raised (the restored-from-save
    // and no-challenger cases both yield none) — so the boss greets the challenger with
    // its taunt, once, as the fight begins.
    let raised = !spawned.is_empty();
    for entity in spawned {
        shared.broadcast_dim(dim, ServerMessage::EntitySpawn { entity });
    }
    if raised {
        shared.broadcast_dim(
            dim,
            ServerMessage::Chat {
                from: DEMON_KING_CHAT_FROM.to_string(),
                text: DEMON_KING_TAUNT.to_string(),
            },
        );
    }
}

/// Raise the arena's post-game [`EntityKind::Twinscale`] superboss when the time is
/// right: the demon king has been slain, [`TWINSCALE_DELAY_SECS`] (five days) have
/// passed since, no Twinscale yet reigns (or has been slain), and a challenger stands
/// in the arena room. Twinscale appears high beneath the room's tall ceiling, out of
/// reach of ground attacks — so a challenger needs the
/// [`dragonian steed`](crate::block::DRAGONIAN_STEED) to fly up and fight it. A no-op
/// otherwise; like the king, exactly one is ever raised per world.
fn ensure_arena_twinscale(shared: &Shared) {
    if shared.twinscale_slain.load(Ordering::SeqCst)
        || shared.twinscale_alive.load(Ordering::SeqCst)
        || !shared.demon_king_slain.load(Ordering::SeqCst)
    {
        return;
    }
    // Five days must have passed in world-time since the king fell.
    let due = match *shared.demon_king_slain_at.lock() {
        Some(t0) => shared.start.lock().elapsed().as_secs_f32() - t0 >= TWINSCALE_DELAY_SECS,
        None => false,
    };
    if !due {
        return;
    }

    let dim = Dimension::Arena;
    let spawned: Option<Entity> = {
        let mut entities = shared.entities(dim).lock();
        // A Twinscale restored from a save (mid-fight): just bring the mirror into
        // agreement and re-derive its dragon-summon latch from its health.
        if let Some(ts) = entities
            .values()
            .find(|e| matches!(e.kind, EntityKind::Twinscale))
        {
            shared.twinscale_alive.store(true, Ordering::SeqCst);
            let already_summoned = ts.health * 2 <= ts.max_health;
            shared
                .twinscale_dragons_summoned
                .store(already_summoned, Ordering::SeqCst);
            None
        } else {
            // Raise it only once a challenger is in the room, like the king.
            let challenger_in_room = entities
                .values()
                .any(|e| e.kind.is_player() && e.x >= arena_room_left_x());
            if !challenger_in_room {
                None
            } else {
                let (tw, _th) = EntityKind::Twinscale.size();
                let cx = arena_room_center_x() + TWINSCALE_SPAWN_OFFSET;
                // Hover high, just below the room's open ceiling.
                let ty = arena_room_ceiling_y() + TWINSCALE_HOVER_DROP;
                let bx = cx - tw * 0.5;
                let twinscale = Entity::new(shared.alloc_id(), EntityKind::Twinscale, bx, ty);
                entities.insert(twinscale.clone());
                shared.twinscale_alive.store(true, Ordering::SeqCst);
                shared
                    .twinscale_dragons_summoned
                    .store(false, Ordering::SeqCst);
                Some(twinscale)
            }
        }
    };
    if let Some(entity) = spawned {
        shared.broadcast_dim(dim, ServerMessage::EntitySpawn { entity });
        shared.broadcast_dim(
            dim,
            ServerMessage::Chat {
                from: TWINSCALE_CHAT_FROM.to_string(),
                text: TWINSCALE_TAUNT.to_string(),
            },
        );
    }
}

/// Spawn a dropped-block item at the center of cell `(cell_x, cell_y)` in `dim`,
/// popping it upward so it clears the player who mined it, and announce it to that
/// dimension. Mined/crafted drops are a single item at full durability.
fn spawn_drop(shared: &Shared, dim: Dimension, cell_x: i32, cell_y: i32, block: BlockId) {
    let (iw, ih) = ITEM_SIZE;
    let x = cell_x as f32 * TILE_SIZE + (TILE_SIZE - iw) * 0.5;
    let y = cell_y as f32 * TILE_SIZE + (TILE_SIZE - ih) * 0.5;
    spawn_item(
        shared,
        dim,
        block,
        1,
        crate::block::max_durability(block),
        x,
        y,
        0.0,
        ITEM_POP_VELOCITY,
        ITEM_PICKUP_DELAY,
    );
}

/// Spawn a dropped-item entity in `dim` carrying its full stack and durability,
/// with an initial velocity and a pickup delay, and announce it to that
/// dimension. The low-level primitive behind both mined drops and player drops.
#[allow(clippy::too_many_arguments)]
fn spawn_item(
    shared: &Shared,
    dim: Dimension,
    block: BlockId,
    count: u32,
    durability: u16,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    pickup_delay: f32,
) {
    if count == 0 {
        return;
    }
    let id = shared.alloc_id();
    let mut item = Entity::new(
        id,
        EntityKind::DroppedItem {
            block,
            count,
            durability,
        },
        x,
        y,
    );
    item.vx = vx;
    item.vy = vy;
    item.attack_cd = pickup_delay; // reused as the pickup-delay timer
    shared.entities(dim).lock().insert(item.clone());
    shared.broadcast_dim(dim, ServerMessage::EntitySpawn { entity: item });
}

/// Periodically simulates non-player entities, applies survival rules, and
/// broadcasts the results (motion, health, time of day, respawns).
async fn entity_tick_loop(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(Duration::from_secs_f32(TICK_DT));
    let mut since_time_bcast = 0.0f32;
    let mut since_save = 0.0f32;
    let mut since_zombie_spawn = 0.0f32;
    let mut since_boss_check = 0.0f32;
    let mut since_water_flow = 0.0f32;
    // Evolving RNG state for scattering night-zombie spawns.
    let mut zombie_rng = 0x9E37_79B9u32;
    loop {
        interval.tick().await;

        // Stop once the session is shutting down; the final save is written by
        // RunningServer::drop.
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }

        // Simulate each dimension independently, scoping its broadcasts to the
        // clients currently in it.
        for dim in Dimension::ALL {
            let Step {
                broadcasts,
                respawns,
                mut pickups,
            } = step_entities(&shared, dim);
            for msg in broadcasts {
                shared.broadcast_dim(dim, msg);
            }
            for info in respawns {
                shared.finish_respawn(info, dim);
            }
            // Items were credited during the step; push each collector a fresh
            // inventory snapshot (deduplicating repeat collectors).
            pickups.sort_unstable();
            pickups.dedup();
            for pid in pickups {
                shared.send_inventory(pid);
            }
        }

        // Burn down lit campfires, extinguishing any that have run out of fuel,
        // and gutter out spent charred-skeleton fire trails.
        shared.tick_campfires(TICK_DT);
        shared.tick_trail_fires(TICK_DT);

        // Creep spreading water one cell outward through loaded terrain, per
        // dimension.
        since_water_flow += TICK_DT;
        if since_water_flow >= WATER_FLOW_SECS {
            since_water_flow = 0.0;
            for dim in Dimension::ALL {
                for msg in shared.tick_water(dim) {
                    shared.broadcast_dim(dim, msg);
                }
            }
        }

        // After dark, periodically conjure zombies near the players.
        since_zombie_spawn += TICK_DT;
        if since_zombie_spawn >= ZOMBIE_SPAWN_INTERVAL {
            since_zombie_spawn = 0.0;
            maybe_spawn_zombies(&shared, &mut zombie_rng);
        }

        // Periodically raise the arena's lone boss, if a challenger is present and
        // it hasn't already been slain in this world.
        since_boss_check += TICK_DT;
        if since_boss_check >= BOSS_CHECK_INTERVAL {
            since_boss_check = 0.0;
            ensure_arena_boss(&shared);
            ensure_arena_twinscale(&shared);
        }

        // Keep every client's day/night clock in sync.
        since_time_bcast += TICK_DT;
        if since_time_bcast >= TIME_BROADCAST_SECS {
            since_time_bcast = 0.0;
            shared.broadcast_all(ServerMessage::TimeOfDay {
                t: shared.time_of_day(),
            });
        }

        // Periodically persist the world so progress survives a crash.
        since_save += TICK_DT;
        if since_save >= AUTOSAVE_SECS {
            since_save = 0.0;
            shared.save();
        }
    }
}

/// Outcome of one simulation tick (for one dimension): messages to broadcast to
/// that dimension, and per-player respawn outcomes to enact for their owners.
struct Step {
    broadcasts: Vec<ServerMessage>,
    respawns: Vec<RespawnInfo>,
    /// Players who collected an item this tick and so need a fresh inventory
    /// snapshot (sent after the entity locks are released). May contain repeats.
    pickups: Vec<EntityId>,
}

/// Advance every server-simulated entity in dimension `dim` by one tick. Players
/// are skipped for motion (they are authoritative on their owning client) but can
/// still be targeted and bitten by creatures and singed by fire.
///
/// Locks `dim`'s `world` then its `entities` for the whole step, matching the
/// order used elsewhere so the two can never deadlock.
fn step_entities(shared: &Shared, dim: Dimension) -> Step {
    // The underworld and the arena are both sunless — they count as night around the
    // clock, so creatures that hunt only after dark (slimes) stay aggressive and
    // undead that burn at daybreak (zombies, skeletons, necromancers, and the skulls
    // the demon king summons) never burn up there.
    let night = matches!(dim, Dimension::Underworld | Dimension::Arena)
        || daylight::is_night(shared.time_of_day());
    let mut world = shared.world(dim).lock();
    let mut entities = shared.entities(dim).lock();

    // Snapshot player positions up front so slime targeting doesn't fight the
    // borrow checker over a second mutable handle into the map.
    let players: Vec<(EntityId, f32, f32)> = entities
        .values()
        .filter(|e| e.kind.is_player())
        .map(|e| (e.id, e.x, e.y))
        .collect();

    // The subset hostile creatures may target: players not currently in creator
    // mode. Monsters never attack a creator, so they pick targets from this list
    // (spawning and despawning still consider every player, creators included).
    let hostile_players: Vec<(EntityId, f32, f32)> = {
        let creators = shared.creators.lock();
        players
            .iter()
            .copied()
            .filter(|(pid, _, _)| !creators.contains(pid))
            .collect()
    };

    // Named snapshot of this dimension's players (id + position keyed by name), so
    // a tamed cat can resolve its owner — stored by name, not by a volatile id —
    // to a live entity without re-locking the entities map mid-tick.
    let named_players: Vec<(String, EntityId, f32, f32)> = entities
        .values()
        .filter_map(|e| match &e.kind {
            EntityKind::Player { name } if !name.is_empty() => Some((name.clone(), e.id, e.x, e.y)),
            _ => None,
        })
        .collect();

    // Tick down each player's dragon plate ward; once its eight minutes are spent the
    // timer reaches zero and the player's defense falls back to ordinary armor (the
    // damage step reads this), until they recast to renew it. Also tick the player's
    // `attack_cd`, which paces their warded empty-hand fireball breath — players run no
    // per-kind AI step, so this is the one place it counts down for them.
    for e in entities.values_mut() {
        if e.kind.is_player() {
            if e.dragon_plate_timer > 0.0 {
                e.dragon_plate_timer = (e.dragon_plate_timer - TICK_DT).max(0.0);
            }
            e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        }
    }

    // Snapshot of which horse each mounted player is riding, with the rider's
    // position, so the horse AI can glue a ridden horse beneath its rider without
    // re-borrowing the entities map mid-loop. `(horse id, rider x, rider y)`.
    let ridden_horses: Vec<(EntityId, f32, f32)> = entities
        .values()
        .filter_map(|e| e.riding.map(|hid| (hid, e.x, e.y)))
        .collect();

    // Snapshot of which white-dragon steed each piloting player is remotely controlling,
    // with that player's movement intent and position, so the steed AI can fly it from
    // the player's input and confine it near them without re-borrowing the map mid-loop.
    // `(steed id, dx, dy, pilot x, pilot y)`.
    let controlled_dragons: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter_map(|e| {
            e.controlling
                .map(|did| (did, e.control_dx, e.control_dy, e.x, e.y))
        })
        .collect();

    // Cull any non-player entity that has drifted beyond DESPAWN_DIST of every
    // player, removing it before it is simulated this tick. This keeps the world
    // from accumulating creatures and stray items in terrain nobody is near
    // (and naturally caps the always-spawning hostiles). Skipped entirely when no
    // one is connected, so a quiet world keeps its inhabitants until a player
    // returns.
    let despawns: Vec<EntityId> = if players.is_empty() {
        Vec::new()
    } else {
        entities
            .values()
            // Players are authoritative on their clients; pets and knights never
            // despawn for distance (they teleport to their owner instead), and the
            // arena boss — together with its orc-mage guardians and the dark-knight /
            // dark-musketeer host it summons when enraged — are fixtures of the arena:
            // they stay put whether or not a player is nearby, so the guardians can't
            // drift off and lift the king's shield, and the summoned host can't be
            // culled while a player is briefly far away (e.g. after respawning at the
            // far mouth of the hall). Everything else is culled once it drifts beyond
            // DESPAWN_DIST of every player.
            .filter(|e| {
                !e.kind.is_player()
                    && !e.kind.is_pet()
                    && !e.kind.is_knight()
                    && !e.kind.is_musketeer()
                    && !e.kind.is_mage()
                    && !e.kind.is_white_dragon()
                    && !matches!(e.kind, EntityKind::DemonKing | EntityKind::Twinscale)
                    && !(dim == Dimension::Arena
                        && matches!(
                            e.kind,
                            EntityKind::OrcMage
                                | EntityKind::DarkKnight
                                | EntityKind::DarkMusketeer
                                | EntityKind::Dragon
                        ))
            })
            .filter(|e| {
                let (w, h) = e.size();
                nearest_player(&players, e.x + w * 0.5, e.y + h * 0.5, DESPAWN_DIST).is_none()
            })
            .map(|e| e.id)
            .collect()
    };
    for &id in &despawns {
        entities.remove(id);
    }

    let slime_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Slime))
        .map(|e| e.id)
        .collect();
    let chicken_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Chicken))
        .map(|e| e.id)
        .collect();
    let goat_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Goat))
        .map(|e| e.id)
        .collect();
    let cat_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| e.kind.is_cat())
        .map(|e| e.id)
        .collect();
    let puppy_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| e.kind.is_puppy())
        .map(|e| e.id)
        .collect();
    let horse_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| e.kind.is_horse())
        .map(|e| e.id)
        .collect();
    let zombie_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Zombie))
        .map(|e| e.id)
        .collect();
    let spider_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Spider))
        .map(|e| e.id)
        .collect();
    let snake_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Snake))
        .map(|e| e.id)
        .collect();
    let skeleton_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Skeleton))
        .map(|e| e.id)
        .collect();
    let charred_skeleton_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::CharredSkeleton))
        .map(|e| e.id)
        .collect();
    let demon_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Demon))
        .map(|e| e.id)
        .collect();
    let orc_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Orc))
        .map(|e| e.id)
        .collect();
    let minotaur_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Minotaur))
        .map(|e| e.id)
        .collect();
    let gargoyle_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Gargoyle))
        .map(|e| e.id)
        .collect();
    let demon_king_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::DemonKing))
        .map(|e| e.id)
        .collect();
    let twinscale_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Twinscale))
        .map(|e| e.id)
        .collect();
    let ash_twister_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::AshTwister))
        .map(|e| e.id)
        .collect();
    let enchanted_demon_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::EnchantedDemon))
        .map(|e| e.id)
        .collect();
    let dragon_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Dragon))
        .map(|e| e.id)
        .collect();
    // Friendly steeds and the friendly fireballs they breathe — the player-summoned
    // twins of the hostile dragon and its fireball (they help the caster, hunting
    // monsters rather than players).
    let white_dragon_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::WhiteDragon { .. }))
        .map(|e| e.id)
        .collect();
    let friendly_dragon_fireball_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::FriendlyDragonFireball))
        .map(|e| e.id)
        .collect();
    let orc_mage_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::OrcMage))
        .map(|e| e.id)
        .collect();
    let necromancer_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Necromancer))
        .map(|e| e.id)
        .collect();
    let skull_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Skull))
        .map(|e| e.id)
        .collect();
    let summoner_fireball_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::SummonerFireball))
        .map(|e| e.id)
        .collect();
    // The player-summoned counterparts: a friendly fireball that bursts into a
    // friendly skull, and the skull itself (which hunts monsters, not players).
    let friendly_fireball_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::FriendlySummonerFireball))
        .map(|e| e.id)
        .collect();
    let friendly_skull_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::FriendlySkull))
        .map(|e| e.id)
        .collect();
    let knight_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Knight { .. }))
        .map(|e| e.id)
        .collect();
    let mage_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Mage { .. }))
        .map(|e| e.id)
        .collect();
    let bone_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Bone))
        .map(|e| e.id)
        .collect();
    let dark_knight_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::DarkKnight))
        .map(|e| e.id)
        .collect();
    let axe_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Axe))
        .map(|e| e.id)
        .collect();
    let musketeer_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Musketeer { .. }))
        .map(|e| e.id)
        .collect();
    let dark_musketeer_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::DarkMusketeer))
        .map(|e| e.id)
        .collect();
    // Hostile bullets (a dark musketeer's shots) and the friendly bullets a musketeer
    // or a player's musket looses fly under their own loops below.
    let bullet_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Bullet))
        .map(|e| e.id)
        .collect();
    let friendly_bullet_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::FriendlyBullet))
        .map(|e| e.id)
        .collect();
    // Both ordinary and magic fireballs fly under one loop below; they differ only
    // in damage, reach and life (the magic bolt's longer life is baked into its
    // per-entity timer at launch).
    let fireball_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Fireball | EntityKind::MagicFireball))
        .map(|e| e.id)
        .collect();
    let item_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| e.kind.is_item())
        .map(|e| e.id)
        .collect();

    // The non-player prey monsters hunt, as `(id, x, y, w, h)`, so a hostile creature
    // can chase one the same way it chases a player. This is the **warriors** — knights
    // and musketeers, wild and recruited alike, which fight back — *and* the **farmers**,
    // friendly bystanders that merely flee (their blows on monsters never come; they just
    // make tempting quarry). Snapped once up front: anything that moves later this tick
    // (in its own loop) reads a tick stale to the monster loops that run after, which is
    // close enough for a chase heading and a reach check.
    let knight_boxes: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| e.kind.is_warrior() || e.kind.is_farmer())
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();

    let mut broadcasts = Vec::new();
    // Tell every client to drop the entities culled for distance above.
    for id in despawns {
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }
    let mut pickups: Vec<EntityId> = Vec::new();
    // Players a creature hit this tick; applied after the movement loop so we
    // never hold two mutable entity borrows at once. Each entry is the bitten
    // player, the knockback `(vx, vy)` to shove it away from the attacker, and
    // the damage dealt (slimes nip, zombies hit hard).
    let mut bites: Vec<(EntityId, (f32, f32), i32)> = Vec::new();
    // Prey (skeletons/chickens) a puppy bit this tick: the victim id, the knockback
    // to shove it away from the puppy, and the damage. Applied after the movement
    // loops alongside `bites` so we never hold two mutable entity borrows at once.
    let mut creature_hits: Vec<(EntityId, (f32, f32), i32)> = Vec::new();
    // Raw-meat drops a puppy ate this tick, removed from the world after the loop.
    let mut meat_eaten: Vec<EntityId> = Vec::new();
    // Damage a knight soaked this tick (enemy reprisal in melee, or fire): the knight
    // id and the raw damage. Applied after the loops; a mounted knight's horse absorbs
    // it first (see [`Entity::mount_health`]), an unmounted one takes it on the chin.
    let mut knight_hits: Vec<(EntityId, i32)> = Vec::new();
    // Cells where a chasing charred skeleton laid a tongue of fire this tick,
    // registered with the trail-fire tracker once the world lock is released.
    let mut new_trail_fires: Vec<(i32, i32)> = Vec::new();

    for id in slime_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);

        // At night, lock onto the nearest player within aggro range.
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = if night {
            nearest_prey(&hostile_players, &knight_boxes, scx, scy, SLIME_AGGRO)
        } else {
            None
        };
        let chasing = target.is_some();

        // Heading: toward the target when chasing, else wander within its home
        // range (turning back once it strays too far).
        let dir = match target {
            Some(p) if p.x + p.w * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };

        // A chasing slime commits to the chase (over ledges and cliffs); a
        // patrolling one negotiates one-block steps and turns back at walls and
        // deep drops.
        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            SLIME_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });

        // Bite the target if it is in reach and the slime is off cooldown.
        if let Some(p) = target {
            if e.attack_cd <= 0.0
                && aabb_gap(m.x, m.y, w, h, p.x, p.y, p.w, p.h) <= SLIME_ATTACK_RANGE
            {
                e.attack_cd = SLIME_ATTACK_INTERVAL;
                let dir = if p.x + p.w * 0.5 >= m.x + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                hit_prey(
                    &mut bites,
                    &mut knight_hits,
                    &p,
                    (dir * KNOCKBACK_X, -KNOCKBACK_Y),
                    SLIME_DAMAGE,
                );
            }
        }
    }

    // Spiders: fast, fragile predators that scuttle after any player on sight
    // and scale sheer walls to reach them. Unlike the night-bound zombie they
    // hunt around the clock — their caverns are always pitch dark and the forest
    // canopy keeps them bold by day.
    for id in spider_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, SPIDER_AGGRO);
        let chasing = target.is_some();
        let dir = match target {
            Some(p) if p.x + p.w * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };

        // A chasing spider climbs walls to reach its target; a wandering one
        // patrols its patch on the ground.
        let m = step_climber(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            SPIDER_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });

        if let Some(p) = target {
            if e.attack_cd <= 0.0
                && aabb_gap(m.x, m.y, w, h, p.x, p.y, p.w, p.h) <= SPIDER_ATTACK_RANGE
            {
                e.attack_cd = SPIDER_ATTACK_INTERVAL;
                let dir = if p.x + p.w * 0.5 >= m.x + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                hit_prey(
                    &mut bites,
                    &mut knight_hits,
                    &p,
                    (dir * KNOCKBACK_X, -KNOCKBACK_Y),
                    SPIDER_DAMAGE,
                );
            }
        }
    }

    // Snakes: desert ambushers that hunt on sight and attack in telegraphed
    // wind-up lunges. They slither into range, coil in place (a dodgeable
    // wind-up), then spring forward to bite.
    let mut snake_despawns: Vec<EntityId> = Vec::new();
    for id in snake_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Killed: writhe through the death animation in place (gravity still
        // settles it onto the ground), run out the death timer, then despawn.
        if e.dying > 0.0 {
            e.dying -= TICK_DT;
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, 0.0, 0.0, false);
            e.x = m.x;
            e.y = m.y;
            e.vx = 0.0;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: 0.0,
                vy: m.vy,
            });
            if e.dying <= 0.0 {
                snake_despawns.push(id);
            }
            continue;
        }

        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);

        // Mid-lunge: a committed strike that plays out over SNAKE_LUNGE_TIME. The
        // snake coils in place through the wind-up, then springs along the heading
        // it locked in. The bite is resolved once, as the wind-up gives way to the
        // strike, so a player who broke away during the telegraph escapes.
        if e.lunge > 0.0 {
            let was_winding = e.lunge > SNAKE_STRIKE_TIME;
            e.lunge = (e.lunge - TICK_DT).max(0.0);
            let striking = e.lunge <= SNAKE_STRIKE_TIME;
            let dir = e.lunge_dir;

            let m = step_ground(
                &mut world,
                (e.x, e.y, w, h),
                e.vy,
                if striking { dir } else { 0.0 },
                if striking { SNAKE_LUNGE_SPEED } else { 0.0 },
                true,
            );
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });

            // The strike connects on the tick the wind-up ends, if a player or
            // knight is within the (generous) lunge reach.
            if was_winding && striking {
                if let Some(p) = nearest_prey(
                    &hostile_players,
                    &knight_boxes,
                    m.x + w * 0.5,
                    m.y + h * 0.5,
                    f32::INFINITY,
                ) {
                    if aabb_gap(m.x, m.y, w, h, p.x, p.y, p.w, p.h) <= SNAKE_LUNGE_REACH {
                        let kdir = if p.x + p.w * 0.5 >= m.x + w * 0.5 {
                            1.0
                        } else {
                            -1.0
                        };
                        hit_prey(
                            &mut bites,
                            &mut knight_hits,
                            &p,
                            (kdir * KNOCKBACK_X, -KNOCKBACK_Y),
                            SNAKE_DAMAGE,
                        );
                    }
                }
            }
            continue;
        }

        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, SNAKE_AGGRO);

        // In range and off cooldown: commit to a wind-up lunge. Lock the heading
        // toward the quarry now and hold still — the strike springs this way even
        // if it sidesteps. Tell every client to play the strike animation.
        if let Some(p) = target {
            if e.attack_cd <= 0.0
                && aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h) <= SNAKE_LUNGE_RANGE
            {
                e.lunge = crate::entity::SNAKE_LUNGE_TIME;
                e.lunge_dir = if p.x + p.w * 0.5 >= scx { 1.0 } else { -1.0 };
                e.attack_cd = SNAKE_ATTACK_INTERVAL;
                e.vx = 0.0;
                broadcasts.push(ServerMessage::EntityLunging { id });
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: e.x,
                    y: e.y,
                    vx: 0.0,
                    vy: e.vy,
                });
                continue;
            }
        }

        // Otherwise slither toward the target, or wander its home patch.
        let chasing = target.is_some();
        let dir = match target {
            Some(p) if p.x + p.w * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };
        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            SNAKE_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }
    for id in snake_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Chickens: peck around peacefully, but bolt away from the nearest player
    // for a few seconds after being hit (the flee timer is set in apply_damage).
    for id in chicken_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.flee = (e.flee - TICK_DT).max(0.0);
        let fleeing = e.flee > 0.0;
        let scx = e.x + w * 0.5;
        let home = *e.home_x.get_or_insert(e.x);

        let dir = if fleeing {
            // Run away from the nearest player, but if an unclimbable wall blocks
            // that escape, veer the other way instead of running into it.
            let away = match nearest_player(&players, scx, e.y + h * 0.5, f32::INFINITY) {
                Some((_, px, _)) if px + PLAYER_SIZE.0 * 0.5 < scx => 1.0,
                Some(_) => -1.0,
                None if e.vx < 0.0 => -1.0,
                None => 1.0,
            };
            if blocked_ahead(&mut world, e.x, e.y, w, h, away) {
                -away
            } else {
                away
            }
        } else {
            wander_dir(scx, e.vx, home)
        };
        let speed = if fleeing {
            CHICKEN_FLEE_SPEED
        } else {
            CHICKEN_WANDER_SPEED
        };

        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, speed, fleeing);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }

    // Goats: calm grazers that simply amble around their home patch of mountain,
    // negotiating one-block steps and turning back at walls and ledges.
    for id in goat_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        let scx = e.x + w * 0.5;
        let home = *e.home_x.get_or_insert(e.x);
        let dir = wander_dir(scx, e.vx, home);
        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, GOAT_SPEED, false);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }

    // Cats: peaceable forest critters that simply pad around their home patch,
    // negotiating one-block steps and turning back at walls and ledges like a
    // goat. A *tamed* cat additionally keeps up with its owner: when the owner
    // strays beyond CAT_TELEPORT_DIST it warps to them (and re-anchors its home
    // there) instead of being left behind — cats never despawn, so this is how a
    // cat follows its player across the world.
    for id in cat_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // A cat told to sit stays where its owner left it: no wandering, no
        // follow-teleport. It still obeys gravity (dir 0) so it drops rather than
        // hangs if the ground beneath it is mined away.
        if e.kind.is_sitting() {
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, 0.0, CAT_SPEED, false);
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        // A tamed cat whose owner (resolved by name to a live player in this
        // dimension) has wandered too far teleports onto them. Cloned to an owned
        // name first so no borrow of `e` is held across the reposition below.
        let owner = e.kind.owner().map(str::to_owned);
        if let Some(owner) = &owner
            && let Some(&(_, _, ox, oy)) = named_players.iter().find(|(n, ..)| n == owner)
            && (((ox + PLAYER_SIZE.0 * 0.5) - scx).powi(2)
                + ((oy + PLAYER_SIZE.1 * 0.5) - scy).powi(2))
                > CAT_TELEPORT_DIST * CAT_TELEPORT_DIST
        {
            // Land at the owner's feet and treat that as its new home anchor so it
            // loiters around them rather than drifting back toward where it spawned.
            e.x = ox;
            e.y = oy;
            e.vx = 0.0;
            e.vy = 0.0;
            e.home_x = Some(ox);
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: ox,
                y: oy,
                vx: 0.0,
                vy: 0.0,
            });
            continue;
        }

        let home = *e.home_x.get_or_insert(e.x);
        let dir = wander_dir(scx, e.vx, home);
        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, CAT_SPEED, false);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }

    // Horses: tall plains grazers that amble around their home patch like a cat. A
    // *ridden* horse instead snaps beneath its rider each tick (the rider drives the
    // movement client-side; the horse is drawn as part of the combined sprite, so its
    // exact spot only matters at dismount). A tamed, unridden horse keeps up with its
    // owner, teleporting to them past HORSE_TELEPORT_DIST — horses, like the other
    // pets, never despawn for distance.
    for id in horse_ids {
        // A ridden horse: glue it centred beneath its rider, feet aligned, and skip
        // its own AI. (Found in the pre-loop snapshot of who's riding what.)
        if let Some(&(_, rx, ry)) = ridden_horses.iter().find(|(hid, ..)| *hid == id) {
            let Some(e) = entities.get_mut(id) else {
                continue;
            };
            let (w, h) = e.size();
            e.x = rx + (PLAYER_SIZE.0 - w) * 0.5;
            e.y = ry + PLAYER_SIZE.1 - h;
            e.vx = 0.0;
            e.vy = 0.0;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: e.x,
                y: e.y,
                vx: 0.0,
                vy: 0.0,
            });
            continue;
        }

        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        // A tamed horse whose owner has strayed too far teleports onto them and
        // re-anchors its home there, exactly like the cat.
        let owner = e.kind.owner().map(str::to_owned);
        if let Some(owner) = &owner
            && let Some(&(_, _, ox, oy)) = named_players.iter().find(|(n, ..)| n == owner)
            && (((ox + PLAYER_SIZE.0 * 0.5) - scx).powi(2)
                + ((oy + PLAYER_SIZE.1 * 0.5) - scy).powi(2))
                > HORSE_TELEPORT_DIST * HORSE_TELEPORT_DIST
        {
            e.x = ox;
            e.y = oy;
            e.vx = 0.0;
            e.vy = 0.0;
            e.home_x = Some(ox);
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: ox,
                y: oy,
                vx: 0.0,
                vy: 0.0,
            });
            continue;
        }

        let home = *e.home_x.get_or_insert(e.x);
        let dir = wander_dir(scx, e.vx, home);
        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, HORSE_SPEED, false);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }

    // Puppies: loyal little hunters that pad around their home patch like a cat,
    // but break off to chase down any nearby skeleton or chicken, biting it, then
    // trot over to any raw meat that falls and gobble it up. A *tamed* puppy also
    // keeps up with its owner, teleporting to them when they stray beyond
    // PUPPY_TELEPORT_DIST (puppies, like cats, never despawn for distance).
    //
    // Snapshot the prey it can hunt and the raw meat it can eat before the loop, so
    // it can pick a target without re-borrowing the entities map while one puppy is
    // mutably held. (Skeletons move later this tick, so their positions are a tick
    // stale — close enough for a chase heading.)
    let prey: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Chicken | EntityKind::Skeleton))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    let meat_items: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::DroppedItem { block, .. } if block == crate::block::RAW_MEAT))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    for id in puppy_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);

        // A puppy told to sit stays where its owner left it: no wandering, hunting
        // or follow-teleport. It still obeys gravity (dir 0) so it drops rather
        // than hangs if the ground beneath it is mined away.
        if e.kind.is_sitting() {
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, 0.0, PUPPY_SPEED, false);
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        // 1. Raw meat on the ground takes priority: trot to it and eat it (healing
        //    a little). Eat it outright once within reach.
        if let Some((mid, mx, my, mw, mh)) = nearest_of(&meat_items, scx, scy, PUPPY_AGGRO) {
            if aabb_gap(e.x, e.y, w, h, mx, my, mw, mh) <= PUPPY_EAT_REACH {
                meat_eaten.push(mid);
                e.health = (e.health + PUPPY_EAT_HEAL).min(e.max_health);
                e.vx = 0.0;
                broadcasts.push(ServerMessage::EntityHealth {
                    id,
                    health: e.health,
                    max_health: e.max_health,
                });
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: e.x,
                    y: e.y,
                    vx: 0.0,
                    vy: e.vy,
                });
                continue;
            }
            let dir = if mx + mw * 0.5 < scx { -1.0 } else { 1.0 };
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, PUPPY_SPEED, true);
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        // 2. Otherwise hunt the nearest skeleton or chicken: charge it and bite
        //    when in reach and off cooldown.
        if let Some((tid, tx, ty, tw, th)) = nearest_of(&prey, scx, scy, PUPPY_AGGRO) {
            if e.attack_cd <= 0.0 && aabb_gap(e.x, e.y, w, h, tx, ty, tw, th) <= PUPPY_ATTACK_RANGE
            {
                e.attack_cd = PUPPY_ATTACK_INTERVAL;
                let kdir = if tx + tw * 0.5 >= scx { 1.0 } else { -1.0 };
                creature_hits.push((tid, (kdir * KNOCKBACK_X, -KNOCKBACK_Y), PUPPY_DAMAGE));
            }
            let dir = if tx + tw * 0.5 < scx { -1.0 } else { 1.0 };
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, PUPPY_SPEED, true);
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        // 3. Nothing to hunt: a tamed puppy whose owner (resolved by name to a live
        //    player) has wandered too far teleports onto them, re-anchoring its home
        //    there — exactly like the cat.
        let owner = e.kind.owner().map(str::to_owned);
        if let Some(owner) = &owner
            && let Some(&(_, _, ox, oy)) = named_players.iter().find(|(n, ..)| n == owner)
            && (((ox + PLAYER_SIZE.0 * 0.5) - scx).powi(2)
                + ((oy + PLAYER_SIZE.1 * 0.5) - scy).powi(2))
                > PUPPY_TELEPORT_DIST * PUPPY_TELEPORT_DIST
        {
            e.x = ox;
            e.y = oy;
            e.vx = 0.0;
            e.vy = 0.0;
            e.home_x = Some(ox);
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: ox,
                y: oy,
                vx: 0.0,
                vy: 0.0,
            });
            continue;
        }

        // 4. Default: amble around the home patch like a cat.
        let home = *e.home_x.get_or_insert(e.x);
        let dir = wander_dir(scx, e.vx, home);
        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, PUPPY_SPEED, false);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }
    // Remove the raw-meat drops the puppies ate this tick.
    for mid in meat_eaten {
        if entities.remove(mid).is_some() {
            broadcasts.push(ServerMessage::EntityDespawn { id: mid });
        }
    }

    // Farmers: friendly plains folk that make their living off the land's livestock.
    // Each one flees the nearest monster if one is close (it's no fighter — monsters
    // hunt it via the `knight_boxes` prey list, and it answers only with its legs),
    // otherwise collects any raw meat that has dropped, otherwise hunts the nearest
    // chicken or goat (charging in to strike it like a puppy hunts), and failing all
    // that just ambles around its home patch like a goat.
    //
    // Snapshots taken before the loop so a farmer can pick a target without re-borrowing
    // the entities map while one farmer is mutably held. (The livestock and monsters move
    // later this tick, so their positions are a tick stale — close enough for a heading.)
    let farmer_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Farmer))
        .map(|e| e.id)
        .collect();
    let livestock: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Chicken | EntityKind::Goat))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    let farmer_monsters: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| is_hostile(&e.kind))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    let farmer_meat: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::DroppedItem { block, .. } if block == crate::block::RAW_MEAT))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    let mut farmer_meat_eaten: Vec<EntityId> = Vec::new();
    for id in farmer_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        // 1. Danger first: bolt away from the nearest monster within flee range, veering
        //    the other way if an unclimbable wall blocks that escape (like a chicken).
        if let Some((_, mx, _, mw, _)) = nearest_of(&farmer_monsters, scx, scy, FARMER_FLEE_RANGE) {
            let away = if mx + mw * 0.5 < scx { 1.0 } else { -1.0 };
            let dir = if blocked_ahead(&mut world, e.x, e.y, w, h, away) {
                -away
            } else {
                away
            };
            let m = step_ground(
                &mut world,
                (e.x, e.y, w, h),
                e.vy,
                dir,
                FARMER_FLEE_SPEED,
                true,
            );
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        // 2. Collect raw meat on the ground: trot to it and gather it up (eating it for
        //    its trouble, healing a little). Collected outright once within reach.
        if let Some((mid, mx, my, mw, mh)) = nearest_of(&farmer_meat, scx, scy, FARMER_AGGRO) {
            if aabb_gap(e.x, e.y, w, h, mx, my, mw, mh) <= FARMER_EAT_REACH {
                farmer_meat_eaten.push(mid);
                e.health = (e.health + FARMER_EAT_HEAL).min(e.max_health);
                e.vx = 0.0;
                broadcasts.push(ServerMessage::EntityHealth {
                    id,
                    health: e.health,
                    max_health: e.max_health,
                });
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: e.x,
                    y: e.y,
                    vx: 0.0,
                    vy: e.vy,
                });
                continue;
            }
            let dir = if mx + mw * 0.5 < scx { -1.0 } else { 1.0 };
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, FARMER_SPEED, true);
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        // 3. Hunt the nearest chicken or goat: charge it and strike when in reach and off
        //    cooldown (kicking off the swing animation on every client), felling it for
        //    the meat it leaves behind.
        if let Some((tid, tx, ty, tw, th)) = nearest_of(&livestock, scx, scy, FARMER_AGGRO) {
            if e.attack_cd <= 0.0 && aabb_gap(e.x, e.y, w, h, tx, ty, tw, th) <= FARMER_ATTACK_RANGE
            {
                e.attack_cd = FARMER_ATTACK_INTERVAL;
                let kdir = if tx + tw * 0.5 >= scx { 1.0 } else { -1.0 };
                creature_hits.push((tid, (kdir * KNOCKBACK_X, -KNOCKBACK_Y), FARMER_DAMAGE));
                broadcasts.push(ServerMessage::EntityLunging { id });
            }
            let dir = if tx + tw * 0.5 < scx { -1.0 } else { 1.0 };
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, FARMER_SPEED, true);
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        // 4. Nothing about: amble around the home patch like a goat.
        let home = *e.home_x.get_or_insert(e.x);
        let dir = wander_dir(scx, e.vx, home);
        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, FARMER_SPEED, false);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }
    // Remove the raw-meat drops the farmers collected this tick.
    for mid in farmer_meat_eaten {
        if entities.remove(mid).is_some() {
            broadcasts.push(ServerMessage::EntityDespawn { id: mid });
        }
    }

    // Knights: a man-at-arms — wild or recruited alike — fights monsters on sight.
    // A wild one roams its home patch but charges any hostile that strays within
    // aggro; a recruited one keeps to its owner (teleporting over when they stray,
    // like a pet) and charges the foe its owner last struck, or — failing that, like
    // a wild knight — the nearest hostile in range. If a wild horse is at hand it
    // mounts up (the horse soaks blows for it via `mount_health` until slain).
    // Monsters strike back from their own loops (a knight draws them like a player
    // does), so there is no reprisal to apply here.
    //
    // Snapshots taken before the loop so a knight can pick a horse to mount and an
    // enemy to chase without re-borrowing the entities map mid-loop.
    let knight_target_marks = shared.knight_targets.lock().clone();
    let hostiles: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| is_hostile(&e.kind))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    let wild_horses: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Horse { owner: None }))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    let mut claimed_horses: HashSet<EntityId> = HashSet::new();
    let mut horse_absorptions: Vec<EntityId> = Vec::new();
    for id in knight_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        // Singed by fire, on the shared interval — the mount soaks it if mounted,
        // else it bites into the knight's own health (applied after the loops).
        {
            let mut cds = shared.fire_cd.lock();
            let cd = cds.entry(id).or_insert(0.0);
            *cd = (*cd - TICK_DT).max(0.0);
            if *cd <= 0.0 && body_in_fire(&mut world, e.x, e.y, w, h) {
                *cd = FIRE_DAMAGE_INTERVAL;
                knight_hits.push((id, FIRE_DAMAGE));
            }
        }

        // Resolve a recruited knight's owner (by name) to a live player in this
        // dimension, as `(id, x, y)`. A wild knight has none; a recruited one whose
        // owner is offline or in another dimension resolves to `None` and fights and
        // roams on its own until they return (a dimension change brings the knight
        // along via `transfer_knights`).
        let owner: Option<(EntityId, f32, f32)> = e.kind.owner().and_then(|name| {
            named_players
                .iter()
                .find(|(n, ..)| n == name)
                .map(|&(_, oid, ox, oy)| (oid, ox, oy))
        });

        // Recruited & owner present and strayed too far: snap to them and re-anchor
        // home there, like a pet.
        if let Some((_, ox, oy)) = owner {
            let ocx = ox + PLAYER_SIZE.0 * 0.5;
            let ocy = oy + PLAYER_SIZE.1 * 0.5;
            if (ocx - scx).powi(2) + (ocy - scy).powi(2)
                > KNIGHT_TELEPORT_DIST * KNIGHT_TELEPORT_DIST
            {
                e.x = ox;
                e.y = oy;
                e.vx = 0.0;
                e.vy = 0.0;
                e.home_x = Some(ox);
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: ox,
                    y: oy,
                    vx: 0.0,
                    vy: 0.0,
                });
                continue;
            }
        }

        // On foot near a wild horse? A recruited knight mounts up: absorb the horse
        // (it's removed from the world) and ride behind its health until it's spent.
        // Tamed horses are spared, and a wild knight stays afoot (it keeps the plains'
        // horses intact until someone recruits it). A knight already mounted on spawn
        // keeps its steed regardless.
        let mut mounted = e.mount_health > 0;
        if !mounted && owner.is_some() {
            let mut best: Option<(EntityId, f32)> = None;
            for &(hid, hx, hy, hw, hh) in &wild_horses {
                if claimed_horses.contains(&hid) {
                    continue;
                }
                let gap = aabb_gap(e.x, e.y, w, h, hx, hy, hw, hh);
                if gap <= KNIGHT_MOUNT_RANGE && best.is_none_or(|(_, bg)| gap < bg) {
                    best = Some((hid, gap));
                }
            }
            if let Some((hid, _)) = best {
                claimed_horses.insert(hid);
                horse_absorptions.push(hid);
                e.mount_health = crate::entity::HORSE_MAX_HEALTH;
                e.riding = Some(hid);
                mounted = true;
                broadcasts.push(ServerMessage::EntityRiding {
                    id,
                    horse: Some(hid),
                });
            }
        }
        let speed = if mounted {
            KNIGHT_MOUNTED_SPEED
        } else {
            KNIGHT_SPEED
        };

        // Pick a quarry: a recruited knight prefers the foe its owner last struck (if
        // it still lives here and is within aggro — not chased clear across the map),
        // then — like a wild knight — falls back to the nearest hostile in aggro.
        let marked = owner
            .and_then(|(oid, _, _)| knight_target_marks.get(&oid))
            .and_then(|teid| hostiles.iter().find(|(hid, ..)| hid == teid).copied())
            .filter(|&(_, tx, ty, tw, th)| {
                aabb_gap(e.x, e.y, w, h, tx, ty, tw, th) <= KNIGHT_AGGRO
            });
        let target = marked.or_else(|| nearest_of(&hostiles, scx, scy, KNIGHT_AGGRO));
        if let Some((tid, tx, ty, tw, th)) = target {
            if e.attack_cd <= 0.0 && aabb_gap(e.x, e.y, w, h, tx, ty, tw, th) <= KNIGHT_ATTACK_RANGE
            {
                e.attack_cd = KNIGHT_ATTACK_INTERVAL;
                let kdir = if tx + tw * 0.5 >= scx { 1.0 } else { -1.0 };
                creature_hits.push((tid, (kdir * KNOCKBACK_X, -KNOCKBACK_Y), KNIGHT_DAMAGE));
                // Play the swing animation on every client.
                broadcasts.push(ServerMessage::EntityLunging { id });
            }
            let dir = if tx + tw * 0.5 < scx { -1.0 } else { 1.0 };
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, speed, true);
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        // No quarry in reach: a recruited knight heels to its owner (closing on them
        // if they've pulled ahead, else holding); a wild knight — or a recruited one
        // whose owner is away — ambles around its home patch.
        let dir = match owner {
            Some((_, ox, oy)) => {
                let ocx = ox + PLAYER_SIZE.0 * 0.5;
                let owner_gap = aabb_gap(e.x, e.y, w, h, ox, oy, PLAYER_SIZE.0, PLAYER_SIZE.1);
                if owner_gap <= KNIGHT_FOLLOW_GAP {
                    0.0
                } else if ocx < scx {
                    -1.0
                } else {
                    1.0
                }
            }
            None => {
                let home = *e.home_x.get_or_insert(e.x);
                wander_dir(scx, e.vx, home)
            }
        };
        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, speed, true);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }
    // Remove the wild horses any knight mounted (absorbed) this tick.
    for hid in horse_absorptions {
        if entities.remove(hid).is_some() {
            broadcasts.push(ServerMessage::EntityDespawn { id: hid });
        }
    }

    // Musketeers: marksmen — wild or recruited alike — that fight monsters at range. A
    // recruited one keeps to its owner (teleporting over when they stray, like a knight)
    // and a wild one roams its home patch, but both lock onto the nearest monster in
    // aggro and kite it: closing to firing range, backing off if it slips too near, and
    // loosing a friendly bullet on a measured cadence. Monsters strike back from their
    // own loops (a musketeer rides the same `knight_boxes` prey list a knight does), so
    // there is no reprisal to apply here. Friendly bullets are spawned after the loop so
    // we aren't holding a mutable borrow of the shooter while inserting.
    let mut friendly_bullet_spawns: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in musketeer_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        // Singed by fire on the shared interval, taken on its own health (applied after
        // the loops via `knight_hits`, the warriors' reprisal bucket).
        {
            let mut cds = shared.fire_cd.lock();
            let cd = cds.entry(id).or_insert(0.0);
            *cd = (*cd - TICK_DT).max(0.0);
            if *cd <= 0.0 && body_in_fire(&mut world, e.x, e.y, w, h) {
                *cd = FIRE_DAMAGE_INTERVAL;
                knight_hits.push((id, FIRE_DAMAGE));
            }
        }

        // Resolve a recruited musketeer's owner (by name) to a live player here.
        let owner: Option<(EntityId, f32, f32)> = e.kind.owner().and_then(|name| {
            named_players
                .iter()
                .find(|(n, ..)| n == name)
                .map(|&(_, oid, ox, oy)| (oid, ox, oy))
        });

        // Recruited & owner present and strayed too far: snap to them and re-anchor.
        if let Some((_, ox, oy)) = owner {
            let ocx = ox + PLAYER_SIZE.0 * 0.5;
            let ocy = oy + PLAYER_SIZE.1 * 0.5;
            if (ocx - scx).powi(2) + (ocy - scy).powi(2)
                > KNIGHT_TELEPORT_DIST * KNIGHT_TELEPORT_DIST
            {
                e.x = ox;
                e.y = oy;
                e.vx = 0.0;
                e.vy = 0.0;
                e.home_x = Some(ox);
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: ox,
                    y: oy,
                    vx: 0.0,
                    vy: 0.0,
                });
                continue;
            }
        }

        let home = *e.home_x.get_or_insert(e.x);
        let target = nearest_of(&hostiles, scx, scy, MUSKETEER_AGGRO);
        let chasing = target.is_some();

        // Kiting heading: close in when out of firing range, back off when the monster
        // slips inside the standoff distance, otherwise hold and fire. With no monster
        // to hunt, a recruited musketeer heels to its owner; a wild one ambles home.
        let (dir, gap, aim) = match target {
            Some((_, tx, ty, tw, th)) => {
                let gap = aabb_gap(e.x, e.y, w, h, tx, ty, tw, th);
                let toward = if tx + tw * 0.5 >= scx { 1.0 } else { -1.0 };
                let dir = if gap < MUSKETEER_KEEP_DIST {
                    -toward
                } else if gap > MUSKETEER_SHOOT_RANGE {
                    toward
                } else {
                    0.0
                };
                (dir, gap, Some((tx, ty, tw, th)))
            }
            None => {
                let dir = match owner {
                    Some((_, ox, oy)) => {
                        let ocx = ox + PLAYER_SIZE.0 * 0.5;
                        let owner_gap =
                            aabb_gap(e.x, e.y, w, h, ox, oy, PLAYER_SIZE.0, PLAYER_SIZE.1);
                        if owner_gap <= KNIGHT_FOLLOW_GAP {
                            0.0
                        } else if ocx < scx {
                            -1.0
                        } else {
                            1.0
                        }
                    }
                    None => wander_dir(scx, e.vx, home),
                };
                (dir, f32::INFINITY, None)
            }
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            MUSKETEER_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        // Face the target even while striding backwards to keep distance.
        let bcast_vx = match aim {
            Some((px, _, pw, _)) => {
                let cx = m.x + w * 0.5;
                let toward = if px + pw * 0.5 >= cx { 1.0 } else { -1.0 };
                toward * m.vx.abs()
            }
            None => m.vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: bcast_vx,
            vy: m.vy,
        });

        // Fire a friendly bullet when a monster is within range and we're off cooldown,
        // from the musketeer's upper body straight at the target's center.
        if let Some((px, py, pw, ph)) = aim {
            if e.attack_cd <= 0.0 && gap <= MUSKETEER_SHOOT_RANGE {
                e.attack_cd = MUSKETEER_SHOOT_INTERVAL;
                let (bw, bh) = crate::entity::BULLET_SIZE;
                let sx = m.x + w * 0.5 - bw * 0.5;
                let sy = m.y + h * 0.3 - bh * 0.5;
                let tx = px + pw * 0.5;
                let ty = py + ph * 0.5;
                let dx = tx - (sx + bw * 0.5);
                let dy = ty - (sy + bh * 0.5);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                friendly_bullet_spawns.push((
                    sx,
                    sy,
                    dx / len * BULLET_SPEED,
                    dy / len * BULLET_SPEED,
                ));
                broadcasts.push(ServerMessage::EntityLunging { id });
            }
        }
    }
    // Spawn the friendly bullets musketeers loosed this tick. They aren't in
    // `friendly_bullet_ids`, so they begin flying next tick rather than at once.
    for (x, y, vx, vy) in friendly_bullet_spawns {
        let bid = shared.alloc_id();
        let mut bullet = Entity::new(bid, EntityKind::FriendlyBullet, x, y);
        bullet.vx = vx;
        bullet.vy = vy;
        bullet.attack_cd = BULLET_LIFETIME; // reused as the airborne lifetime timer
        entities.insert(bullet.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: bullet });
    }

    // Mages: the spellcasters the restore spell conjures. A recruited mage shadows
    // its owner (teleporting over when they stray, like a knight); a wild one ambles
    // its home patch. On a measured cadence it looses one of the world's spells at
    // whatever's nearby — restoring a foe it can turn, bursting daylight-burning
    // undead, or summoning a friendly skull against any other monster. A mage it
    // restores is recruited to *its* owner (so a recruited mage builds its master a
    // retinue); a wild mage's restorations stay wild. Nothing attacks a mage, so
    // there is no combat to resolve here. Snapshots of restorable foes and
    // daylight-burners are taken now so a mage can choose a spell without re-borrowing
    // the entities map mid-loop.
    let restorable_boxes: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| is_restorable(&e.kind))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    let burner_boxes: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| burns_in_daylight(&e.kind))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    // Spells a mage loosed this tick, applied after the loop so we never hold two
    // mutable entity borrows at once.
    let mut mage_restores: Vec<(EntityId, Option<String>)> = Vec::new();
    let mut mage_sunbursts: Vec<(f32, f32)> = Vec::new(); // blast centers
    let mut mage_bolts: Vec<(f32, f32, f32, f32)> = Vec::new(); // friendly summoner bolts
    for id in mage_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        e.lunge = (e.lunge - TICK_DT).max(0.0);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let owner_name = e.kind.owner().map(str::to_owned);

        // Resolve a recruited mage's owner (by name) to a live player in this
        // dimension; a wild mage (or one whose owner is away) has none.
        let owner_pos: Option<(f32, f32)> = owner_name.as_deref().and_then(|name| {
            named_players
                .iter()
                .find(|(n, ..)| n == name)
                .map(|&(_, _, ox, oy)| (ox, oy))
        });

        // Recruited & owner present and strayed too far: snap to them, like a knight.
        if let Some((ox, oy)) = owner_pos {
            let ocx = ox + PLAYER_SIZE.0 * 0.5;
            let ocy = oy + PLAYER_SIZE.1 * 0.5;
            if (ocx - scx).powi(2) + (ocy - scy).powi(2) > MAGE_TELEPORT_DIST * MAGE_TELEPORT_DIST {
                e.x = ox;
                e.y = oy;
                e.vx = 0.0;
                e.vy = 0.0;
                e.home_x = Some(ox);
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: ox,
                    y: oy,
                    vx: 0.0,
                    vy: 0.0,
                });
                continue;
            }
        }

        // Loose a spell if the cast cooldown is up and something nearby calls for one.
        if e.attack_cd <= 0.0 {
            // 1. A foe it can restore (turn to its side) takes priority.
            let restore_target = nearest_of(&restorable_boxes, scx, scy, MAGE_CAST_RANGE);
            // 2. Else a cluster of daylight-burning undead to incinerate.
            let burner_target = nearest_of(&burner_boxes, scx, scy, SUNBURST_RADIUS);
            // 3. Else any other monster to fling a friendly skull at.
            let hostile_target = nearest_of(&hostiles, scx, scy, MAGE_CAST_RANGE);
            let cast =
                restore_target.is_some() || burner_target.is_some() || hostile_target.is_some();
            if cast {
                if let Some((tid, ..)) = restore_target {
                    mage_restores.push((tid, owner_name.clone()));
                } else if burner_target.is_some() {
                    mage_sunbursts.push((scx, scy));
                } else if let Some((_, hx, hy, hw, hh)) = hostile_target {
                    let (fw, fh) = crate::entity::FRIENDLY_SUMMONER_FIREBALL_SIZE;
                    let sx = scx - fw * 0.5;
                    let sy = e.y + h * 0.3 - fh * 0.5;
                    let tx = hx + hw * 0.5;
                    let ty = hy + hh * 0.5;
                    let dx = tx - (sx + fw * 0.5);
                    let dy = ty - (sy + fh * 0.5);
                    let len = (dx * dx + dy * dy).sqrt().max(1.0);
                    mage_bolts.push((
                        sx,
                        sy,
                        dx / len * SUMMONER_FIREBALL_SPEED,
                        dy / len * SUMMONER_FIREBALL_SPEED,
                    ));
                }
                e.attack_cd = MAGE_CAST_INTERVAL;
                e.lunge = crate::entity::MAGE_CAST_TIME;
                e.vx = 0.0;
                broadcasts.push(ServerMessage::EntityLunging { id });
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: e.x,
                    y: e.y,
                    vx: 0.0,
                    vy: e.vy,
                });
                continue;
            }
        }

        // No spell to cast: a recruited mage heels to its owner; a wild one (or one
        // whose owner is away) ambles its home patch.
        let dir = match owner_pos {
            Some((ox, oy)) => {
                let ocx = ox + PLAYER_SIZE.0 * 0.5;
                let owner_gap = aabb_gap(e.x, e.y, w, h, ox, oy, PLAYER_SIZE.0, PLAYER_SIZE.1);
                if owner_gap <= MAGE_FOLLOW_GAP {
                    0.0
                } else if ocx < scx {
                    -1.0
                } else {
                    1.0
                }
            }
            None => {
                let home = *e.home_x.get_or_insert(e.x);
                wander_dir(scx, e.vx, home)
            }
        };
        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, MAGE_SPEED, true);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }
    // Apply the mage casts queued above (the entities lock is free of the per-mage
    // borrow now). Restores turn a foe (recruiting it to the mage's owner, if any);
    // sunbursts wipe daylight-burners around the mage; bolts spawn friendly skulls.
    for (tid, owner) in mage_restores {
        let new_kind = entities
            .get(tid)
            .and_then(|e| restored_kind(&e.kind, &owner));
        if let Some(nk) = new_kind {
            broadcasts.extend(transform_entity(&mut entities, tid, nk));
        }
    }
    for (bx, by) in mage_sunbursts {
        let r2 = SUNBURST_RADIUS * SUNBURST_RADIUS;
        let targets: Vec<EntityId> = entities
            .values()
            .filter(|e| burns_in_daylight(&e.kind))
            .filter(|e| {
                let (w, h) = e.size();
                (e.x + w * 0.5 - bx).powi(2) + (e.y + h * 0.5 - by).powi(2) <= r2
            })
            .map(|e| e.id)
            .collect();
        for tid in targets {
            let (msgs, _) = apply_damage(
                &mut entities,
                tid,
                1_000_000,
                (0.0, 0.0),
                dim,
                (dim, 0.0, 0.0),
            );
            broadcasts.extend(msgs);
        }
    }
    for (x, y, vx, vy) in mage_bolts {
        let fid = shared.alloc_id();
        let mut bolt = Entity::new(fid, EntityKind::FriendlySummonerFireball, x, y);
        bolt.vx = vx;
        bolt.vy = vy;
        bolt.attack_cd = SUMMONER_FIREBALL_LIFETIME; // reused as the airborne life timer
        entities.insert(bolt.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: bolt });
    }

    // Zombies: slow, relentless night hunters that hit hard. When day breaks
    // they crumble where they stand, playing a death animation before despawning.
    // Ids whose death animation finished this tick and must be removed below.
    let mut zombie_despawns: Vec<EntityId> = Vec::new();
    for id in zombie_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Already crumbling: hold still (gravity still settles it onto ground),
        // run out the death timer, then mark it for removal.
        if e.dying > 0.0 {
            e.dying -= TICK_DT;
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, 0.0, 0.0, false);
            e.x = m.x;
            e.y = m.y;
            e.vx = 0.0;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: 0.0,
                vy: m.vy,
            });
            if e.dying <= 0.0 {
                zombie_despawns.push(id);
            }
            continue;
        }

        // Daybreak: begin dying. Tell every client to play the crumble.
        if !night {
            e.dying = crate::entity::ZOMBIE_DEATH_TIME;
            e.vx = 0.0;
            broadcasts.push(ServerMessage::EntityDying { id });
            continue;
        }

        // Night: shamble after the nearest player within aggro range, otherwise
        // wander its home patch like the other ground creatures.
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, ZOMBIE_AGGRO);
        let chasing = target.is_some();
        let dir = match target {
            Some(p) if p.x + p.w * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            ZOMBIE_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });

        if let Some(p) = target {
            if e.attack_cd <= 0.0
                && aabb_gap(m.x, m.y, w, h, p.x, p.y, p.w, p.h) <= ZOMBIE_ATTACK_RANGE
            {
                e.attack_cd = ZOMBIE_ATTACK_INTERVAL;
                let dir = if p.x + p.w * 0.5 >= m.x + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                hit_prey(
                    &mut bites,
                    &mut knight_hits,
                    &p,
                    (dir * KNOCKBACK_X, -KNOCKBACK_Y),
                    ZOMBIE_DAMAGE,
                );
            }
        }
    }
    for id in zombie_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Skeletons: night undead archers. They stalk the player like a zombie but
    // hang back and lob bones from range. Daybreak destroys them outright —
    // unlike the zombie they have no crumble animation to play.
    let mut skeleton_despawns: Vec<EntityId> = Vec::new();
    // Bones loosed this tick, spawned after the loop so we aren't holding a
    // mutable borrow of the thrower while inserting into the same map. Each entry
    // is the bone's spawn `(x, y)` and its flight `(vx, vy)`.
    let mut throws: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in skeleton_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Daybreak: burn up and vanish on the spot (no death animation yet).
        if !night {
            skeleton_despawns.push(id);
            continue;
        }

        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, SKELETON_AGGRO);
        let chasing = target.is_some();

        // Kiting heading: close in when out of throwing range, back off when the
        // target slips inside the standoff distance, otherwise hold and fire.
        let (dir, gap, aim) = match target {
            Some(p) => {
                let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
                let toward = if p.x + p.w * 0.5 >= scx { 1.0 } else { -1.0 };
                let dir = if gap < SKELETON_KEEP_DIST {
                    -toward // too close: retreat
                } else if gap > SKELETON_THROW_RANGE {
                    toward // too far: advance
                } else {
                    0.0 // in the sweet spot: stand and throw
                };
                (dir, gap, Some((p.x, p.y, p.w, p.h)))
            }
            None => (wander_dir(scx, e.vx, home), f32::INFINITY, None),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            SKELETON_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        // The client derives facing from the sign of the broadcast vx. A skeleton
        // should always face the player it's fighting — even while striding
        // backwards to keep its distance — so point the reported vx's sign at the
        // target while keeping its true magnitude (so the walk cycle still plays).
        // Its real velocity stays in `e.vx` so wander heading survives losing the
        // target.
        let bcast_vx = match aim {
            Some((px, _, pw, _)) => {
                let cx = m.x + w * 0.5;
                let toward = if px + pw * 0.5 >= cx { 1.0 } else { -1.0 };
                toward * m.vx.abs()
            }
            None => m.vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: bcast_vx,
            vy: m.vy,
        });

        // Loose a bone when a target is within range and we're off cooldown,
        // aiming from the skeleton's upper body straight at the target's center.
        if let Some((px, py, pw, ph)) = aim {
            if e.attack_cd <= 0.0 && gap <= SKELETON_THROW_RANGE {
                e.attack_cd = SKELETON_THROW_INTERVAL;
                let (bw, bh) = BONE_SIZE;
                let sx = m.x + w * 0.5 - bw * 0.5;
                let sy = m.y + h * 0.3 - bh * 0.5;
                let tx = px + pw * 0.5;
                let ty = py + ph * 0.5;
                let dx = tx - (sx + bw * 0.5);
                let dy = ty - (sy + bh * 0.5);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                throws.push((sx, sy, dx / len * BONE_SPEED, dy / len * BONE_SPEED));
            }
        }
    }
    for id in skeleton_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }
    // Spawn the bones loosed this tick. They aren't in `bone_ids`, so they begin
    // flying next tick rather than being simulated again immediately.
    for (x, y, vx, vy) in throws {
        let bid = shared.alloc_id();
        let mut bone = Entity::new(bid, EntityKind::Bone, x, y);
        bone.vx = vx;
        bone.vy = vy;
        bone.attack_cd = BONE_LIFETIME; // reused as the airborne lifetime timer
        entities.insert(bone.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: bone });
    }

    // Dark knights: black-armoured warriors that stalk the overworld night and kite
    // their quarry like a skeleton — hanging at range and hurling axes — but harder and
    // deadlier, and they hunt knights as readily as players (nearest_prey hands back
    // both). They burn up at daybreak like the other overworld night mobs.
    let mut dark_knight_despawns: Vec<EntityId> = Vec::new();
    // Axes loosed this tick, spawned after the loop so we aren't holding a mutable
    // borrow of the thrower while inserting. Each is the axe's spawn `(x, y)` and its
    // flight `(vx, vy)`.
    let mut axe_throws: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in dark_knight_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Daybreak: burn up and vanish on the spot (no death animation).
        if !night {
            dark_knight_despawns.push(id);
            continue;
        }

        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, DARK_KNIGHT_AGGRO);
        let chasing = target.is_some();

        // Kiting heading: close in when out of throwing range, back off when the target
        // slips inside the standoff distance, otherwise hold and throw.
        let (dir, gap, aim) = match target {
            Some(p) => {
                let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
                let toward = if p.x + p.w * 0.5 >= scx { 1.0 } else { -1.0 };
                let dir = if gap < DARK_KNIGHT_KEEP_DIST {
                    -toward // too close: retreat
                } else if gap > DARK_KNIGHT_THROW_RANGE {
                    toward // too far: advance
                } else {
                    0.0 // in the sweet spot: stand and throw
                };
                (dir, gap, Some((p.x, p.y, p.w, p.h)))
            }
            None => (wander_dir(scx, e.vx, home), f32::INFINITY, None),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            DARK_KNIGHT_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        // Face the target even while striding backwards to keep distance (see the
        // skeleton loop): point the broadcast vx's sign at the target, true magnitude.
        let bcast_vx = match aim {
            Some((px, _, pw, _)) => {
                let cx = m.x + w * 0.5;
                let toward = if px + pw * 0.5 >= cx { 1.0 } else { -1.0 };
                toward * m.vx.abs()
            }
            None => m.vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: bcast_vx,
            vy: m.vy,
        });

        // Loose an axe when a target is within range and we're off cooldown, aiming
        // from the dark knight's upper body straight at the target's center.
        if let Some((px, py, pw, ph)) = aim {
            if e.attack_cd <= 0.0 && gap <= DARK_KNIGHT_THROW_RANGE {
                e.attack_cd = DARK_KNIGHT_THROW_INTERVAL;
                let (aw, ah) = AXE_SIZE;
                let sx = m.x + w * 0.5 - aw * 0.5;
                let sy = m.y + h * 0.3 - ah * 0.5;
                let tx = px + pw * 0.5;
                let ty = py + ph * 0.5;
                let dx = tx - (sx + aw * 0.5);
                let dy = ty - (sy + ah * 0.5);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                axe_throws.push((sx, sy, dx / len * AXE_SPEED, dy / len * AXE_SPEED));
            }
        }
    }
    for id in dark_knight_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }
    // Spawn the axes loosed this tick. They aren't in `axe_ids`, so they begin flying
    // next tick rather than being simulated again immediately.
    for (x, y, vx, vy) in axe_throws {
        let aid = shared.alloc_id();
        let mut axe = Entity::new(aid, EntityKind::Axe, x, y);
        axe.vx = vx;
        axe.vy = vy;
        axe.attack_cd = AXE_LIFETIME; // reused as the airborne lifetime timer
        entities.insert(axe.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: axe });
    }

    // Dark musketeers: black-clad marksmen that march under the demon king's banner and
    // kite their quarry like a dark knight — hanging at range and firing bullets — but
    // they hunt knights and musketeers as readily as players (nearest_prey hands back
    // players and warriors alike, and `knight_boxes` now carries both). They burn up at
    // daybreak like the other overworld night mobs (and roam the always-dark arena they
    // are summoned into around the clock).
    let mut dark_musketeer_despawns: Vec<EntityId> = Vec::new();
    // Bullets loosed this tick, spawned after the loop so we aren't holding a mutable
    // borrow of the shooter while inserting. Each is `(x, y, vx, vy)`.
    let mut bullet_throws: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in dark_musketeer_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Daybreak: burn up and vanish on the spot (no death animation).
        if !night {
            dark_musketeer_despawns.push(id);
            continue;
        }

        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(
            &hostile_players,
            &knight_boxes,
            scx,
            scy,
            DARK_MUSKETEER_AGGRO,
        );
        let chasing = target.is_some();

        let (dir, gap, aim) = match target {
            Some(p) => {
                let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
                let toward = if p.x + p.w * 0.5 >= scx { 1.0 } else { -1.0 };
                let dir = if gap < DARK_MUSKETEER_KEEP_DIST {
                    -toward
                } else if gap > DARK_MUSKETEER_SHOOT_RANGE {
                    toward
                } else {
                    0.0
                };
                (dir, gap, Some((p.x, p.y, p.w, p.h)))
            }
            None => (wander_dir(scx, e.vx, home), f32::INFINITY, None),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            DARK_MUSKETEER_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        let bcast_vx = match aim {
            Some((px, _, pw, _)) => {
                let cx = m.x + w * 0.5;
                let toward = if px + pw * 0.5 >= cx { 1.0 } else { -1.0 };
                toward * m.vx.abs()
            }
            None => m.vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: bcast_vx,
            vy: m.vy,
        });

        // Fire a bullet when a target is within range and we're off cooldown, from the
        // dark musketeer's upper body straight at the target's center.
        if let Some((px, py, pw, ph)) = aim {
            if e.attack_cd <= 0.0 && gap <= DARK_MUSKETEER_SHOOT_RANGE {
                e.attack_cd = DARK_MUSKETEER_SHOOT_INTERVAL;
                let (bw, bh) = crate::entity::BULLET_SIZE;
                let sx = m.x + w * 0.5 - bw * 0.5;
                let sy = m.y + h * 0.3 - bh * 0.5;
                let tx = px + pw * 0.5;
                let ty = py + ph * 0.5;
                let dx = tx - (sx + bw * 0.5);
                let dy = ty - (sy + bh * 0.5);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                bullet_throws.push((sx, sy, dx / len * BULLET_SPEED, dy / len * BULLET_SPEED));
                broadcasts.push(ServerMessage::EntityLunging { id });
            }
        }
    }
    for id in dark_musketeer_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }
    // Spawn the bullets dark musketeers loosed this tick. They aren't in `bullet_ids`,
    // so they begin flying next tick rather than being simulated again immediately.
    for (x, y, vx, vy) in bullet_throws {
        let bid = shared.alloc_id();
        let mut bullet = Entity::new(bid, EntityKind::Bullet, x, y);
        bullet.vx = vx;
        bullet.vy = vy;
        bullet.attack_cd = BULLET_LIFETIME; // reused as the airborne lifetime timer
        entities.insert(bullet.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: bullet });
    }

    // Necromancers: hooded ranged casters that kite the player like a skeleton but,
    // instead of bones, hurl summoner fireballs that burst into bouncing skulls. They
    // burn up at daybreak (in the overworld; the underworld is always "night").
    let mut necromancer_despawns: Vec<EntityId> = Vec::new();
    // Summoner fireballs loosed this tick, spawned after the loop so we aren't holding
    // a mutable borrow of the caster while inserting. Each is `(x, y, vx, vy)`.
    let mut summons: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in necromancer_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Daybreak: burn up and vanish on the spot (like the skeleton).
        if !night {
            necromancer_despawns.push(id);
            continue;
        }

        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, NECROMANCER_AGGRO);
        let chasing = target.is_some();

        // Kiting heading: close in when out of range, back off when crowded, else hold.
        let (dir, gap, aim) = match target {
            Some(p) => {
                let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
                let toward = if p.x + p.w * 0.5 >= scx { 1.0 } else { -1.0 };
                let dir = if gap < NECROMANCER_KEEP_DIST {
                    -toward // too close: retreat
                } else if gap > NECROMANCER_SHOOT_RANGE {
                    toward // too far: advance
                } else {
                    0.0 // in the sweet spot: stand and summon
                };
                (dir, gap, Some((p.x, p.y, p.w, p.h)))
            }
            None => (wander_dir(scx, e.vx, home), f32::INFINITY, None),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            NECROMANCER_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        // Point the reported vx's sign at the target so it faces the player it's
        // fighting even while backing away (as the skeleton does).
        let bcast_vx = match aim {
            Some((px, _, pw, _)) => {
                let cx = m.x + w * 0.5;
                let toward = if px + pw * 0.5 >= cx { 1.0 } else { -1.0 };
                toward * m.vx.abs()
            }
            None => m.vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: bcast_vx,
            vy: m.vy,
        });

        // Loose a summoner fireball when a target is within range and we're off
        // cooldown, aiming from the caster's upper body at the target's center.
        if let Some((px, py, pw, ph)) = aim {
            if e.attack_cd <= 0.0 && gap <= NECROMANCER_SHOOT_RANGE {
                e.attack_cd = NECROMANCER_SHOOT_INTERVAL;
                let (fw, fh) = SUMMONER_FIREBALL_SIZE;
                let sx = m.x + w * 0.5 - fw * 0.5;
                let sy = m.y + h * 0.3 - fh * 0.5;
                let tx = px + pw * 0.5;
                let ty = py + ph * 0.5;
                let dx = tx - (sx + fw * 0.5);
                let dy = ty - (sy + fh * 0.5);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                summons.push((
                    sx,
                    sy,
                    dx / len * SUMMONER_FIREBALL_SPEED,
                    dy / len * SUMMONER_FIREBALL_SPEED,
                ));
            }
        }
    }
    for id in necromancer_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }
    // Spawn the summoner fireballs loosed this tick (not in `summoner_fireball_ids`, so
    // they begin flying next tick rather than being simulated again immediately).
    for (x, y, vx, vy) in summons {
        let fid = shared.alloc_id();
        let mut fb = Entity::new(fid, EntityKind::SummonerFireball, x, y);
        fb.vx = vx;
        fb.vy = vy;
        fb.attack_cd = SUMMONER_FIREBALL_LIFETIME; // reused as the airborne lifetime timer
        entities.insert(fb.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: fb });
    }

    // Skulls: bouncing skeleton skulls a necromancer summoned. They carom around under
    // gravity — springing off floors, ricocheting off walls — and steer their hops
    // toward nearby players to gnash at them, giving out after a short life. They burn
    // up at daybreak in the overworld (but bound freely through the always-dark depths).
    let mut skull_despawns: Vec<EntityId> = Vec::new();
    for id in skull_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Daybreak: burn up and vanish (like its summoner).
        if !night {
            skull_despawns.push(id);
            continue;
        }

        // Run out its short life, then despawn.
        e.flee = (e.flee - TICK_DT).max(0.0); // `flee` repurposed as the skull's life timer
        if e.flee <= 0.0 {
            skull_despawns.push(id);
            continue;
        }
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, SKULL_AGGRO);

        // Ballistic bounce: gravity pulls it down; it reflects off walls and springs
        // back up off floors, biasing each hop toward the player it can see.
        e.vy = (e.vy + GRAVITY * TICK_DT).min(MAX_FALL);
        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        if hit_x {
            e.vx = -e.vx; // ricochet off the wall
        }
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;
        if hit_y {
            if e.vy > 0.0 {
                // Landed: spring back up and steer this hop toward the player.
                e.vy = SKULL_BOUNCE_VELOCITY;
                e.vx = match target {
                    Some(ref p) if p.x + p.w * 0.5 < scx => -SKULL_SPEED,
                    Some(_) => SKULL_SPEED,
                    None if e.vx < 0.0 => -SKULL_SPEED,
                    None => SKULL_SPEED,
                };
            } else {
                e.vy = 0.0; // bonked a ceiling: kill the upward run
            }
        }
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });

        // Gnash the player on contact, on its own cadence.
        if let Some(p) = target {
            if e.attack_cd <= 0.0
                && aabb_gap(nx, ny, w, h, p.x, p.y, p.w, p.h) <= SKULL_ATTACK_RANGE
            {
                e.attack_cd = SKULL_ATTACK_INTERVAL;
                let kdir = if p.x + p.w * 0.5 >= nx + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                hit_prey(
                    &mut bites,
                    &mut knight_hits,
                    &p,
                    (kdir * KNOCKBACK_X, -KNOCKBACK_Y),
                    SKULL_DAMAGE,
                );
            }
        }
    }
    for id in skull_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Friendly skulls: a player's summoner spell conjured these to fight on their
    // behalf. They carom under gravity exactly like a necromancer's skull, but they
    // steer their hops toward — and gnash at — the nearest *monster* (`hostiles`)
    // rather than players or knights, and they pay no heed to day or night. Like the
    // hostile skull they give out after a short life. Being non-hostile, knights and
    // monsters ignore them entirely.
    let mut friendly_skull_despawns: Vec<EntityId> = Vec::new();
    for id in friendly_skull_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Run out its short life, then despawn.
        e.flee = (e.flee - TICK_DT).max(0.0); // `flee` repurposed as the skull's life timer
        if e.flee <= 0.0 {
            friendly_skull_despawns.push(id);
            continue;
        }
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_of(&hostiles, scx, scy, SKULL_AGGRO);

        // Ballistic bounce: gravity pulls it down; it reflects off walls and springs
        // back up off floors, biasing each hop toward the monster it can see.
        e.vy = (e.vy + GRAVITY * TICK_DT).min(MAX_FALL);
        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        if hit_x {
            e.vx = -e.vx; // ricochet off the wall
        }
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;
        if hit_y {
            if e.vy > 0.0 {
                // Landed: spring back up and steer this hop toward the monster.
                e.vy = SKULL_BOUNCE_VELOCITY;
                e.vx = match target {
                    Some((_, tx, _, tw, _)) if tx + tw * 0.5 < scx => -SKULL_SPEED,
                    Some(_) => SKULL_SPEED,
                    None if e.vx < 0.0 => -SKULL_SPEED,
                    None => SKULL_SPEED,
                };
            } else {
                e.vy = 0.0; // bonked a ceiling: kill the upward run
            }
        }
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });

        // Gnash the monster on contact, on its own cadence.
        if let Some((tid, tx, ty, tw, th)) = target {
            if e.attack_cd <= 0.0 && aabb_gap(nx, ny, w, h, tx, ty, tw, th) <= SKULL_ATTACK_RANGE {
                e.attack_cd = SKULL_ATTACK_INTERVAL;
                let kdir = if tx + tw * 0.5 >= nx + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                creature_hits.push((tid, (kdir * KNOCKBACK_X, -KNOCKBACK_Y), SKULL_DAMAGE));
            }
        }
    }
    for id in friendly_skull_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Charred skeletons: the underworld's relentless brawlers. They charge any
    // player on sight (at all hours — the underworld is always dark), hit harder
    // than a zombie, and lay down a trail of fire behind them while closing in.
    for id in charred_skeleton_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        // The fire-trail timer rides on the unused `flee` field for this kind.
        e.flee = (e.flee - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        let target = nearest_prey(
            &hostile_players,
            &knight_boxes,
            scx,
            scy,
            CHARRED_SKELETON_AGGRO,
        );
        let chasing = target.is_some();
        let dir = match target {
            Some(p) if p.x + p.w * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            CHARRED_SKELETON_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        let ready_to_burn = chasing && e.flee <= 0.0;
        if ready_to_burn {
            e.flee = CHARRED_SKELETON_FIRE_INTERVAL;
        }
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });

        // Only while actively chasing does it scorch the ground: drop a flame in
        // the (empty) cell at its feet, on a short interval so the trail is broken.
        if ready_to_burn {
            let fx = (m.x + w * 0.5) / TILE_SIZE;
            let fy = (m.y + h - EPS) / TILE_SIZE;
            let (fx, fy) = (fx.floor() as i32, fy.floor() as i32);
            if world.get(fx, fy) == crate::block::AIR && world.set(fx, fy, crate::block::FIRE) {
                new_trail_fires.push((fx, fy));
                broadcasts.push(ServerMessage::BlockUpdate {
                    dim,
                    x: fx,
                    y: fy,
                    block: crate::block::FIRE,
                });
            }
        }

        // Land a heavy melee blow when a target is in reach and we're off cooldown.
        if let Some(p) = target {
            if e.attack_cd <= 0.0
                && aabb_gap(m.x, m.y, w, h, p.x, p.y, p.w, p.h) <= CHARRED_SKELETON_ATTACK_RANGE
            {
                e.attack_cd = CHARRED_SKELETON_ATTACK_INTERVAL;
                let dir = if p.x + p.w * 0.5 >= m.x + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                hit_prey(
                    &mut bites,
                    &mut knight_hits,
                    &p,
                    (dir * KNOCKBACK_X, -KNOCKBACK_Y),
                    CHARRED_SKELETON_DAMAGE,
                );
            }
        }
    }

    // Demons: underworld casters. Like the charred skeleton they hunt at all hours
    // (the underworld is always dark), but instead of charging into melee they kite
    // the player the way a skeleton does — closing when out of range, backing off
    // when crowded — and hurl fireballs from a distance.
    //
    // Fireballs loosed this tick, spawned after the loop so we aren't holding a
    // mutable borrow of the caster while inserting into the same map. Each entry is
    // the fireball's spawn `(x, y)` and its flight `(vx, vy)`.
    let mut fireball_throws: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in demon_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, DEMON_AGGRO);
        let chasing = target.is_some();

        // Kiting heading: close in when out of fireball range, back off when the
        // target slips inside the standoff distance, otherwise hold and fire.
        let (dir, gap, aim) = match target {
            Some(p) => {
                let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
                let toward = if p.x + p.w * 0.5 >= scx { 1.0 } else { -1.0 };
                let dir = if gap < DEMON_KEEP_DIST {
                    -toward // too close: retreat
                } else if gap > DEMON_SHOOT_RANGE {
                    toward // too far: advance
                } else {
                    0.0 // in the sweet spot: stand and fire
                };
                (dir, gap, Some((p.x, p.y, p.w, p.h)))
            }
            None => (wander_dir(scx, e.vx, home), f32::INFINITY, None),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            DEMON_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        // As with the skeleton, point the reported vx's sign at the target so the
        // demon always faces the player it's fighting — even while backing away —
        // while keeping its true magnitude so the walk cycle still plays.
        let bcast_vx = match aim {
            Some((px, _, pw, _)) => {
                let cx = m.x + w * 0.5;
                let toward = if px + pw * 0.5 >= cx { 1.0 } else { -1.0 };
                toward * m.vx.abs()
            }
            None => m.vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: bcast_vx,
            vy: m.vy,
        });

        // Hurl a fireball when a target is within range and we're off cooldown,
        // aiming from the demon's upper body straight at the target's center.
        if let Some((px, py, pw, ph)) = aim {
            if e.attack_cd <= 0.0 && gap <= DEMON_SHOOT_RANGE {
                e.attack_cd = DEMON_SHOOT_INTERVAL;
                let (fw, fh) = FIREBALL_SIZE;
                let sx = m.x + w * 0.5 - fw * 0.5;
                let sy = m.y + h * 0.3 - fh * 0.5;
                let tx = px + pw * 0.5;
                let ty = py + ph * 0.5;
                let dx = tx - (sx + fw * 0.5);
                let dy = ty - (sy + fh * 0.5);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                fireball_throws.push((
                    sx,
                    sy,
                    dx / len * FIREBALL_SPEED,
                    dy / len * FIREBALL_SPEED,
                ));
            }
        }
    }
    // Spawn the fireballs loosed this tick. They aren't in `fireball_ids`, so they
    // begin flying next tick rather than being simulated again immediately.
    for (x, y, vx, vy) in fireball_throws {
        let fid = shared.alloc_id();
        let mut fireball = Entity::new(fid, EntityKind::Fireball, x, y);
        fireball.vx = vx;
        fireball.vy = vy;
        fireball.attack_cd = FIREBALL_LIFETIME; // reused as the airborne lifetime timer
        entities.insert(fireball.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: fireball });
    }

    // Enchanted demons: demons an orc mage has empowered. They kite the player like
    // an ordinary demon — closing when out of range, backing off when crowded — but
    // they FLY, chasing through the air rather than scrambling over the ground, and
    // hurl magic fireballs that reach farther and hit harder. Active at all hours.
    //
    // Magic fireballs loosed this tick, spawned after the loop (as for the demon) so
    // we aren't holding a mutable borrow of the caster while inserting. Each entry is
    // the bolt's spawn `(x, y)` and its flight `(vx, vy)`.
    let mut magic_fireball_throws: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in enchanted_demon_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(
            &hostile_players,
            &knight_boxes,
            scx,
            scy,
            ENCHANTED_DEMON_AGGRO,
        );

        // Flying kite: steer in both axes toward (or away from) the target. Close in
        // when out of fireball range, back off when crowded, otherwise hold position
        // horizontally while matching the target's altitude so its shots fly level.
        let (vx, vy, aim) = match target {
            Some(p) => {
                let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
                let dx = (p.x + p.w * 0.5) - scx;
                let dy = (p.y + p.h * 0.5) - scy;
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                let (ux, uy) = (dx / len, dy / len);
                let (vx, vy) = if gap < ENCHANTED_DEMON_KEEP_DIST {
                    (-ux * ENCHANTED_DEMON_SPEED, -uy * ENCHANTED_DEMON_SPEED) // too close: retreat
                } else if gap > ENCHANTED_DEMON_SHOOT_RANGE {
                    (ux * ENCHANTED_DEMON_SPEED, uy * ENCHANTED_DEMON_SPEED) // too far: advance
                } else {
                    (0.0, uy * ENCHANTED_DEMON_SPEED) // in range: hold, just match altitude
                };
                (vx, vy, Some((p.x, p.y, p.w, p.h, gap)))
            }
            // No quarry: hover, drifting gently across its home patch at its level.
            None => (
                wander_dir(scx, e.vx, home) * ENCHANTED_DEMON_SPEED,
                0.0,
                None,
            ),
        };

        // Fly freely (no gravity), each axis stopped independently by walls.
        let (nx, _) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
        let (ny, _) = move_y(&mut world, nx, e.y, w, h, vy * TICK_DT);
        e.x = nx;
        e.y = ny;
        e.vx = vx;
        e.vy = vy;
        // Point the reported vx's sign at the target so it always faces the player it
        // is fighting (even while backing away), as the demon does.
        let bcast_vx = match aim {
            Some((px, _, pw, _, _)) => {
                let cx = nx + w * 0.5;
                let toward = if px + pw * 0.5 >= cx { 1.0 } else { -1.0 };
                toward * vx.abs()
            }
            None => vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: bcast_vx,
            vy,
        });

        // Loose a volley of magic fireballs when a target is within range and we're
        // off cooldown, aiming from the demon's upper body at the target's center.
        // Rather than one bolt it fires three: one straight at the target and two
        // fanned above and below it (the unit aim vector rotated by ±the spread).
        if let Some((px, py, pw, ph, gap)) = aim {
            if e.attack_cd <= 0.0 && gap <= ENCHANTED_DEMON_SHOOT_RANGE {
                e.attack_cd = ENCHANTED_DEMON_SHOOT_INTERVAL;
                let (fw, fh) = MAGIC_FIREBALL_SIZE;
                let sx = nx + w * 0.5 - fw * 0.5;
                let sy = ny + h * 0.3 - fh * 0.5;
                let tx = px + pw * 0.5;
                let ty = py + ph * 0.5;
                let ddx = tx - (sx + fw * 0.5);
                let ddy = ty - (sy + fh * 0.5);
                let dlen = (ddx * ddx + ddy * ddy).sqrt().max(1.0);
                let (ux, uy) = (ddx / dlen, ddy / dlen);
                for a in [0.0, ENCHANTED_DEMON_SPREAD, -ENCHANTED_DEMON_SPREAD] {
                    let (s, c) = a.sin_cos();
                    let rx = ux * c - uy * s;
                    let ry = ux * s + uy * c;
                    magic_fireball_throws.push((
                        sx,
                        sy,
                        rx * MAGIC_FIREBALL_SPEED,
                        ry * MAGIC_FIREBALL_SPEED,
                    ));
                }
            }
        }
    }
    // Spawn the magic fireballs loosed this tick (not in `fireball_ids`, so they begin
    // flying next tick rather than being simulated again immediately).
    for (x, y, vx, vy) in magic_fireball_throws {
        let fid = shared.alloc_id();
        let mut fireball = Entity::new(fid, EntityKind::MagicFireball, x, y);
        fireball.vx = vx;
        fireball.vy = vy;
        fireball.attack_cd = MAGIC_FIREBALL_LIFETIME; // reused as the airborne lifetime timer
        entities.insert(fireball.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: fireball });
    }

    // Dragons: the underworld's rare flying miniboss. Like the enchanted demon they
    // FLY, kiting the player through the air — closing when out of range, backing off
    // when crowded — but they are swifter, tougher, and breathe ordinary fireballs on
    // a brisk cadence. Active at all hours (the underworld is always dark).
    //
    // Fireballs loosed this tick, spawned after the loop (as for the demons) so we
    // aren't holding a mutable borrow of the dragon while inserting. Each entry is the
    // fireball's spawn `(x, y)` and its flight `(vx, vy)`.
    let mut dragon_fireball_throws: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in dragon_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        e.lunge = (e.lunge - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, DRAGON_AGGRO);

        // Flying kite, steering in both axes: close in when out of fireball range,
        // back off when crowded, otherwise hold horizontally and match the target's
        // altitude so its breath flies level.
        let (vx, vy, aim) = match target {
            Some(p) => {
                let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
                let dx = (p.x + p.w * 0.5) - scx;
                let dy = (p.y + p.h * 0.5) - scy;
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                let (ux, uy) = (dx / len, dy / len);
                let (vx, vy) = if gap < DRAGON_KEEP_DIST {
                    (-ux * DRAGON_SPEED, -uy * DRAGON_SPEED) // too close: retreat
                } else if gap > DRAGON_SHOOT_RANGE {
                    (ux * DRAGON_SPEED, uy * DRAGON_SPEED) // too far: advance
                } else {
                    (0.0, uy * DRAGON_SPEED) // in range: hold, just match altitude
                };
                (vx, vy, Some((p.x, p.y, p.w, p.h, gap)))
            }
            // No quarry: soar gently across its home patch at its current level.
            None => (wander_dir(scx, e.vx, home) * DRAGON_SPEED, 0.0, None),
        };

        // Fly freely (no gravity), each axis stopped independently by walls.
        let (nx, _) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
        let (ny, _) = move_y(&mut world, nx, e.y, w, h, vy * TICK_DT);
        e.x = nx;
        e.y = ny;
        e.vx = vx;
        e.vy = vy;
        // Point the reported vx's sign at the target so it always faces the player it
        // is fighting, even while backing away.
        let bcast_vx = match aim {
            Some((px, _, pw, _, _)) => {
                let cx = nx + w * 0.5;
                let toward = if px + pw * 0.5 >= cx { 1.0 } else { -1.0 };
                toward * vx.abs()
            }
            None => vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: bcast_vx,
            vy,
        });

        // Breathe a fireball when a target is within range and we're off cooldown,
        // aiming from the dragon's maw (front-upper body) at the target's center, and
        // kick off the fire-breathing animation.
        if let Some((px, py, pw, ph, gap)) = aim {
            if e.attack_cd <= 0.0 && gap <= DRAGON_SHOOT_RANGE {
                e.attack_cd = DRAGON_SHOOT_INTERVAL;
                e.lunge = crate::entity::DRAGON_ATTACK_TIME;
                broadcasts.push(ServerMessage::EntityLunging { id });
                let (fw, fh) = FIREBALL_SIZE;
                let sx = nx + w * 0.5 - fw * 0.5;
                let sy = ny + h * 0.3 - fh * 0.5;
                let tx = px + pw * 0.5;
                let ty = py + ph * 0.5;
                let dx = tx - (sx + fw * 0.5);
                let dy = ty - (sy + fh * 0.5);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                dragon_fireball_throws.push((
                    sx,
                    sy,
                    dx / len * FIREBALL_SPEED,
                    dy / len * FIREBALL_SPEED,
                ));
            }
        }
    }
    // Spawn the dragon fireballs loosed this tick (not in `fireball_ids`, so they begin
    // flying next tick rather than being simulated again immediately).
    for (x, y, vx, vy) in dragon_fireball_throws {
        let fid = shared.alloc_id();
        let mut fireball = Entity::new(fid, EntityKind::Fireball, x, y);
        fireball.vx = vx;
        fireball.vy = vy;
        fireball.attack_cd = FIREBALL_LIFETIME; // reused as the airborne lifetime timer
        entities.insert(fireball.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: fireball });
    }

    // White dragons: the friendly steeds the dragonian steed spell summons. They FLY
    // like the hostile dragon, but they serve their owner: a *ridden* one snaps beneath
    // its rider each tick (the rider drives the flight client-side and breathes fire on
    // the breath key — handled in the message loop); a free one hunts the nearest
    // monster, breathing friendly fireballs at it, and otherwise soars to its owner's
    // side, teleporting over if they stray too far. Like a pet it never despawns.
    let mut white_dragon_fireball_throws: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in white_dragon_ids {
        // A ridden steed: glue it centred beneath its rider and skip its own AI, just
        // like a ridden horse (it is hidden and drawn as the combined player/dragon
        // sprite, so its exact spot only matters at dismount).
        if let Some(&(_, rx, ry)) = ridden_horses.iter().find(|(hid, ..)| *hid == id) {
            let Some(e) = entities.get_mut(id) else {
                continue;
            };
            let (w, h) = e.size();
            e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
            e.lunge = (e.lunge - TICK_DT).max(0.0);
            e.x = rx + (PLAYER_SIZE.0 - w) * 0.5;
            e.y = ry + (PLAYER_SIZE.1 - h) * 0.5;
            e.vx = 0.0;
            e.vy = 0.0;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: e.x,
                y: e.y,
                vx: 0.0,
                vy: 0.0,
            });
            continue;
        }

        // A remotely-piloted steed: its summoner is driving it telepathically, so it runs
        // no AI of its own. Fly it from the pilot's movement intent (each axis stopped by
        // walls, like the free flight below), but never let it slip beyond the telepathic
        // leash — clamp it to within WHITE_DRAGON_CONTROL_RANGE of the pilot. Its attacks
        // are loosed on command (see `dragon_breath`), not chosen here.
        if let Some(&(_, dx, dy, px, py)) = controlled_dragons.iter().find(|(did, ..)| *did == id) {
            let Some(e) = entities.get_mut(id) else {
                continue;
            };
            let (w, h) = e.size();
            e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
            e.lunge = (e.lunge - TICK_DT).max(0.0);
            let vx = dx * WHITE_DRAGON_CONTROL_SPEED;
            let vy = dy * WHITE_DRAGON_CONTROL_SPEED;
            let (nx, _) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
            let (ny, _) = move_y(&mut world, nx, e.y, w, h, vy * TICK_DT);
            // Hold within the telepathic range of the pilot (centre to centre).
            let pcx = px + PLAYER_SIZE.0 * 0.5;
            let pcy = py + PLAYER_SIZE.1 * 0.5;
            let cx = (nx + w * 0.5).clamp(
                pcx - WHITE_DRAGON_CONTROL_RANGE,
                pcx + WHITE_DRAGON_CONTROL_RANGE,
            );
            let cy = (ny + h * 0.5).clamp(
                pcy - WHITE_DRAGON_CONTROL_RANGE,
                pcy + WHITE_DRAGON_CONTROL_RANGE,
            );
            e.x = cx - w * 0.5;
            e.y = cy - h * 0.5;
            e.vx = vx;
            e.vy = vy;
            e.home_x = Some(e.x);
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: e.x,
                y: e.y,
                vx,
                vy,
            });
            continue;
        }

        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        e.lunge = (e.lunge - TICK_DT).max(0.0);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        // Resolve the owner's live position (by name) in this dimension, if they're here.
        let owner_pos: Option<(f32, f32)> = e.kind.owner().and_then(|name| {
            named_players
                .iter()
                .find(|(n, ..)| n == name)
                .map(|&(_, _, ox, oy)| (ox, oy))
        });

        // Owner strayed too far: snap to their side rather than despawning.
        if let Some((ox, oy)) = owner_pos {
            let ocx = ox + PLAYER_SIZE.0 * 0.5;
            let ocy = oy + PLAYER_SIZE.1 * 0.5;
            if (ocx - scx).powi(2) + (ocy - scy).powi(2)
                > WHITE_DRAGON_TELEPORT_DIST * WHITE_DRAGON_TELEPORT_DIST
            {
                e.x = ox + (PLAYER_SIZE.0 - w) * 0.5;
                e.y = oy - h - 4.0;
                e.vx = 0.0;
                e.vy = 0.0;
                e.home_x = Some(e.x);
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: e.x,
                    y: e.y,
                    vx: 0.0,
                    vy: 0.0,
                });
                continue;
            }
        }

        // Hunt the nearest monster within aggro; failing that, soar to the owner's side.
        let target = nearest_of(&hostiles, scx, scy, WHITE_DRAGON_AGGRO);
        let (vx, vy, aim) = match target {
            // A monster to burn: kite it like the hostile dragon — close when far, back
            // off when crowded, hold and match altitude when in range.
            Some((_, tx, ty, tw, th)) => {
                let gap = aabb_gap(e.x, e.y, w, h, tx, ty, tw, th);
                let dx = (tx + tw * 0.5) - scx;
                let dy = (ty + th * 0.5) - scy;
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                let (ux, uy) = (dx / len, dy / len);
                let (vx, vy) = if gap < DRAGON_KEEP_DIST {
                    (-ux * DRAGON_SPEED, -uy * DRAGON_SPEED)
                } else if gap > WHITE_DRAGON_SHOOT_RANGE {
                    (ux * DRAGON_SPEED, uy * DRAGON_SPEED)
                } else {
                    (0.0, uy * DRAGON_SPEED)
                };
                (vx, vy, Some((tx, ty, tw, th, gap)))
            }
            // No monster near: glide to the owner, easing to a hover at their side. With
            // no owner here (offline or another dimension), drift on its home patch.
            None => match owner_pos {
                Some((ox, oy)) => {
                    // Aim for a spot just above and beside the owner.
                    let tx = ox + PLAYER_SIZE.0 * 0.5;
                    let ty = oy - h * 0.5;
                    let dx = tx - scx;
                    let dy = ty - scy;
                    let len = (dx * dx + dy * dy).sqrt();
                    if len <= WHITE_DRAGON_FOLLOW_GAP {
                        (0.0, 0.0, None)
                    } else {
                        (dx / len * DRAGON_SPEED, dy / len * DRAGON_SPEED, None)
                    }
                }
                None => {
                    let home = *e.home_x.get_or_insert(e.x);
                    (wander_dir(scx, e.vx, home) * DRAGON_SPEED, 0.0, None)
                }
            },
        };

        // Fly freely (no gravity), each axis stopped independently by walls.
        let (nx, _) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
        let (ny, _) = move_y(&mut world, nx, e.y, w, h, vy * TICK_DT);
        e.x = nx;
        e.y = ny;
        e.vx = vx;
        e.vy = vy;
        // Face the monster it is fighting even while backing away (else its travel dir).
        let bcast_vx = match aim {
            Some((tx, _, tw, _, _)) => {
                let cx = nx + w * 0.5;
                let toward = if tx + tw * 0.5 >= cx { 1.0 } else { -1.0 };
                toward * vx.abs()
            }
            None => vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: bcast_vx,
            vy,
        });

        // Breathe a friendly fireball at the monster when in range and off cooldown,
        // kicking off the fire-breathing animation.
        if let Some((tx, ty, tw, th, gap)) = aim
            && e.attack_cd <= 0.0
            && gap <= WHITE_DRAGON_SHOOT_RANGE
        {
            e.attack_cd = WHITE_DRAGON_SHOOT_INTERVAL;
            e.lunge = crate::entity::DRAGON_ATTACK_TIME;
            broadcasts.push(ServerMessage::EntityLunging { id });
            let (fw, fh) = crate::entity::FRIENDLY_DRAGON_FIREBALL_SIZE;
            let sx = nx + w * 0.5 - fw * 0.5;
            let sy = ny + h * 0.3 - fh * 0.5;
            let dx = (tx + tw * 0.5) - (sx + fw * 0.5);
            let dy = (ty + th * 0.5) - (sy + fh * 0.5);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            white_dragon_fireball_throws.push((
                sx,
                sy,
                dx / len * FIREBALL_SPEED,
                dy / len * FIREBALL_SPEED,
            ));
        }
    }
    // Spawn the friendly fireballs the steeds breathed this tick (not in
    // `friendly_dragon_fireball_ids`, so they begin flying next tick).
    for (x, y, vx, vy) in white_dragon_fireball_throws {
        let fid = shared.alloc_id();
        let mut fireball = Entity::new(fid, EntityKind::FriendlyDragonFireball, x, y);
        fireball.vx = vx;
        fireball.vy = vy;
        fireball.attack_cd = FRIENDLY_DRAGON_FIREBALL_LIFETIME; // reused as the airborne timer
        entities.insert(fireball.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: fireball });
    }

    // Orcs: the underworld's hulking brutes. They lumber after players at all hours
    // (slower than anything else afoot), then plant their feet and commit to a
    // telegraphed slam — heaving their arms up and crashing them down for heavy
    // damage. Like the snake's lunge the slam is dodgeable: the blow only lands on
    // the frame the fists hit the ground, so a player who backs out of reach during
    // the wind-up escapes it.
    for id in orc_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);

        // Mid-slam: a committed attack that plays out over ORC_SLAM_TIME. The orc
        // stands its ground through the wind-up and crash (gravity still settles it
        // onto the floor). The blow is resolved once, as the wind-up gives way to
        // the strike, so a player who broke away during the telegraph escapes.
        if e.lunge > 0.0 {
            let was_winding = e.lunge > ORC_SLAM_STRIKE_TIME;
            e.lunge = (e.lunge - TICK_DT).max(0.0);
            let striking = e.lunge <= ORC_SLAM_STRIKE_TIME;

            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, 0.0, 0.0, true);
            e.x = m.x;
            e.y = m.y;
            e.vx = 0.0;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: 0.0,
                vy: m.vy,
            });

            // The slam connects on the tick the wind-up ends, if a player or knight
            // is still within the (generous) slam reach.
            if was_winding && striking {
                if let Some(p) = nearest_prey(
                    &hostile_players,
                    &knight_boxes,
                    m.x + w * 0.5,
                    m.y + h * 0.5,
                    f32::INFINITY,
                ) {
                    if aabb_gap(m.x, m.y, w, h, p.x, p.y, p.w, p.h) <= ORC_SLAM_REACH {
                        let kdir = if p.x + p.w * 0.5 >= m.x + w * 0.5 {
                            1.0
                        } else {
                            -1.0
                        };
                        hit_prey(
                            &mut bites,
                            &mut knight_hits,
                            &p,
                            (kdir * KNOCKBACK_X, -KNOCKBACK_Y),
                            ORC_SLAM_DAMAGE,
                        );
                    }
                }
            }
            continue;
        }

        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, ORC_AGGRO);

        // In range and off cooldown: commit to a wind-up slam. Hold still (facing is
        // already pointed at the quarry from the lumber in) and tell every client to
        // play the slam animation.
        if let Some(p) = target {
            if e.attack_cd <= 0.0 && aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h) <= ORC_SLAM_RANGE
            {
                e.lunge = crate::entity::ORC_SLAM_TIME;
                e.attack_cd = ORC_SLAM_INTERVAL;
                e.vx = 0.0;
                broadcasts.push(ServerMessage::EntityLunging { id });
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: e.x,
                    y: e.y,
                    vx: 0.0,
                    vy: e.vy,
                });
                continue;
            }
        }

        // Otherwise lumber toward the target, or wander its home patch.
        let chasing = target.is_some();
        let dir = match target {
            Some(p) if p.x + p.w * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };
        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, ORC_SPEED, chasing);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }

    // Minotaurs: the underworld's rare horned miniboss. A ground brute that normally
    // **hulks slowly** after the player, but commits — on a cooldown — to one of two
    // telegraphed attacks. Its **jump slam**: it leaps (toward the player), hangs at the
    // apex, then crashes down — the landing sends out a shockwave that damages anyone
    // left standing on the ground, so a player who is airborne (mid-jump) when it lands
    // is spared. Its **headbutt charge**: it lowers its head and barrels in fast along
    // the ground to gore whoever it reaches. Active at all hours (the underworld is
    // always dark). The slam rides the `lunge` timer (with `lunge_dir` doubling as a
    // "landing already dealt" latch); the charge rides the `flee` timer (with `lunge_dir`
    // holding its locked heading).
    for id in minotaur_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);

        // Mid jump-slam: a committed leap that plays out over MINOTAUR_SLAM_TIME. It
        // rises and hangs under reduced gravity while the timer is high, then — once the
        // timer crosses the drop threshold — slams straight down, the blow landing the
        // tick it touches earth.
        if e.lunge > 0.0 {
            e.lunge = (e.lunge - TICK_DT).max(0.0);
            let dropping = e.lunge <= MINOTAUR_SLAM_DROP_TIME;
            if !dropping {
                // Ascent/hang: float up (then drift down) under a fraction of gravity.
                e.vy = (e.vy + GRAVITY * MINOTAUR_HANG_GRAVITY * TICK_DT).min(MAX_FALL);
            } else if e.vy < MINOTAUR_SLAM_VY {
                // Drop: commit to a hard crash downward.
                e.vy = MINOTAUR_SLAM_VY;
            }
            // Fly the leap freely, each axis stopped independently by walls.
            let (nx, _) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
            let (ny, on_ground) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
            e.x = nx;
            e.y = ny;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: nx,
                y: ny,
                vx: e.vx,
                vy: e.vy,
            });
            // Land the slam once, the tick it touches down during the drop: a shockwave
            // that catches every GROUNDED player (and any knight — they can't jump)
            // within reach. `lunge_dir` latches so the blow lands a single time.
            if dropping && on_ground && e.lunge_dir == 0.0 {
                e.lunge_dir = 1.0;
                e.vx = 0.0;
                let scx = nx + w * 0.5;
                let scy = ny + h * 0.5;
                for &(pid, px, py) in &hostile_players {
                    let (pw, ph) = PLAYER_SIZE;
                    if grounded(&mut world, px, py, pw, ph) {
                        let pcx = px + pw * 0.5;
                        let pcy = py + ph * 0.5;
                        if (pcx - scx).hypot(pcy - scy) <= MINOTAUR_SLAM_RADIUS {
                            let kdir = if pcx >= scx { 1.0 } else { -1.0 };
                            bites.push((
                                pid,
                                (kdir * KNOCKBACK_X, -KNOCKBACK_Y),
                                MINOTAUR_SLAM_DAMAGE,
                            ));
                        }
                    }
                }
                for &(kid, kx, ky, kw, kh) in &knight_boxes {
                    let kcx = kx + kw * 0.5;
                    let kcy = ky + kh * 0.5;
                    if (kcx - scx).hypot(kcy - scy) <= MINOTAUR_SLAM_RADIUS {
                        knight_hits.push((kid, MINOTAUR_SLAM_DAMAGE));
                    }
                }
            }
            continue;
        }

        // Mid headbutt charge: barrel along the ground in the locked heading until it
        // gores its target, slams into a wall, or the charge times out.
        if e.flee > 0.0 {
            e.flee = (e.flee - TICK_DT).max(0.0);
            let dir = e.lunge_dir;
            let prev_x = e.x;
            let m = step_ground(
                &mut world,
                (e.x, e.y, w, h),
                e.vy,
                dir,
                MINOTAUR_CHARGE_SPEED,
                true,
            );
            e.x = m.x;
            e.y = m.y;
            // Report the full charge speed (signed by heading) so the client blurs its
            // legs into a sprint; the actual position came from step_ground.
            e.vx = dir * MINOTAUR_CHARGE_SPEED;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: e.vx,
                vy: m.vy,
            });
            // Gore the first target within reach, ending the charge.
            let scx = m.x + w * 0.5;
            let scy = m.y + h * 0.5;
            if let Some(p) = nearest_prey(
                &hostile_players,
                &knight_boxes,
                scx,
                scy,
                MINOTAUR_CHARGE_RANGE,
            ) && aabb_gap(m.x, m.y, w, h, p.x, p.y, p.w, p.h) <= MINOTAUR_HEADBUTT_REACH
            {
                let kdir = if p.x + p.w * 0.5 >= scx { 1.0 } else { -1.0 };
                hit_prey(
                    &mut bites,
                    &mut knight_hits,
                    &p,
                    (kdir * KNOCKBACK_X, -KNOCKBACK_Y),
                    MINOTAUR_HEADBUTT_DAMAGE,
                );
                e.flee = 0.0;
            }
            // A charge that rammed a wall (made no horizontal headway) also ends.
            if (m.x - prev_x).abs() < 0.5 {
                e.flee = 0.0;
            }
            continue;
        }

        // Not mid-attack: decide whether to launch one, else hulk/wander.
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, MINOTAUR_AGGRO);
        if let Some(ref p) = target
            && e.attack_cd <= 0.0
            && grounded(&mut world, e.x, e.y, w, h)
        {
            let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
            let pcx = p.x + p.w * 0.5;
            let pcy = p.y + p.h * 0.5;
            let toward = if pcx >= scx { 1.0 } else { -1.0 };
            if gap <= MINOTAUR_SLAM_RANGE {
                // Jump slam: crouch and leap toward the player, hanging then crashing.
                e.lunge = crate::entity::MINOTAUR_SLAM_TIME;
                e.lunge_dir = 0.0;
                e.attack_cd = MINOTAUR_ATTACK_INTERVAL + crate::entity::MINOTAUR_SLAM_TIME;
                e.vy = MINOTAUR_JUMP_VY;
                e.vx = toward * MINOTAUR_LEAP_VX;
                broadcasts.push(ServerMessage::EntityLunging { id });
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: e.x,
                    y: e.y,
                    vx: e.vx,
                    vy: e.vy,
                });
                continue;
            } else if (pcy - scy).abs() <= MINOTAUR_CHARGE_LEVEL && gap <= MINOTAUR_CHARGE_RANGE {
                // Headbutt charge: lock the heading and barrel in (no wind-up pose — the
                // client reads the high reported speed and blurs its walk into a sprint).
                e.flee = MINOTAUR_CHARGE_TIME;
                e.lunge_dir = toward;
                e.attack_cd = MINOTAUR_ATTACK_INTERVAL + MINOTAUR_CHARGE_TIME;
                e.vx = toward * MINOTAUR_CHARGE_SPEED;
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: e.x,
                    y: e.y,
                    vx: e.vx,
                    vy: e.vy,
                });
                continue;
            }
        }

        // Otherwise lumber slowly toward the target, or wander its home patch.
        let chasing = target.is_some();
        let dir = match target {
            Some(p) if p.x + p.w * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };
        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            MINOTAUR_HULK_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }

    // Gargoyles: stone hoppers of the charred expanse. A gargoyle never walks — it sits
    // gathered on the ground, then **springs** in a short arc, chasing the player by
    // hopping toward them (or drifting across its home patch when alone). When a player
    // strays within slam range it commits to a **jump-slam**: a higher, harder leap
    // aimed to come down on top of them, the blow landing the tick it touches earth. The
    // slam rides the `lunge` timer (with `lunge_dir` doubling as a "landing already
    // dealt" latch); the sit-between-hops timer rides the otherwise-unused `flee` field.
    // Active at all hours (the underworld is always dark).
    for id in gargoyle_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        e.flee = (e.flee - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);

        // Mid jump-slam: a committed leap that flies ballistically until it touches down.
        // The blow lands once, the tick it lands during the descent — it tries to crash
        // onto whoever is within reach. `lunge_dir` latches so the slam lands a single
        // time.
        if e.lunge > 0.0 {
            e.lunge = (e.lunge - TICK_DT).max(0.0);
            e.vy = (e.vy + GRAVITY * TICK_DT).min(MAX_FALL);
            let (nx, _) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
            let (ny, on_ground) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
            e.x = nx;
            e.y = ny;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: nx,
                y: ny,
                vx: e.vx,
                vy: e.vy,
            });
            // `move_y` reports "blocked" for a ceiling bonk on the way up too, so only a
            // block while **descending** counts as touching down: that's the slam landing.
            if on_ground && e.vy >= 0.0 && e.lunge_dir == 0.0 {
                e.lunge_dir = 1.0;
                e.lunge = 0.0;
                e.vx = 0.0;
                e.vy = 0.0;
                let scx = nx + w * 0.5;
                if let Some(p) = nearest_prey(
                    &hostile_players,
                    &knight_boxes,
                    scx,
                    ny + h * 0.5,
                    GARGOYLE_AGGRO,
                ) && aabb_gap(nx, ny, w, h, p.x, p.y, p.w, p.h) <= GARGOYLE_SLAM_REACH
                {
                    let kdir = if p.x + p.w * 0.5 >= scx { 1.0 } else { -1.0 };
                    hit_prey(
                        &mut bites,
                        &mut knight_hits,
                        &p,
                        (kdir * KNOCKBACK_X, -KNOCKBACK_Y),
                        GARGOYLE_SLAM_DAMAGE,
                    );
                }
            } else if on_ground {
                // Clipped a ceiling mid-leap: stop rising so gravity can pull it back down.
                e.vy = 0.0;
            }
            continue;
        }

        // Airborne from an ordinary hop: fly the arc out under gravity, each axis stopped
        // by walls. It settles only when it comes down on a floor — a ceiling bonk on the
        // way up (also reported as "blocked") just halts the rise so it starts to fall.
        if !grounded(&mut world, e.x, e.y, w, h) {
            e.vy = (e.vy + GRAVITY * TICK_DT).min(MAX_FALL);
            let (nx, _) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
            let (ny, on_ground) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
            e.x = nx;
            e.y = ny;
            if on_ground && e.vy >= 0.0 {
                e.vx = 0.0;
                e.vy = 0.0;
            } else if on_ground {
                e.vy = 0.0;
            }
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: nx,
                y: ny,
                vx: e.vx,
                vy: e.vy,
            });
            continue;
        }

        // Grounded and gathered: decide whether to slam, hop, or keep sitting.
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, GARGOYLE_AGGRO);

        // A player within slam range and off cooldown: leap at them, aiming the arc to
        // come down on top of them.
        if let Some(ref p) = target
            && e.attack_cd <= 0.0
            && aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h) <= GARGOYLE_SLAM_RANGE
        {
            let dx = (p.x + p.w * 0.5) - scx;
            e.lunge = crate::entity::GARGOYLE_SLAM_TIME;
            e.lunge_dir = 0.0;
            e.attack_cd = GARGOYLE_ATTACK_INTERVAL + crate::entity::GARGOYLE_SLAM_TIME;
            e.vy = GARGOYLE_SLAM_VY;
            e.vx = (dx / GARGOYLE_SLAM_AIRTIME).clamp(-GARGOYLE_SLAM_MAX_VX, GARGOYLE_SLAM_MAX_VX);
            broadcasts.push(ServerMessage::EntityLunging { id });
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: e.x,
                y: e.y,
                vx: e.vx,
                vy: e.vy,
            });
            continue;
        }

        // Otherwise spring an ordinary hop toward the target (or across its home patch)
        // once the sit timer is up; between hops it stays gathered and still. The launch
        // is applied **this tick** (a gravity step then a move) so it leaves the ground
        // immediately — otherwise it would still read as grounded next tick and the
        // gathered-between-hops branch below would cancel the spring before it lifted off.
        if e.flee <= 0.0 {
            // Persist the roaming heading in `lunge_dir` (free while not slamming) so a
            // wandering gargoyle keeps drifting one way instead of jittering in place.
            let dir = match target {
                Some(ref p) if p.x + p.w * 0.5 < scx => -1.0,
                Some(_) => 1.0,
                None => wander_dir(scx, e.lunge_dir, home),
            };
            e.lunge_dir = dir;
            e.flee = GARGOYLE_HOP_INTERVAL;
            let vx = dir * GARGOYLE_HOP_VX;
            let vy = (GARGOYLE_HOP_VY + GRAVITY * TICK_DT).min(MAX_FALL);
            let (nx, _) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
            let (ny, on_ground) = move_y(&mut world, nx, e.y, w, h, vy * TICK_DT);
            e.x = nx;
            e.y = ny;
            e.vx = vx;
            e.vy = if on_ground { 0.0 } else { vy };
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: nx,
                y: ny,
                vx: e.vx,
                vy: e.vy,
            });
            continue;
        }

        // Gathered between hops: hold still, gravity keeping it pinned to the floor.
        e.vx = 0.0;
        e.vy = (e.vy + GRAVITY * TICK_DT).min(MAX_FALL);
        let (ny, on_ground) = move_y(&mut world, e.x, e.y, w, h, e.vy * TICK_DT);
        e.y = ny;
        if on_ground {
            e.vy = 0.0;
        }
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: e.x,
            y: ny,
            vx: 0.0,
            vy: e.vy,
        });
    }

    // Demon king: the arena boss. It opens the fight **on foot**, striding after the
    // player across the floor, and only once it is wounded past two-thirds health does
    // it take to the air — thereafter it FLIES after the player like an enchanted demon,
    // closing to melee range and hovering in their face. On either footing it commits,
    // on a cooldown, to one of four attacks — chosen at random and telegraphed by its
    // wind-up animation (riding the `lunge` timer, the chosen attack id stashed in
    // `lunge_dir`): a fan of five fireballs, a tighter spread of three magic fireballs,
    // a single summoner bolt, or — when the player is close — a heavy melee slam. The
    // bolts loose (and the slam lands) on the strike frame partway through the wind-up,
    // so an alert fighter can read the tell.
    //
    // Bolts loosed this tick, spawned after the loop so we aren't holding a mutable
    // borrow of the king while inserting. Each is `(projectile kind, x, y, vx, vy)`.
    let mut king_bolts: Vec<(EntityKind, f32, f32, f32, f32)> = Vec::new();
    // World-x to summon the king's dark-knight host around, set the instant it enrages
    // (spawned after the loop so we don't insert while borrowing the king).
    let mut summon_around: Option<f32> = None;
    let arena_seed = world.generator.seed();
    for id in demon_king_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        // The king fights on foot the entire bout — it never takes wing. Past
        // two-thirds health it enrages, calling down its host of dark knights.
        let enraged = e.health * 3 <= e.max_health * 2;
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        // Re-acquire the nearest target each tick (for both movement and facing).
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, DEMON_KING_AGGRO);
        let toward = match target {
            Some(ref p) => {
                if p.x + p.w * 0.5 >= scx {
                    1.0
                } else {
                    -1.0
                }
            }
            None => {
                if e.vx >= 0.0 {
                    1.0
                } else {
                    -1.0
                }
            }
        };

        // The instant the king enrages, it calls down a host of dark knights to swarm
        // the player alongside it — once per fight (the swap latches the flag).
        if enraged
            && !shared
                .demon_king_knights_summoned
                .swap(true, Ordering::SeqCst)
        {
            summon_around = Some(match target {
                Some(ref p) => p.x + p.w * 0.5,
                None => scx,
            });
        }

        // Mid-attack: a committed wind-up over DEMON_KING_ATTACK_TIME, resolving once on
        // the strike. The king stands planted through the wind-up (gravity still settles
        // it onto the floor), then looses its bolts or brings its fists down.
        if e.lunge > 0.0 {
            let was_winding = e.lunge > DEMON_KING_STRIKE_TIME;
            e.lunge = (e.lunge - TICK_DT).max(0.0);
            let striking = e.lunge <= DEMON_KING_STRIKE_TIME;
            {
                let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, 0.0, 0.0, true);
                e.x = m.x;
                e.y = m.y;
                e.vx = 0.0;
                e.vy = m.vy;
            }
            // Broadcast a non-zero vx sign so clients keep facing the player through
            // the wind-up (facing only updates on a non-zero vx).
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: e.x,
                y: e.y,
                vx: toward,
                vy: e.vy,
            });

            if was_winding && striking {
                let attack = e.lunge_dir as i32;
                let (fw, fh) = FIREBALL_SIZE;
                let sx = scx - fw * 0.5;
                let sy = e.y + h * 0.3 - fh * 0.5;
                // Aim from the king's upper body at the target's center (or straight
                // ahead if it has lost its quarry mid-cast).
                let (ux, uy) = match target {
                    Some(ref p) => {
                        let dx = (p.x + p.w * 0.5) - (sx + fw * 0.5);
                        let dy = (p.y + p.h * 0.5) - (sy + fh * 0.5);
                        let len = (dx * dx + dy * dy).sqrt().max(1.0);
                        (dx / len, dy / len)
                    }
                    None => (toward, 0.0),
                };
                // Loose the chosen attack. A `rot` helper fans a bolt off the aim
                // vector by a signed angle.
                let rot = |a: f32| -> (f32, f32) {
                    let (s, c) = a.sin_cos();
                    (ux * c - uy * s, ux * s + uy * c)
                };
                match attack {
                    // 0: a wide fan of five ordinary fireballs.
                    0 => {
                        for k in -2..=2 {
                            let (rx, ry) = rot(k as f32 * DEMON_KING_FIREBALL_SPREAD);
                            king_bolts.push((
                                EntityKind::Fireball,
                                sx,
                                sy,
                                rx * FIREBALL_SPEED,
                                ry * FIREBALL_SPEED,
                            ));
                        }
                    }
                    // 1: a tighter spread of three magic fireballs.
                    1 => {
                        for k in -1..=1 {
                            let (rx, ry) = rot(k as f32 * DEMON_KING_MAGIC_SPREAD);
                            king_bolts.push((
                                EntityKind::MagicFireball,
                                sx,
                                sy,
                                rx * MAGIC_FIREBALL_SPEED,
                                ry * MAGIC_FIREBALL_SPEED,
                            ));
                        }
                    }
                    // 2: a single summoner bolt that bursts into a bouncing skull.
                    2 => {
                        king_bolts.push((
                            EntityKind::SummonerFireball,
                            sx,
                            sy,
                            ux * SUMMONER_FIREBALL_SPEED,
                            uy * SUMMONER_FIREBALL_SPEED,
                        ));
                    }
                    // 3: a melee slam, landing only if a player is still in reach.
                    _ => {
                        if let Some(p) =
                            nearest_prey(&hostile_players, &knight_boxes, scx, scy, f32::INFINITY)
                            && aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h) <= DEMON_KING_SLAM_REACH
                        {
                            let kdir = if p.x + p.w * 0.5 >= scx { 1.0 } else { -1.0 };
                            hit_prey(
                                &mut bites,
                                &mut knight_hits,
                                &p,
                                (kdir * KNOCKBACK_X, -KNOCKBACK_Y),
                                DEMON_KING_SLAM_DAMAGE,
                            );
                        }
                    }
                }
            }
            continue;
        }

        // Begin an attack if a target is in range and the cooldown has elapsed.
        if target.is_some() && e.attack_cd <= 0.0 {
            // Pick one of the four attacks. Derived from the seed, the king's
            // position, and a rolling per-king counter (stashed in the otherwise
            // unused `flee` field) so the choice varies bout to bout even when the
            // king stands stock-still over a stationary target.
            let salt = id.wrapping_add((e.flee as u32).wrapping_mul(2_654_435_761));
            e.flee = (e.flee + 1.0) % 1_000_000.0;
            let roll = chunk_hash(arena_seed, e.x as i32, e.y as i32, salt);
            // Pick one of its four attacks (0/1/2 ranged, 3 the melee slam) evenly.
            let attack = roll % 4;
            e.lunge = crate::entity::DEMON_KING_ATTACK_TIME;
            e.lunge_dir = attack as f32;
            e.attack_cd = DEMON_KING_ATTACK_INTERVAL;
            e.vx = 0.0;
            broadcasts.push(ServerMessage::EntityLunging { id });
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: e.x,
                y: e.y,
                vx: toward,
                vy: e.vy,
            });
            continue;
        }

        // Not attacking: stride toward the target across the floor (gravity-bound), or
        // wander its home patch when it has no quarry.
        {
            let chasing = target.is_some();
            let dir = match target {
                Some(ref p) if p.x + p.w * 0.5 < scx => -1.0,
                Some(_) => 1.0,
                None => wander_dir(scx, e.vx, home),
            };
            let m = step_ground(
                &mut world,
                (e.x, e.y, w, h),
                e.vy,
                dir,
                DEMON_KING_SPEED,
                chasing,
            );
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            // Face the target while keeping the true speed so the walk cycle plays.
            let bcast_vx = toward * m.vx.abs();
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: bcast_vx,
                vy: m.vy,
            });
        }
    }
    // Spawn the bolts the king loosed this tick, each with its own lifetime (they are
    // not in `fireball_ids`/`summoner_fireball_ids`, so they begin flying next tick).
    for (kind, x, y, vx, vy) in king_bolts {
        let life = match kind {
            EntityKind::MagicFireball => MAGIC_FIREBALL_LIFETIME,
            EntityKind::SummonerFireball => SUMMONER_FIREBALL_LIFETIME,
            _ => FIREBALL_LIFETIME,
        };
        let bid = shared.alloc_id();
        let mut bolt = Entity::new(bid, kind, x, y);
        bolt.vx = vx;
        bolt.vy = vy;
        bolt.attack_cd = life; // reused as the airborne lifetime timer
        entities.insert(bolt.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: bolt });
    }
    // Spawn the host the king called down as it enraged — two dark knights and two dark
    // musketeers — fanned out across the floor around the player to swarm them alongside
    // the king. The two kinds are interleaved so the marksmen don't all cluster on one
    // flank, and each is dropped onto its own column's surface.
    if let Some(px) = summon_around {
        let mut host: Vec<EntityKind> = Vec::new();
        for _ in 0..DEMON_KING_DARK_KNIGHT_COUNT {
            host.push(EntityKind::DarkKnight);
        }
        for _ in 0..DEMON_KING_DARK_MUSKETEER_COUNT {
            host.push(EntityKind::DarkMusketeer);
        }
        for (i, kind) in host.into_iter().enumerate() {
            let i = i as u32;
            let rank = (i / 2) as f32 + 1.0;
            let dir = if i % 2 == 0 { -1.0 } else { 1.0 };
            let gx = px + dir * rank * DEMON_KING_KNIGHT_SPACING;
            let gcx = (gx / TILE_SIZE).floor() as i32;
            let surface = world.surface(gcx);
            let (_, kh) = kind.size();
            let gy = surface as f32 * TILE_SIZE - kh;
            let warrior = Entity::new(shared.alloc_id(), kind, gx, gy);
            entities.insert(warrior.clone());
            broadcasts.push(ServerMessage::EntitySpawn { entity: warrior });
        }
    }

    // Twinscale: the post-game arena superboss — a huge twin-headed dragon that holds
    // station HIGH beneath the room's ceiling and rains its arsenal down, chosen at
    // random and telegraphed by a wind-up (riding `lunge`, the chosen attack stashed in
    // `lunge_dir`): a wide fan of ten fireballs, a spread of six magic fireballs, or two
    // summoner bolts. It flies straight at the challenger to hover right overhead (at a
    // fixed altitude — it never descends), so you cannot park in one spot and let every
    // volley miss, and at half health calls down a flight of dragons. Because it never
    // descends, only a flying challenger (the dragonian steed) can fight it.
    //
    // Bolts loosed this tick and the room-x to summon dragons around, both applied after
    // the loop so we aren't holding a mutable borrow of the boss while inserting.
    let mut twinscale_bolts: Vec<(EntityKind, f32, f32, f32, f32)> = Vec::new();
    let mut twinscale_summon: Option<f32> = None;
    let ts_seed = world.generator.seed();
    let ts_hover_y = arena_room_ceiling_y() + TWINSCALE_HOVER_DROP;
    for id in twinscale_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, TWINSCALE_AGGRO);
        let toward = match target {
            Some(ref p) => {
                if p.x + p.w * 0.5 >= scx {
                    1.0
                } else {
                    -1.0
                }
            }
            None => {
                if e.vx >= 0.0 {
                    1.0
                } else {
                    -1.0
                }
            }
        };

        // Half health: call down a flight of dragons to fight alongside it — once.
        if e.health * 2 <= e.max_health
            && !shared
                .twinscale_dragons_summoned
                .swap(true, Ordering::SeqCst)
        {
            twinscale_summon = Some(scx);
        }

        // Directly below: pour an unbroken stream of magic fireballs straight down — no
        // wind-up, no cooldown, a fresh bolt every tick for as long as the challenger
        // cowers in the blind spot a fan would arc over. This takes priority over (and
        // cancels) any rolled fan, while the boss still drifts to stay overhead. Sliding
        // out from under it past TWINSCALE_BELOW_THRESHOLD is the only way to break it.
        if let Some(ref p) = target {
            let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
            let pcx = p.x + p.w * 0.5;
            let below = (pcx - scx).abs() <= TWINSCALE_BELOW_THRESHOLD
                && (p.y + p.h * 0.5) > scy
                && gap <= TWINSCALE_SHOOT_RANGE;
            if below {
                e.lunge = 0.0;
                let (mw, _mh) = MAGIC_FIREBALL_SIZE;
                twinscale_bolts.push((
                    EntityKind::MagicFireball,
                    pcx - mw * 0.5,
                    scy,
                    0.0,
                    MAGIC_FIREBALL_SPEED,
                ));
                // Hold station overhead: track horizontally and correct back to altitude.
                let hx = pcx - scx;
                let vx = if hx.abs() > TWINSCALE_PURSUE_DIST {
                    hx.signum() * TWINSCALE_SPEED
                } else {
                    0.0
                };
                let vy = (ts_hover_y - e.y).clamp(-TWINSCALE_SPEED, TWINSCALE_SPEED);
                let (nx, _) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
                let (ny, _) = move_y(&mut world, nx, e.y, w, h, vy * TICK_DT);
                e.x = nx;
                e.y = ny;
                e.vx = vx;
                e.vy = vy;
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: nx,
                    y: ny,
                    vx: toward * vx.abs().max(1.0),
                    vy,
                });
                continue;
            }
        }

        // Mid-attack: a committed wind-up over TWINSCALE_ATTACK_TIME, holding station and
        // correcting back to its hover altitude, then loosing its bolts on the strike.
        if e.lunge > 0.0 {
            let was_winding = e.lunge > TWINSCALE_STRIKE_TIME;
            e.lunge = (e.lunge - TICK_DT).max(0.0);
            let striking = e.lunge <= TWINSCALE_STRIKE_TIME;
            let vy = (ts_hover_y - e.y).clamp(-TWINSCALE_SPEED, TWINSCALE_SPEED);
            let (ny, _) = move_y(&mut world, e.x, e.y, w, h, vy * TICK_DT);
            e.y = ny;
            e.vx = 0.0;
            e.vy = vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: e.x,
                y: e.y,
                vx: toward,
                vy,
            });

            if was_winding && striking {
                let attack = e.lunge_dir as i32;
                let (fw, fh) = FIREBALL_SIZE;
                let sx = scx - fw * 0.5;
                let sy = scy - fh * 0.5;
                // Aim from Twinscale's body at the target's center, or straight down if
                // it has lost its quarry mid-cast (it fires from on high).
                let (ux, uy) = match target {
                    Some(ref p) => {
                        let dx = (p.x + p.w * 0.5) - (sx + fw * 0.5);
                        let dy = (p.y + p.h * 0.5) - (sy + fh * 0.5);
                        let len = (dx * dx + dy * dy).sqrt().max(1.0);
                        (dx / len, dy / len)
                    }
                    None => (0.0, 1.0),
                };
                let rot = |a: f32| -> (f32, f32) {
                    let (s, c) = a.sin_cos();
                    (ux * c - uy * s, ux * s + uy * c)
                };
                // A symmetric fan of `n` bolts at `speed`, evenly spread about the aim.
                let mut fan = |kind: EntityKind, n: i32, spread: f32, speed: f32| {
                    for i in 0..n {
                        let off = (i as f32 - (n as f32 - 1.0) * 0.5) * spread;
                        let (rx, ry) = rot(off);
                        twinscale_bolts.push((kind.clone(), sx, sy, rx * speed, ry * speed));
                    }
                };
                match attack {
                    0 => fan(
                        EntityKind::Fireball,
                        TWINSCALE_FIREBALL_COUNT,
                        TWINSCALE_FIREBALL_SPREAD,
                        FIREBALL_SPEED,
                    ),
                    1 => fan(
                        EntityKind::MagicFireball,
                        TWINSCALE_MAGIC_COUNT,
                        TWINSCALE_MAGIC_SPREAD,
                        MAGIC_FIREBALL_SPEED,
                    ),
                    _ => fan(
                        EntityKind::SummonerFireball,
                        TWINSCALE_SUMMONER_COUNT,
                        TWINSCALE_SUMMONER_SPREAD,
                        SUMMONER_FIREBALL_SPEED,
                    ),
                }
            }
            continue;
        }

        // Begin an attack if a target is within range and the cooldown has elapsed.
        if let Some(ref p) = target {
            let gap = aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h);
            if e.attack_cd <= 0.0 && gap <= TWINSCALE_SHOOT_RANGE {
                let salt = id.wrapping_add((e.flee as u32).wrapping_mul(2_654_435_761));
                e.flee = (e.flee + 1.0) % 1_000_000.0;
                let roll = chunk_hash(ts_seed, e.x as i32, e.y as i32, salt);
                e.lunge = crate::entity::TWINSCALE_ATTACK_TIME;
                e.lunge_dir = (roll % 3) as f32;
                e.attack_cd = TWINSCALE_ATTACK_INTERVAL;
                e.vx = 0.0;
                broadcasts.push(ServerMessage::EntityLunging { id });
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: e.x,
                    y: e.y,
                    vx: toward,
                    vy: e.vy,
                });
                continue;
            }
        }

        // Not attacking: hold its high altitude and fly straight at the target, closing
        // until it hovers right overhead — never descending to the floor. Running you
        // down like this is what denies a static dodge spot; only a small deadzone keeps
        // it from jittering back and forth once it is on top of you.
        let vx = match target {
            Some(ref p) => {
                let hx = (p.x + p.w * 0.5) - scx;
                let hdir = if hx >= 0.0 { 1.0 } else { -1.0 };
                if hx.abs() > TWINSCALE_PURSUE_DIST {
                    hdir * TWINSCALE_SPEED
                } else {
                    0.0
                }
            }
            None => wander_dir(scx, e.vx, home) * TWINSCALE_SPEED,
        };
        let vy = (ts_hover_y - e.y).clamp(-TWINSCALE_SPEED, TWINSCALE_SPEED);
        let (nx, _) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
        let (ny, _) = move_y(&mut world, nx, e.y, w, h, vy * TICK_DT);
        e.x = nx;
        e.y = ny;
        e.vx = vx;
        e.vy = vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: toward * vx.abs().max(1.0),
            vy,
        });
    }
    // Spawn the bolts Twinscale loosed this tick (each with its own lifetime; not in any
    // projectile id list, so they begin flying next tick).
    for (kind, x, y, vx, vy) in twinscale_bolts {
        let life = match kind {
            EntityKind::MagicFireball => MAGIC_FIREBALL_LIFETIME,
            EntityKind::SummonerFireball => SUMMONER_FIREBALL_LIFETIME,
            _ => FIREBALL_LIFETIME,
        };
        let bid = shared.alloc_id();
        let mut bolt = Entity::new(bid, kind, x, y);
        bolt.vx = vx;
        bolt.vy = vy;
        bolt.attack_cd = life; // reused as the airborne lifetime timer
        entities.insert(bolt.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: bolt });
    }
    // Spawn the flight of dragons Twinscale called down at half health, fanned across the
    // upper room just below it, soaring in to harry the challenger.
    if let Some(cx) = twinscale_summon {
        let (dw, _dh) = EntityKind::Dragon.size();
        let dy = ts_hover_y + 4.0 * TILE_SIZE;
        for i in 0..TWINSCALE_DRAGON_COUNT {
            let rank = (i / 2) as f32 + 1.0;
            let dir = if i % 2 == 0 { -1.0 } else { 1.0 };
            let gx = cx + dir * rank * TWINSCALE_DRAGON_SPACING - dw * 0.5;
            let dragon = Entity::new(shared.alloc_id(), EntityKind::Dragon, gx, dy);
            entities.insert(dragon.clone());
            broadcasts.push(ServerMessage::EntitySpawn { entity: dragon });
        }
    }

    // Orc mages: robed underworld support casters. They land no blows of their own —
    // they shy away from players and instead seek out ordinary demons to empower,
    // casting an enchant that turns a demon into a flying, harder-hitting enchanted
    // demon. The cast plays out over ORC_MAGE_CAST_TIME (riding the `lunge` timer); the
    // demon is empowered when the cast is kicked off.
    //
    // Snapshot of ordinary (un-enchanted) demons as `(id, x, y, w, h)`, so a mage can
    // seek one out without re-borrowing the map mid-loop.
    let plain_demons: Vec<(EntityId, f32, f32, f32, f32)> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Demon))
        .map(|e| {
            let (w, h) = e.size();
            (e.id, e.x, e.y, w, h)
        })
        .collect();
    // Demons a mage commits to enchanting this tick, converted after the loop so we
    // aren't holding a mutable borrow of the mage while mutating its target.
    let mut to_enchant: Vec<EntityId> = Vec::new();
    for id in orc_mage_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);

        // Mid-cast: the enchant gesture plays out in place (gravity still settles it
        // onto the floor). The demon was empowered when the cast began, so here we
        // only play out the animation.
        if e.lunge > 0.0 {
            e.lunge = (e.lunge - TICK_DT).max(0.0);
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, 0.0, 0.0, true);
            e.x = m.x;
            e.y = m.y;
            e.vx = 0.0;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: 0.0,
                vy: m.vy,
            });
            continue;
        }

        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        // A player too close: the mage can't fight, so fleeing takes priority over
        // shepherding demons — it backs away along the ground.
        let threat = nearest_prey(&hostile_players, &knight_boxes, scx, scy, ORC_MAGE_AGGRO);
        if let Some(p) =
            threat.filter(|p| aabb_gap(e.x, e.y, w, h, p.x, p.y, p.w, p.h) < ORC_MAGE_FLEE_DIST)
        {
            let dir = if p.x + p.w * 0.5 >= scx { -1.0 } else { 1.0 };
            let m = step_ground(
                &mut world,
                (e.x, e.y, w, h),
                e.vy,
                dir,
                ORC_MAGE_SPEED,
                true,
            );
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        // Otherwise seek out the nearest ordinary demon (by center distance) to empower.
        let mut best: Option<(usize, f32)> = None;
        for (i, &(_, dx, dy, dw, dh)) in plain_demons.iter().enumerate() {
            let ddx = (dx + dw * 0.5) - scx;
            let ddy = (dy + dh * 0.5) - scy;
            let d2 = ddx * ddx + ddy * ddy;
            if d2 <= ORC_MAGE_ASSIST_RANGE * ORC_MAGE_ASSIST_RANGE
                && best.is_none_or(|(_, bd)| d2 < bd)
            {
                best = Some((i, d2));
            }
        }

        if let Some((i, _)) = best {
            let (did, dx, dy, dw, dh) = plain_demons[i];
            if aabb_gap(e.x, e.y, w, h, dx, dy, dw, dh) <= ORC_MAGE_ENCHANT_RANGE {
                // In reach: cast if off cooldown, else just hold still beside its charge.
                if e.attack_cd <= 0.0 {
                    e.lunge = crate::entity::ORC_MAGE_CAST_TIME;
                    e.attack_cd = ORC_MAGE_ENCHANT_INTERVAL;
                    e.vx = 0.0;
                    to_enchant.push(did);
                    broadcasts.push(ServerMessage::EntityLunging { id });
                    broadcasts.push(ServerMessage::EntityMoved {
                        id,
                        x: e.x,
                        y: e.y,
                        vx: 0.0,
                        vy: e.vy,
                    });
                    continue;
                }
                let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, 0.0, 0.0, false);
                e.x = m.x;
                e.y = m.y;
                e.vx = m.vx;
                e.vy = m.vy;
                broadcasts.push(ServerMessage::EntityMoved {
                    id,
                    x: m.x,
                    y: m.y,
                    vx: m.vx,
                    vy: m.vy,
                });
                continue;
            }
            // Out of reach: lumber toward the demon to close the gap.
            let dir = if dx + dw * 0.5 < scx { -1.0 } else { 1.0 };
            let m = step_ground(
                &mut world,
                (e.x, e.y, w, h),
                e.vy,
                dir,
                ORC_MAGE_SPEED,
                true,
            );
            e.x = m.x;
            e.y = m.y;
            e.vx = m.vx;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: m.vx,
                vy: m.vy,
            });
            continue;
        }

        // Nothing to flee and no demon to shepherd: wander its home patch.
        let dir = wander_dir(scx, e.vx, home);
        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            ORC_MAGE_SPEED,
            false,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }
    // Empower the demons the mages cast on this tick: an ordinary demon becomes an
    // enchanted one, healed to its new (higher) full as the glamour takes hold.
    // Re-broadcasting its full description (an EntitySpawn for an existing id updates
    // it on every client) re-tags it as the enchanted kind.
    for did in to_enchant {
        if let Some(d) = entities.get_mut(did) {
            if matches!(d.kind, EntityKind::Demon) {
                d.kind = EntityKind::EnchantedDemon;
                d.max_health = crate::entity::ENCHANTED_DEMON_MAX_HEALTH;
                d.health = d.max_health;
                d.attack_cd = 0.0;
                let updated = d.clone();
                broadcasts.push(ServerMessage::EntitySpawn { entity: updated });
            }
        }
    }

    // Ash twisters: whirling columns of ash that haunt the ash valleys. They drift
    // toward players at all hours (the underworld is always dark) and, on contact,
    // fling the player high into the air rather than biting — the punishing fall
    // back down is what hurts. The contact buffeting itself deals only a token blow.
    for id in ash_twister_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        let target = nearest_prey(&hostile_players, &knight_boxes, scx, scy, ASH_TWISTER_AGGRO);
        let chasing = target.is_some();
        let dir = match target {
            Some(p) if p.x + p.w * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            ASH_TWISTER_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });

        // In reach and off cooldown: catch the player and hurl them skyward. The big
        // upward knockback (and a sideways toss) rides the EntityHit to the owning
        // client, which launches its avatar — the fall that follows deals the real
        // damage (see the client's fall-damage handling).
        if let Some(p) = target {
            if e.attack_cd <= 0.0
                && aabb_gap(m.x, m.y, w, h, p.x, p.y, p.w, p.h) <= ASH_TWISTER_ATTACK_RANGE
            {
                e.attack_cd = ASH_TWISTER_ATTACK_INTERVAL;
                let kdir = if p.x + p.w * 0.5 >= m.x + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                // A player is flung skyward (the fall does the real damage); a
                // server-simulated knight can't be launched, so via `hit_prey` it
                // just soaks the token buffeting.
                hit_prey(
                    &mut bites,
                    &mut knight_hits,
                    &p,
                    (kdir * ASH_TWISTER_TOSS, -ASH_TWISTER_LAUNCH),
                    ASH_TWISTER_DAMAGE,
                );
            }
        }
    }

    // Bones in flight: travel in a straight line (no gravity), striking the first
    // player they overlap or winking out on a wall or when their short life ends.
    let mut bone_despawns: Vec<EntityId> = Vec::new();
    for id in bone_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);

        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;

        // Struck a wall, or flew long enough without hitting anything: gone.
        if hit_x || hit_y || e.attack_cd <= 0.0 {
            bone_despawns.push(id);
            continue;
        }

        // Struck a player or knight: deal damage, knock them along the bone's
        // flight (a knight soaks it via `knight_hits`), gone.
        let kx = if e.vx >= 0.0 {
            KNOCKBACK_X
        } else {
            -KNOCKBACK_X
        };
        let mut struck = false;
        for &(pid, px, py) in &players {
            if aabb_gap(nx, ny, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1) <= BONE_HIT_RANGE {
                bites.push((pid, (kx, -KNOCKBACK_Y), BONE_DAMAGE));
                struck = true;
                break;
            }
        }
        if !struck {
            for &(kid, kx2, ky, kw, kh) in &knight_boxes {
                if aabb_gap(nx, ny, w, h, kx2, ky, kw, kh) <= BONE_HIT_RANGE {
                    knight_hits.push((kid, BONE_DAMAGE));
                    struck = true;
                    break;
                }
            }
        }
        if struck {
            bone_despawns.push(id);
            continue;
        }

        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });
    }
    for id in bone_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Axes in flight: like bones they travel in a straight line (no gravity), striking
    // the first player or knight they overlap (a knight soaks it via `knight_hits`) or
    // winking out on a wall or when their short life ends. They tumble end over end (a
    // multi-frame sprite), leaving nothing behind where they land.
    let mut axe_despawns: Vec<EntityId> = Vec::new();
    for id in axe_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);

        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;

        // Struck a wall, or flew long enough without hitting anything: gone.
        if hit_x || hit_y || e.attack_cd <= 0.0 {
            axe_despawns.push(id);
            continue;
        }

        // Struck a player or knight: deal damage, knock them along the axe's flight (a
        // knight soaks it via `knight_hits`), gone.
        let kx = if e.vx >= 0.0 {
            KNOCKBACK_X
        } else {
            -KNOCKBACK_X
        };
        let mut struck = false;
        for &(pid, px, py) in &players {
            if aabb_gap(nx, ny, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1) <= AXE_HIT_RANGE {
                bites.push((pid, (kx, -KNOCKBACK_Y), AXE_DAMAGE));
                struck = true;
                break;
            }
        }
        if !struck {
            for &(kid, kx2, ky, kw, kh) in &knight_boxes {
                if aabb_gap(nx, ny, w, h, kx2, ky, kw, kh) <= AXE_HIT_RANGE {
                    knight_hits.push((kid, AXE_DAMAGE));
                    struck = true;
                    break;
                }
            }
        }
        if struck {
            axe_despawns.push(id);
            continue;
        }

        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });
    }
    for id in axe_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Hostile bullets in flight: a dark musketeer's shots. Like axes they travel in a
    // straight line (no gravity), striking the first player or warrior they overlap (a
    // warrior soaks it via `knight_hits`) or winking out on a wall or when their short
    // life ends. They leave nothing behind where they land.
    let mut bullet_despawns: Vec<EntityId> = Vec::new();
    for id in bullet_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);

        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;

        if hit_x || hit_y || e.attack_cd <= 0.0 {
            bullet_despawns.push(id);
            continue;
        }

        let kx = if e.vx >= 0.0 {
            KNOCKBACK_X
        } else {
            -KNOCKBACK_X
        };
        let mut struck = false;
        for &(pid, px, py) in &players {
            if aabb_gap(nx, ny, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1) <= BULLET_HIT_RANGE {
                bites.push((pid, (kx, -KNOCKBACK_Y), BULLET_DAMAGE));
                struck = true;
                break;
            }
        }
        if !struck {
            for &(kid, kx2, ky, kw, kh) in &knight_boxes {
                if aabb_gap(nx, ny, w, h, kx2, ky, kw, kh) <= BULLET_HIT_RANGE {
                    knight_hits.push((kid, BULLET_DAMAGE));
                    struck = true;
                    break;
                }
            }
        }
        if struck {
            bullet_despawns.push(id);
            continue;
        }

        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });
    }
    for id in bullet_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Friendly bullets in flight: the shots a musketeer or a player's musket looses.
    // Like the hostile bullet they fly straight (no gravity), but they help the caster —
    // damaging the *monster* they strike rather than players, and knights and monsters
    // pay them no mind. They leave nothing behind where they land.
    let mut friendly_bullet_despawns: Vec<EntityId> = Vec::new();
    for id in friendly_bullet_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);

        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;

        if hit_x || hit_y || e.attack_cd <= 0.0 {
            friendly_bullet_despawns.push(id);
            continue;
        }

        let kx = if e.vx >= 0.0 {
            KNOCKBACK_X
        } else {
            -KNOCKBACK_X
        };
        let mut struck = false;
        for &(tid, tx, ty, tw, th) in &hostiles {
            if aabb_gap(nx, ny, w, h, tx, ty, tw, th) <= BULLET_HIT_RANGE {
                creature_hits.push((tid, (kx, -KNOCKBACK_Y), FRIENDLY_BULLET_DAMAGE));
                struck = true;
                break;
            }
        }
        if struck {
            friendly_bullet_despawns.push(id);
            continue;
        }

        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });
    }
    for id in friendly_bullet_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Fireballs in flight: like bones they travel in a straight line (no gravity),
    // striking the first player they overlap or winking out on a wall or when their
    // short life ends — but where they burst they leave a lick of fire behind.
    let mut fireball_despawns: Vec<EntityId> = Vec::new();
    for id in fireball_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        // The magic bolt hits harder and over a touch more reach than the ordinary one.
        let (dmg, hit_range) = if matches!(e.kind, EntityKind::MagicFireball) {
            (MAGIC_FIREBALL_DAMAGE, MAGIC_FIREBALL_HIT_RANGE)
        } else {
            (FIREBALL_DAMAGE, FIREBALL_HIT_RANGE)
        };

        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;

        // Did it strike a player or knight this tick? Damage and knock them along
        // its flight (a knight soaks it via `knight_hits`).
        let kx = if e.vx >= 0.0 {
            KNOCKBACK_X
        } else {
            -KNOCKBACK_X
        };
        let mut struck = false;
        for &(pid, px, py) in &players {
            if aabb_gap(nx, ny, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1) <= hit_range {
                bites.push((pid, (kx, -KNOCKBACK_Y), dmg));
                struck = true;
                break;
            }
        }
        if !struck {
            for &(kid, kx2, ky, kw, kh) in &knight_boxes {
                if aabb_gap(nx, ny, w, h, kx2, ky, kw, kh) <= hit_range {
                    knight_hits.push((kid, dmg));
                    struck = true;
                    break;
                }
            }
        }

        // Burst on a wall, on a player or knight, or when its life runs out: leave a
        // tongue of fire in the (empty) cell where it died, then despawn. Only when
        // that cell rests on solid ground, though — a fireball that bursts in open air
        // no longer hangs a flame in the void (its summoner counterpart still bursts
        // into a skull, untouched by this).
        if hit_x || hit_y || struck || e.attack_cd <= 0.0 {
            let fx = ((nx + w * 0.5) / TILE_SIZE).floor() as i32;
            let fy = ((ny + h * 0.5) / TILE_SIZE).floor() as i32;
            if world.solid(fx, fy + 1)
                && world.get(fx, fy) == crate::block::AIR
                && world.set(fx, fy, crate::block::FIRE)
            {
                new_trail_fires.push((fx, fy));
                broadcasts.push(ServerMessage::BlockUpdate {
                    dim,
                    x: fx,
                    y: fy,
                    block: crate::block::FIRE,
                });
            }
            fireball_despawns.push(id);
            continue;
        }

        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });
    }
    for id in fireball_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Friendly dragon fireballs in flight: the bolts a player's white-dragon steed
    // breathes. Like the hostile fireball they fly straight (no gravity) and leave a
    // tongue of fire where they burst — but they help the caster, damaging the
    // *monster* they strike rather than players, and knights and monsters pay them no
    // mind.
    let mut friendly_dragon_fireball_despawns: Vec<EntityId> = Vec::new();
    for id in friendly_dragon_fireball_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);

        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;

        // Did it strike a monster this tick? Damage and knock it along the flight.
        let kx = if e.vx >= 0.0 {
            KNOCKBACK_X
        } else {
            -KNOCKBACK_X
        };
        let mut struck = false;
        for &(tid, tx, ty, tw, th) in &hostiles {
            if aabb_gap(nx, ny, w, h, tx, ty, tw, th) <= FRIENDLY_DRAGON_FIREBALL_HIT_RANGE {
                creature_hits.push((tid, (kx, -KNOCKBACK_Y), FRIENDLY_DRAGON_FIREBALL_DAMAGE));
                struck = true;
                break;
            }
        }

        // Burst on a wall, on a monster, or when its life runs out: leave a tongue of
        // fire in the (empty) cell where it died, then despawn.
        if hit_x || hit_y || struck || e.attack_cd <= 0.0 {
            let fx = ((nx + w * 0.5) / TILE_SIZE).floor() as i32;
            let fy = ((ny + h * 0.5) / TILE_SIZE).floor() as i32;
            if world.get(fx, fy) == crate::block::AIR && world.set(fx, fy, crate::block::FIRE) {
                new_trail_fires.push((fx, fy));
                broadcasts.push(ServerMessage::BlockUpdate {
                    dim,
                    x: fx,
                    y: fy,
                    block: crate::block::FIRE,
                });
            }
            friendly_dragon_fireball_despawns.push(id);
            continue;
        }

        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });
    }
    for id in friendly_dragon_fireball_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Summoner fireballs in flight: like the others they travel in a straight line
    // (no gravity), but where they burst — on a wall, on a player, or when their life
    // runs out — they summon a bouncing skull rather than leaving fire behind.
    let mut summoner_despawns: Vec<EntityId> = Vec::new();
    // Skulls conjured this tick, as `(x, y, vx_sign)`, spawned after the loop so we
    // aren't holding a mutable borrow of the bursting fireball while inserting.
    let mut skull_summons: Vec<(f32, f32, f32)> = Vec::new();
    for id in summoner_fireball_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);

        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;

        // Did it directly strike a player or knight this tick? Light damage and a knock.
        let kx = if e.vx >= 0.0 {
            KNOCKBACK_X
        } else {
            -KNOCKBACK_X
        };
        let mut struck = false;
        for &(pid, px, py) in &players {
            if aabb_gap(nx, ny, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1)
                <= SUMMONER_FIREBALL_HIT_RANGE
            {
                bites.push((pid, (kx, -KNOCKBACK_Y), SUMMONER_FIREBALL_DAMAGE));
                struck = true;
                break;
            }
        }
        if !struck {
            for &(kid, kx2, ky, kw, kh) in &knight_boxes {
                if aabb_gap(nx, ny, w, h, kx2, ky, kw, kh) <= SUMMONER_FIREBALL_HIT_RANGE {
                    knight_hits.push((kid, SUMMONER_FIREBALL_DAMAGE));
                    struck = true;
                    break;
                }
            }
        }

        // Burst on a wall, on a player or knight, or when its life runs out: summon a
        // skull where it died (popping up in its flight direction), then despawn.
        if hit_x || hit_y || struck || e.attack_cd <= 0.0 {
            let sign = if e.vx >= 0.0 { 1.0 } else { -1.0 };
            skull_summons.push((nx, ny, sign));
            summoner_despawns.push(id);
            continue;
        }

        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });
    }
    for id in summoner_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }
    // Conjure the skulls summoned this tick, springing up in the bolt's direction.
    for (x, y, sign) in skull_summons {
        let sid = shared.alloc_id();
        let mut skull = Entity::new(sid, EntityKind::Skull, x, y);
        skull.vx = sign * SKULL_SPEED;
        skull.vy = SKULL_BOUNCE_VELOCITY;
        skull.flee = SKULL_LIFETIME; // `flee` repurposed as the skull's life timer
        entities.insert(skull.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: skull });
    }

    // Friendly summoner fireballs in flight: a player's summoner spell loosed these.
    // Like the necromancer's bolt they travel in a straight line, but they strike
    // *monsters* (dealing a touch of damage) rather than players, and where they
    // burst — on a wall, on a monster, or when their life runs out — they summon a
    // *friendly* skull that fights for the caster.
    let mut friendly_fireball_despawns: Vec<EntityId> = Vec::new();
    // Friendly skulls conjured this tick, as `(x, y, vx_sign)`, spawned after the
    // loop so we aren't holding a mutable borrow of the bursting fireball.
    let mut friendly_skull_summons: Vec<(f32, f32, f32)> = Vec::new();
    for id in friendly_fireball_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);

        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;

        // Did it directly strike a monster this tick? Deal a little damage and knock.
        let kx = if e.vx >= 0.0 {
            KNOCKBACK_X
        } else {
            -KNOCKBACK_X
        };
        let mut struck = false;
        for &(tid, tx, ty, tw, th) in &hostiles {
            if aabb_gap(nx, ny, w, h, tx, ty, tw, th) <= SUMMONER_FIREBALL_HIT_RANGE {
                creature_hits.push((tid, (kx, -KNOCKBACK_Y), SUMMONER_FIREBALL_DAMAGE));
                struck = true;
                break;
            }
        }

        // Burst on a wall, on a monster, or when its life runs out: summon a friendly
        // skull where it died (popping up in its flight direction), then despawn.
        if hit_x || hit_y || struck || e.attack_cd <= 0.0 {
            let sign = if e.vx >= 0.0 { 1.0 } else { -1.0 };
            friendly_skull_summons.push((nx, ny, sign));
            friendly_fireball_despawns.push(id);
            continue;
        }

        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });
    }
    for id in friendly_fireball_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }
    // Conjure the friendly skulls summoned this tick, springing up in the bolt's
    // direction, set to hunt monsters until their short life runs out.
    for (x, y, sign) in friendly_skull_summons {
        let sid = shared.alloc_id();
        let mut skull = Entity::new(sid, EntityKind::FriendlySkull, x, y);
        skull.vx = sign * SKULL_SPEED;
        skull.vy = SKULL_BOUNCE_VELOCITY;
        skull.flee = SKULL_LIFETIME; // `flee` repurposed as the skull's life timer
        entities.insert(skull.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: skull });
    }

    // Dropped items: fall under gravity, then get collected by any player that
    // is touching them once their pickup delay has elapsed.
    'items: for id in item_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        let EntityKind::DroppedItem {
            block,
            count,
            durability,
        } = e.kind
        else {
            continue;
        };
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0); // reused as pickup delay

        let mut vx = e.vx;
        let mut vy = (e.vy + GRAVITY * TICK_DT).min(MAX_FALL);
        let (x, hit_wall) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
        if hit_wall {
            vx = 0.0;
        }
        let (y, on_ground) = move_y(&mut world, x, e.y, w, h, vy * TICK_DT);
        if on_ground {
            vy = 0.0;
            vx *= ITEM_GROUND_DRAG;
            if vx.abs() < 1.0 {
                vx = 0.0;
            }
        }
        e.x = x;
        e.y = y;
        e.vx = vx;
        e.vy = vy;

        // Collect into the first touching player with room (delay permitting).
        // A stack picks up as far as it fits; any remainder stays on the ground.
        if e.attack_cd <= 0.0 {
            for &(pid, px, py) in &players {
                if aabb_gap(x, y, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1) > ITEM_PICKUP_REACH {
                    continue;
                }
                let left = shared.add_stack(pid, block, count, durability);
                if left == count {
                    continue; // no room for this player; try the next
                }
                pickups.push(pid);
                if left == 0 {
                    entities.remove(id);
                    broadcasts.push(ServerMessage::EntityDespawn { id });
                } else {
                    if let Some(e) = entities.get_mut(id)
                        && let EntityKind::DroppedItem { count: c, .. } = &mut e.kind
                    {
                        *c = left;
                    }
                    broadcasts.push(ServerMessage::EntityMoved { id, x, y, vx, vy });
                }
                continue 'items;
            }
        }
        broadcasts.push(ServerMessage::EntityMoved { id, x, y, vx, vy });
    }

    // Fire damage: a player (or a pet — its one mortal hazard, since a server
    // creature never takes fall damage) whose body overlaps a burning cell is
    // singed on a steady interval. Detected while the world lock is still held;
    // applied below.
    let mut fire_hits: Vec<EntityId> = Vec::new();
    {
        let mut cds = shared.fire_cd.lock();
        for &(pid, px, py) in &players {
            let cd = cds.entry(pid).or_insert(0.0);
            *cd = (*cd - TICK_DT).max(0.0);
            if *cd <= 0.0 && body_in_fire(&mut world, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1) {
                *cd = FIRE_DAMAGE_INTERVAL;
                fire_hits.push(pid);
            }
        }
        for e in entities.values() {
            if !e.kind.is_pet() {
                continue;
            }
            let (w, h) = e.size();
            let cd = cds.entry(e.id).or_insert(0.0);
            *cd = (*cd - TICK_DT).max(0.0);
            if *cd <= 0.0 && body_in_fire(&mut world, e.x, e.y, w, h) {
                *cd = FIRE_DAMAGE_INTERVAL;
                fire_hits.push(e.id);
            }
        }
    }

    // Release the world lock before applying damage: a respawn at a campfire
    // relocks the world to verify it, which would deadlock if we still held it.
    drop(world);

    let mut respawns = Vec::new();
    // Suits that soaked a blow this tick, to wear down once the entities lock is
    // released (wear_tool relocks inventories and pushes a snapshot to the owner).
    let mut armor_wear: Vec<(EntityId, BlockId)> = Vec::new();
    for (pid, kb, damage) in bites {
        // Armor softens the blow: a worn suit turns aside a share of the damage,
        // but a hit always lands for at least 1 — armor never makes you invincible.
        // A suit that absorbs a blow takes a point of wear for it (applied below).
        //
        // An active dragon plate ward overrides all of this: while it holds, defense is
        // the ward's (far higher than any suit), it replaces rather than stacks with
        // armor, and — being magic, not metal — it wears no armor down. Once it lapses
        // the player falls back on whatever suit they carry.
        let warded = entities
            .get(pid)
            .is_some_and(|e| e.dragon_plate_timer > 0.0);
        let damage = if warded {
            let defense = crate::block::DRAGON_PLATE_DEFENSE;
            (damage - damage * defense / 100).max(1)
        } else {
            let armor = shared.worn_armor(pid);
            match armor {
                Some(a) => {
                    let defense = crate::block::armor_defense(a);
                    armor_wear.push((pid, a));
                    (damage - damage * defense / 100).max(1)
                }
                None => damage,
            }
        };
        let (msgs, respawn) = apply_damage(
            &mut entities,
            pid,
            damage,
            kb,
            dim,
            shared.respawn_target(pid),
        );
        broadcasts.extend(msgs);
        if let Some(r) = respawn {
            respawns.push(r);
        }
    }
    // Puppy bites on prey: apply the damage, and remember any creature that died
    // (and where) so we can spill its loot — raw meat for the puppy to eat — once
    // the entities lock is released (spawn_drop relocks it). The respawn arg is
    // unused for non-players, so a dummy is fine here.
    let mut puppy_kills: Vec<(EntityKind, f32, f32)> = Vec::new();
    for (tid, kb, damage) in creature_hits {
        let victim = entities.get(tid).map(|e| (e.kind.clone(), e.x, e.y));
        let (msgs, _) = apply_damage(&mut entities, tid, damage, kb, dim, (dim, 0.0, 0.0));
        broadcasts.extend(msgs);
        if let Some(v) = victim
            && entities.get(tid).is_none()
        {
            puppy_kills.push(v);
        }
    }
    // Damage knights soaked this tick. A mounted knight's horse takes the blow first
    // (its `mount_health` shield), and is thrown — the knight fighting on from foot —
    // once that's spent. An unmounted knight takes it on its own health, and a slain
    // one respawns wild at its owner's point (see `apply_damage`).
    for (kid, dmg) in knight_hits {
        let mounted = entities.get(kid).is_some_and(|e| e.mount_health > 0);
        if mounted {
            if let Some(e) = entities.get_mut(kid) {
                e.mount_health -= dmg;
                broadcasts.push(ServerMessage::EntityHit {
                    id: kid,
                    vx: 0.0,
                    vy: 0.0,
                });
                if e.mount_health <= 0 {
                    e.mount_health = 0;
                    e.riding = None;
                    broadcasts.push(ServerMessage::EntityRiding {
                        id: kid,
                        horse: None,
                    });
                }
            }
            continue;
        }
        // Unmounted: resolve the owner's respawn point (by name) for a fatal blow.
        let owner = entities
            .get(kid)
            .and_then(|e| e.kind.owner().map(str::to_owned));
        let target = match owner {
            Some(name) => named_players
                .iter()
                .find(|(n, ..)| *n == name)
                .map(|&(_, oid, _, _)| shared.respawn_target(oid))
                .unwrap_or((Dimension::Overworld, shared.spawn.0, shared.spawn.1)),
            None => shared.respawn_target(kid),
        };
        let (msgs, _) = apply_damage(&mut entities, kid, dmg, (0.0, 0.0), dim, target);
        broadcasts.extend(msgs);
    }
    for eid in fire_hits {
        // A cat returns to *its owner's* respawn point; a player to their own. The
        // owner is stored by name, so resolve it to a live player's id via the
        // tick's name snapshot (no entities re-lock); an absent/offline owner falls
        // back to world spawn.
        let owner = entities
            .get(eid)
            .and_then(|e| e.kind.owner().map(str::to_owned));
        let target = match owner {
            Some(name) => named_players
                .iter()
                .find(|(n, ..)| *n == name)
                .map(|&(_, oid, _, _)| shared.respawn_target(oid))
                .unwrap_or((Dimension::Overworld, shared.spawn.0, shared.spawn.1)),
            None => shared.respawn_target(eid),
        };
        let (msgs, respawn) =
            apply_damage(&mut entities, eid, FIRE_DAMAGE, (0.0, 0.0), dim, target);
        broadcasts.extend(msgs);
        if let Some(r) = respawn {
            respawns.push(r);
        }
    }
    drop(entities);

    // Wear down the armor of every player a blow landed on this tick: one point per
    // blow soaked. A suit worn to nothing shatters and frees its slot (handled by
    // wear_tool / damage_tool), and the owner gets a fresh inventory snapshot so the
    // durability bar — and a now-broken suit's empty slot — updates.
    for (pid, armor) in armor_wear {
        shared.wear_tool(pid, armor, ARMOR_WEAR_PER_HIT);
    }

    // Spill the loot of any creature felled this tick by a companion or a player's
    // friendly summon (a puppy's bite, a knight's or musketeer's blow, a bullet, a
    // friendly skull or dragon-fire). A chicken leaves raw meat; the **demon king**
    // leaves its boss chest and is marked slain, exactly as when a player lands the
    // killing blow by hand (otherwise a king finished off by an ally or a stray
    // bullet would drop nothing and the world would think it still reigns). Done
    // after the entities lock is released, since spawn_drop / spawn_boss_chest relock it.
    for (kind, kx, ky) in puppy_kills {
        if matches!(kind, EntityKind::DemonKing) {
            spawn_boss_chest(shared, dim, kx);
            shared.record_demon_king_slain();
            continue;
        }
        if matches!(kind, EntityKind::Twinscale) {
            shared.record_twinscale_slain();
            for (item, n) in twinscale_loot() {
                let cx = (kx / TILE_SIZE) as i32;
                let cy = (ky / TILE_SIZE) as i32;
                for _ in 0..n {
                    spawn_drop(shared, dim, cx, cy, item);
                }
            }
            continue;
        }
        let cx = (kx / TILE_SIZE) as i32;
        let cy = (ky / TILE_SIZE) as i32;
        for &(item, n) in creature_loot(&kind) {
            for _ in 0..n {
                spawn_drop(shared, dim, cx, cy, item);
            }
        }
    }

    // Register the tongues of fire laid this tick so they burn out shortly.
    if !new_trail_fires.is_empty() {
        let mut tf = shared.trail_fires.lock();
        for (x, y) in new_trail_fires {
            tf.insert((dim, x, y), TRAIL_FIRE_LIFETIME);
        }
    }

    Step {
        broadcasts,
        respawns,
        pickups,
    }
}

/// Whether the AABB at `(x, y)` of size `(w, h)` overlaps any burning cell, so an
/// entity standing in flame takes damage. Mirrors `body_in_water` for fire.
fn body_in_fire(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32) -> bool {
    let x0 = (x / TILE_SIZE).floor() as i32;
    let x1 = ((x + w - EPS) / TILE_SIZE).floor() as i32;
    let y0 = (y / TILE_SIZE).floor() as i32;
    let y1 = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    (y0..=y1).any(|ty| (x0..=x1).any(|tx| crate::block::is_fire(world.get(tx, ty))))
}

/// Nearest player (returned as its top-left `(id, x, y)`) whose center is within
/// `range` of `(x, y)`, or `None` if none are close enough.
fn nearest_player(
    players: &[(EntityId, f32, f32)],
    x: f32,
    y: f32,
    range: f32,
) -> Option<(EntityId, f32, f32)> {
    let mut best: Option<(EntityId, f32, f32, f32)> = None;
    for &(pid, px, py) in players {
        let dx = (px + PLAYER_SIZE.0 * 0.5) - x;
        let dy = (py + PLAYER_SIZE.1 * 0.5) - y;
        let d2 = dx * dx + dy * dy;
        if d2 <= range * range && best.is_none_or(|(_, _, _, bd)| d2 < bd) {
            best = Some((pid, px, py, d2));
        }
    }
    best.map(|(pid, px, py, _)| (pid, px, py))
}

/// Nearest entity from `list` (each `(id, x, y, w, h)`) whose center lies within
/// `range` of `(x, y)`, or `None` if none are close enough. Used by the puppy to
/// pick the nearest prey to hunt or scrap of meat to eat.
fn nearest_of(
    list: &[(EntityId, f32, f32, f32, f32)],
    x: f32,
    y: f32,
    range: f32,
) -> Option<(EntityId, f32, f32, f32, f32)> {
    let mut best: Option<(EntityId, f32, f32, f32, f32, f32)> = None;
    for &(id, ex, ey, ew, eh) in list {
        let dx = (ex + ew * 0.5) - x;
        let dy = (ey + eh * 0.5) - y;
        let d2 = dx * dx + dy * dy;
        if d2 <= range * range && best.is_none_or(|(.., bd)| d2 < bd) {
            best = Some((id, ex, ey, ew, eh, d2));
        }
    }
    best.map(|(id, ex, ey, ew, eh, _)| (id, ex, ey, ew, eh))
}

/// A target a hostile creature hunts: a player, or a knight (wild or recruited —
/// monsters are enemies of every man-at-arms, recruited or not). Carries the body
/// box for reach checks and a `knight` flag so a blow is routed to the right bucket
/// — `bites` for a player, `knight_hits` for a knight (where a mounted knight's
/// horse soaks it and a slain one respawns at its owner's point).
#[derive(Clone, Copy)]
struct Prey {
    id: EntityId,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    knight: bool,
}

/// Nearest prey — player or knight — whose center lies within `range` of `(x, y)`.
/// Hostile creatures hunt both, so a wandering knight draws monsters the same way a
/// player does. `knights` is each knight as `(id, x, y, w, h)`.
fn nearest_prey(
    players: &[(EntityId, f32, f32)],
    knights: &[(EntityId, f32, f32, f32, f32)],
    x: f32,
    y: f32,
    range: f32,
) -> Option<Prey> {
    let mut best: Option<(Prey, f32)> = None;
    for &(id, px, py) in players {
        let dx = (px + PLAYER_SIZE.0 * 0.5) - x;
        let dy = (py + PLAYER_SIZE.1 * 0.5) - y;
        let d2 = dx * dx + dy * dy;
        if d2 <= range * range && best.is_none_or(|(_, bd)| d2 < bd) {
            best = Some((
                Prey {
                    id,
                    x: px,
                    y: py,
                    w: PLAYER_SIZE.0,
                    h: PLAYER_SIZE.1,
                    knight: false,
                },
                d2,
            ));
        }
    }
    for &(id, kx, ky, kw, kh) in knights {
        let dx = (kx + kw * 0.5) - x;
        let dy = (ky + kh * 0.5) - y;
        let d2 = dx * dx + dy * dy;
        if d2 <= range * range && best.is_none_or(|(_, bd)| d2 < bd) {
            best = Some((
                Prey {
                    id,
                    x: kx,
                    y: ky,
                    w: kw,
                    h: kh,
                    knight: true,
                },
                d2,
            ));
        }
    }
    best.map(|(p, _)| p)
}

/// Route a hostile creature's blow at `p` to the right bucket: a knight takes it on
/// `knight_hits` (its mount shield / owner-respawn handling lives there, so it gets
/// no knockback), a player on `bites` with the given knockback.
fn hit_prey(
    bites: &mut Vec<(EntityId, (f32, f32), i32)>,
    knight_hits: &mut Vec<(EntityId, i32)>,
    p: &Prey,
    knockback: (f32, f32),
    damage: i32,
) {
    if p.knight {
        knight_hits.push((p.id, damage));
    } else {
        bites.push((p.id, knockback, damage));
    }
}

/// Whether `kind` is a hostile monster — the set a knight or musketeer will fight (and
/// that a player can "mark" for its warrior by striking one, and that the player's own
/// friendly summons — bullets, fireballs, skulls, dragon-fire — strike). Passive
/// animals, pets, companions, projectiles and items are not hostile. The
/// [`EntityKind::DemonKing`] counts: companions charge it and friendly fire reaches it
/// like any other foe (its guardian shield still gates the actual damage in
/// [`apply_damage`]).
fn is_hostile(kind: &EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Slime
            | EntityKind::Zombie
            | EntityKind::Spider
            | EntityKind::Snake
            | EntityKind::Skeleton
            | EntityKind::CharredSkeleton
            | EntityKind::Demon
            | EntityKind::Orc
            | EntityKind::AshTwister
            | EntityKind::OrcMage
            | EntityKind::EnchantedDemon
            | EntityKind::Necromancer
            | EntityKind::Skull
            | EntityKind::DarkKnight
            | EntityKind::DarkMusketeer
            | EntityKind::Dragon
            | EntityKind::Minotaur
            | EntityKind::Gargoyle
            | EntityKind::DemonKing
            | EntityKind::Twinscale
    )
}

/// Whether `kind` is a creature that **burns up in daylight** — the night undead
/// that crumble or wink out at daybreak in the overworld (and roam the always-dark
/// underworld around the clock). This is exactly the set the [sunburst
/// spell](crate::block::SUNBURST_SPELL) annihilates. The player's own friendly
/// summons are *not* included — they are the caster's magic, not creatures of the
/// night — so a sunburst never wipes your own skulls.
fn burns_in_daylight(kind: &EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Zombie
            | EntityKind::Skeleton
            | EntityKind::DarkKnight
            | EntityKind::DarkMusketeer
            | EntityKind::Necromancer
            | EntityKind::Skull
    )
}

/// What a creature `kind` is **restored** into by the [restore
/// spell](crate::block::RESTORE_SPELL), or `None` if it cannot be restored. An orc
/// or a dark knight is turned to a [`EntityKind::Knight`]; a dark musketeer to a
/// [`EntityKind::Musketeer`]; an orc mage to a [`EntityKind::Mage`]; an enchanted
/// demon is calmed back into an ordinary [`EntityKind::Demon`]. `owner` is stamped
/// onto the resulting knight/musketeer/mage — the caster's name (recruiting it) or
/// `None` (leaving it wild); it is ignored for the ownerless demon.
fn restored_kind(kind: &EntityKind, owner: &Option<String>) -> Option<EntityKind> {
    match kind {
        EntityKind::Orc | EntityKind::DarkKnight => Some(EntityKind::Knight {
            owner: owner.clone(),
        }),
        EntityKind::DarkMusketeer => Some(EntityKind::Musketeer {
            owner: owner.clone(),
        }),
        EntityKind::OrcMage => Some(EntityKind::Mage {
            owner: owner.clone(),
        }),
        EntityKind::EnchantedDemon => Some(EntityKind::Demon),
        _ => None,
    }
}

/// Whether `kind` can be turned by the restore spell (see [`restored_kind`]).
fn is_restorable(kind: &EntityKind) -> bool {
    restored_kind(kind, &None).is_some()
}

/// Transform an existing entity in place into `new_kind`: swap its kind, heal it to
/// the new kind's full health, and clear the transient combat/mount/animation state
/// the old kind may have set (a flying enchanted demon becomes a ground-walking
/// demon; a mounted dark knight becomes an unmounted knight). Returns the messages to
/// broadcast so every client adopts the new form (a full [`ServerMessage::EntitySpawn`]
/// resend, plus its fresh health). No-op (empty) if `id` is gone.
fn transform_entity(
    entities: &mut Entities,
    id: EntityId,
    new_kind: EntityKind,
) -> Vec<ServerMessage> {
    let Some(e) = entities.get_mut(id) else {
        return Vec::new();
    };
    let max = new_kind.max_health();
    e.kind = new_kind;
    e.health = max;
    e.max_health = max;
    e.vx = 0.0;
    e.vy = 0.0;
    e.attack_cd = 0.0;
    e.lunge = 0.0;
    e.lunge_dir = 0.0;
    e.dying = 0.0;
    e.flee = 0.0;
    e.home_x = None;
    e.riding = None;
    e.mount_health = 0;
    e.controlling = None;
    vec![
        ServerMessage::EntitySpawn { entity: e.clone() },
        ServerMessage::EntityHealth {
            id,
            health: e.health,
            max_health: e.max_health,
        },
    ]
}

/// Smallest gap (px) between two AABBs; `0.0` when they overlap.
fn aabb_gap(ax: f32, ay: f32, aw: f32, ah: f32, bx: f32, by: f32, bw: f32, bh: f32) -> f32 {
    let gx = (bx - (ax + aw)).max(ax - (bx + bw)).max(0.0);
    let gy = (by - (ay + ah)).max(ay - (by + bh)).max(0.0);
    gx.max(gy)
}

/// Where a freshly killed player must be sent to respawn: the destination
/// dimension and world-pixel position. Resolved after the entity locks are
/// released (see [`Shared::finish_respawn`]) since a respawn may cross dimensions.
struct RespawnInfo {
    id: EntityId,
    dim: Dimension,
    x: f32,
    y: f32,
}

/// Apply `amount` damage to entity `id` (caller already holds the entities lock
/// for `current_dim`). Returns the messages to broadcast and, if a *player* died,
/// the [`RespawnInfo`] to enact once locks are released. A non-player that dies is
/// removed from the world. A player who dies is healed; if their respawn is in
/// `current_dim` it is repositioned in place here, otherwise it is left untouched
/// for [`Shared::enter_dimension`] to move it across.
fn apply_damage(
    entities: &mut Entities,
    id: EntityId,
    amount: i32,
    knockback: (f32, f32),
    current_dim: Dimension,
    respawn: (Dimension, f32, f32),
) -> (Vec<ServerMessage>, Option<RespawnInfo>) {
    // The arena boss is invulnerable while its orc-mage guardians still live: a blow
    // lands and flashes it red so the strike reads, but it takes no damage until the
    // last mage falls. Checked before the mutable borrow below so we can scan the map.
    let shielded = entities
        .get(id)
        .is_some_and(|e| matches!(e.kind, EntityKind::DemonKing))
        && entities
            .values()
            .any(|e| matches!(e.kind, EntityKind::OrcMage));
    if shielded {
        return (
            vec![ServerMessage::EntityHit {
                id,
                vx: knockback.0,
                vy: knockback.1,
            }],
            None,
        );
    }

    let Some(e) = entities.get_mut(id) else {
        return (Vec::new(), None);
    };
    // An entity already playing its death animation is committed to despawning;
    // further damage (e.g. lingering fire on the corpse) is moot and must never
    // retrigger or restart the animation.
    if e.dying > 0.0 {
        return (Vec::new(), None);
    }
    e.health = (e.health - amount).max(0);

    // Knockback: shove server-simulated creatures directly (their motion is
    // authoritative here, so the next tick carries them off). Player avatars are
    // client-authoritative, so we can't move them from here — the owning client
    // applies the same velocity when it sees the EntityHit below.
    if !e.kind.is_player() {
        e.vx += knockback.0;
        e.vy += knockback.1;
    }

    // Getting hit sends a chicken into a panicked sprint away from players.
    if matches!(e.kind, EntityKind::Chicken) {
        e.flee = CHICKEN_FLEE_TIME;
    }

    // Every hit flashes the target red on all clients and carries the knockback.
    let mut msgs = vec![ServerMessage::EntityHit {
        id,
        vx: knockback.0,
        vy: knockback.1,
    }];

    if e.health > 0 {
        msgs.push(ServerMessage::EntityHealth {
            id,
            health: e.health,
            max_health: e.max_health,
        });
        return (msgs, None);
    }

    if e.kind.is_player() {
        // Death = respawn at full health at the player's respawn point (their last
        // campfire, or world spawn if they haven't used one).
        let (rdim, rx, ry) = respawn;
        e.health = e.max_health;
        // A same-dimension respawn repositions the avatar here; a cross-dimension
        // one is left for enter_dimension to relocate after locks are released.
        if rdim == current_dim {
            e.x = rx;
            e.y = ry;
        }
        let health = e.health;
        let max_health = e.max_health;
        msgs.push(ServerMessage::EntityHealth {
            id,
            health,
            max_health,
        });
        (
            msgs,
            Some(RespawnInfo {
                id,
                dim: rdim,
                x: rx,
                y: ry,
            }),
        )
    } else if matches!(e.kind, EntityKind::Knight { owner: Some(_) }) {
        // A slain knight reappears at its owner's respawn point — but, unlike a pet,
        // the bond does not survive death: it comes back **wild** (owner cleared) and
        // must be recruited afresh with another tungsten ingot.
        let (_, rx, ry) = respawn;
        e.health = e.max_health;
        e.x = rx;
        e.y = ry;
        e.vx = 0.0;
        e.vy = 0.0;
        e.home_x = Some(rx);
        e.riding = None;
        e.mount_health = 0;
        if let EntityKind::Knight { owner } = &mut e.kind {
            *owner = None;
        }
        // Resync the whole entity (the cleared owner rides along in `kind`).
        msgs.push(ServerMessage::EntitySpawn { entity: e.clone() });
        (msgs, None)
    } else if matches!(e.kind, EntityKind::Musketeer { owner: Some(_) }) {
        // A slain musketeer, like a knight, reappears **wild** at its owner's respawn
        // point — the bond does not survive death; it must be recruited afresh.
        let (_, rx, ry) = respawn;
        e.health = e.max_health;
        e.x = rx;
        e.y = ry;
        e.vx = 0.0;
        e.vy = 0.0;
        e.home_x = Some(rx);
        if let EntityKind::Musketeer { owner } = &mut e.kind {
            *owner = None;
        }
        msgs.push(ServerMessage::EntitySpawn { entity: e.clone() });
        (msgs, None)
    } else if e.kind.owner().is_some() {
        // A tamed pet doesn't die for good: it reappears at its owner's respawn
        // point, healed and re-anchoring its wander there, rather than being
        // removed from the world. (Repositioned within the current dimension —
        // owner respawn points are effectively always the overworld surface a pet
        // roams, so a cross-dimension respawn is not worth shuttling it for.)
        let (_, rx, ry) = respawn;
        e.health = e.max_health;
        e.x = rx;
        e.y = ry;
        e.vx = 0.0;
        e.vy = 0.0;
        e.home_x = Some(rx);
        // A respawned pet reappears sitting at its owner's campfire — waiting there
        // patiently rather than wandering off — regardless of how it was before.
        if let EntityKind::Cat { sitting, .. } | EntityKind::Puppy { sitting, .. } = &mut e.kind {
            *sitting = true;
        }
        // Resync the whole entity (a plain EntityMoved/EntityHealth pair wouldn't
        // carry the flipped `sitting` flag, which rides along in `kind`).
        msgs.push(ServerMessage::EntitySpawn { entity: e.clone() });
        (msgs, None)
    } else if let Some(t) = e.kind.death_time() {
        // A killed snake writhes through its death animation before despawning,
        // rather than vanishing on the spot. The AI loop runs the timer down and
        // removes it once the animation finishes. (A zombie's death_time is its
        // daylight crumble — but a zombie only ever reaches 0 health here, where
        // it likewise gets the courtesy of playing out rather than blinking away.)
        e.dying = t;
        e.vx = 0.0;
        msgs.push(ServerMessage::EntityDying { id });
        (msgs, None)
    } else {
        entities.remove(id);
        msgs.push(ServerMessage::EntityDespawn { id });
        (msgs, None)
    }
}

/// Move an AABB horizontally by `dx`, stopping at the first solid column.
/// Returns the resolved x and whether a wall was hit.
fn move_x(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, dx: f32) -> (f32, bool) {
    if dx == 0.0 {
        return (x, false);
    }
    let y0 = (y / TILE_SIZE).floor() as i32;
    let y1 = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    // Substep so a fast mover can't tunnel through a tile: each pass advances at
    // most one tile and checks the column it lands in.
    let steps = (dx.abs() / TILE_SIZE).ceil().max(1.0) as i32;
    let step = dx / steps as f32;
    let mut cx = x;
    for _ in 0..steps {
        let new_x = cx + step;
        if step > 0.0 {
            let tx = ((new_x + w - EPS) / TILE_SIZE).floor() as i32;
            if (y0..=y1).any(|ty| world.solid(tx, ty)) {
                return (tx as f32 * TILE_SIZE - w, true);
            }
        } else {
            let tx = (new_x / TILE_SIZE).floor() as i32;
            if (y0..=y1).any(|ty| world.solid(tx, ty)) {
                return ((tx + 1) as f32 * TILE_SIZE, true);
            }
        }
        cx = new_x;
    }
    (cx, false)
}

/// Move an AABB vertically by `dy`, stopping at the first solid row. Returns the
/// resolved y and whether the entity is now resting on the ground.
fn move_y(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, dy: f32) -> (f32, bool) {
    if dy == 0.0 {
        return (y, false);
    }
    let x0 = (x / TILE_SIZE).floor() as i32;
    let x1 = ((x + w - EPS) / TILE_SIZE).floor() as i32;
    // Substep so a fast fall can't tunnel through a tile: each pass advances at
    // most one tile and checks the row it lands in.
    let steps = (dy.abs() / TILE_SIZE).ceil().max(1.0) as i32;
    let step = dy / steps as f32;
    let mut cy = y;
    for _ in 0..steps {
        let new_y = cy + step;
        if step > 0.0 {
            let ty = ((new_y + h - EPS) / TILE_SIZE).floor() as i32;
            if (x0..=x1).any(|tx| world.solid(tx, ty)) {
                return (ty as f32 * TILE_SIZE - h, true);
            }
        } else {
            let ty = (new_y / TILE_SIZE).floor() as i32;
            if (x0..=x1).any(|tx| world.solid(tx, ty)) {
                return ((ty + 1) as f32 * TILE_SIZE, true);
            }
        }
        cy = new_y;
    }
    (cy, false)
}

/// Resolved motion of a ground-walking creature after one [`step_ground`] tick.
struct GroundMotion {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

/// Advance a ground-walking creature (slime, chicken) one tick.
///
/// It moves horizontally in direction `dir` (sign of intent) at `speed`, falls
/// under gravity, and negotiates one-block height changes so it roams across
/// uneven ground instead of being trapped on a single level: it hops a
/// single-block step in its path and walks down a single-block drop. A
/// `committed` creature (chasing or fleeing) bulls through — never reversing and
/// following the player off cliffs — while an uncommitted (wandering) one turns
/// around at walls it can't hop and at drops deeper than one block. The returned
/// `vx` carries the post-turn heading so patrol direction persists across ticks.
fn step_ground(
    world: &mut ServerWorld,
    aabb: (f32, f32, f32, f32),
    vy_in: f32,
    dir: f32,
    speed: f32,
    committed: bool,
) -> GroundMotion {
    let (x, y, w, h) = aabb;
    let grounded_before = grounded(world, x, y, w, h);
    // A creature already standing in water has its water-avoidance suspended, so
    // one that fell in can wade back out rather than turning back at every edge.
    let in_water = body_in_water(world, x, y, w, h);
    let mut vx = dir * speed;
    let mut vy = (vy_in + GRAVITY * TICK_DT).min(MAX_FALL);

    // Horizontal first, then vertical — each axis resolved independently.
    let (nx, hit_wall) = move_x(world, x, y, w, h, vx * TICK_DT);

    // Hop a single-block step we ran into, if there's headroom to clear it.
    if grounded_before && hit_wall && can_step_up(world, nx, y, w, h, vx) {
        vy = HOP_VELOCITY;
    }

    let (ny, on_ground) = move_y(world, nx, y, w, h, vy * TICK_DT);
    if on_ground {
        vy = 0.0;
    }

    // Wandering creatures reverse at walls they couldn't hop (vy not launched
    // upward), at drops too deep to step down, and at the water's edge (which they
    // prefer not to wade into). Committed ones never turn.
    if !committed
        && on_ground
        && ((hit_wall && vy >= 0.0)
            || drop_ahead(world, nx, ny, w, h, vx) >= 2
            || (!in_water && water_ahead(world, nx, ny, w, h, vx)))
    {
        vx = -vx;
    }

    GroundMotion {
        x: nx,
        y: ny,
        vx,
        vy,
    }
}

/// Advance a wall-climbing creature (spider) one tick.
///
/// Like [`step_ground`] it walks in direction `dir` at `speed` and falls under
/// gravity, but when it runs into a wall it can *climb*: a `committed` (chasing)
/// spider that hits a wall ascends it at [`SPIDER_CLIMB_SPEED`] instead of being
/// stopped, so it scales sheer terrain to reach a player above. An uncommitted
/// (wandering) one behaves like a plain ground creature — hopping single-block
/// steps and turning back at taller walls and deep drops — so it patrols its
/// patch instead of climbing out of sight.
fn step_climber(
    world: &mut ServerWorld,
    aabb: (f32, f32, f32, f32),
    vy_in: f32,
    dir: f32,
    speed: f32,
    committed: bool,
) -> GroundMotion {
    let (x, y, w, h) = aabb;
    let grounded_before = grounded(world, x, y, w, h);
    let in_water = body_in_water(world, x, y, w, h);
    let mut vx = dir * speed;
    let mut vy = (vy_in + GRAVITY * TICK_DT).min(MAX_FALL);

    let (nx, hit_wall) = move_x(world, x, y, w, h, vx * TICK_DT);

    if hit_wall {
        if committed {
            // Chasing into a wall: climb straight up its face to follow the player.
            vy = -SPIDER_CLIMB_SPEED;
        } else if grounded_before && can_step_up(world, nx, y, w, h, vx) {
            // Wandering: clear a single-block step like a ground creature.
            vy = HOP_VELOCITY;
        }
    }

    let (ny, on_ground) = move_y(world, nx, y, w, h, vy * TICK_DT);
    if on_ground {
        vy = 0.0;
    }

    // A wandering spider reverses at a wall it didn't hop, at deep drops, and at
    // the water's edge; a chasing one never turns (it climbs walls or follows the
    // player off ledges and into water).
    if !committed
        && on_ground
        && ((hit_wall && vy >= 0.0)
            || drop_ahead(world, nx, ny, w, h, vx) >= 2
            || (!in_water && water_ahead(world, nx, ny, w, h, vx)))
    {
        vx = -vx;
    }

    GroundMotion {
        x: nx,
        y: ny,
        vx,
        vy,
    }
}

/// Heading (`-1.0`/`1.0`) for a wandering creature whose center is at `center_x`:
/// keep going the way it was, but turn back toward `home_x` once it strays past
/// [`WANDER_RANGE`], so it loiters in one area instead of marching off.
fn wander_dir(center_x: f32, vx: f32, home_x: f32) -> f32 {
    if center_x > home_x + WANDER_RANGE {
        -1.0
    } else if center_x < home_x - WANDER_RANGE {
        1.0
    } else if vx < 0.0 {
        -1.0
    } else {
        1.0
    }
}

/// Whether an unclimbable wall blocks a creature trying to move in direction
/// `dir`: a solid cell ahead at body height that a single hop can't clear. Lets
/// a fleeing creature veer around walls instead of running headlong into them.
fn blocked_ahead(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, dir: f32) -> bool {
    let ahead = if dir > 0.0 { x + w + EPS } else { x - EPS };
    let tx = (ahead / TILE_SIZE).floor() as i32;
    let y0 = (y / TILE_SIZE).floor() as i32;
    let y1 = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    let wall = (y0..=y1).any(|ty| world.solid(tx, ty));
    wall && !can_step_up(world, x, y, w, h, dir)
}

/// Whether any cell overlapping an entity's AABB is water. Used to suspend a
/// creature's water-avoidance once it is already submerged, so it can wade back
/// out instead of jittering against the bank it can no longer escape past.
fn body_in_water(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32) -> bool {
    let x0 = (x / TILE_SIZE).floor() as i32;
    let x1 = ((x + w - EPS) / TILE_SIZE).floor() as i32;
    let y0 = (y / TILE_SIZE).floor() as i32;
    let y1 = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    (y0..=y1).any(|ty| (x0..=x1).any(|tx| crate::block::is_water(world.get(tx, ty))))
}

/// Whether water lies just ahead of a creature heading `dir`: pooled in the cell
/// it would step into at body height, or in the cell directly below that step
/// (so it won't wade off a bank into deeper water). Land creatures shy away from
/// water, turning back at its edge much as they do at a wall or a cliff.
fn water_ahead(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, dir: f32) -> bool {
    let ahead = if dir > 0.0 { x + w + EPS } else { x - EPS };
    let tx = (ahead / TILE_SIZE).floor() as i32;
    let y0 = (y / TILE_SIZE).floor() as i32;
    let foot = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    (y0..=foot).any(|ty| crate::block::is_water(world.get(tx, ty)))
        || crate::block::is_water(world.get(tx, foot + 1))
}

/// Whether an entity's AABB is resting on solid ground (a solid cell directly
/// beneath its feet).
fn grounded(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32) -> bool {
    let ty = ((y + h + EPS) / TILE_SIZE).floor() as i32;
    let x0 = (x / TILE_SIZE).floor() as i32;
    let x1 = ((x + w - EPS) / TILE_SIZE).floor() as i32;
    (x0..=x1).any(|tx| world.solid(tx, ty))
}

/// Whether a grounded creature heading `vx` can clear the block directly in its
/// path with a single hop: the cell ahead at foot level is solid, but the two
/// cells above it are clear, leaving room to rise and land one block up.
fn can_step_up(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, vx: f32) -> bool {
    let ahead = if vx > 0.0 { x + w + EPS } else { x - EPS };
    let tx = (ahead / TILE_SIZE).floor() as i32;
    let foot = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    world.solid(tx, foot) && !world.solid(tx, foot - 1) && !world.solid(tx, foot - 2)
}

/// How far the ground drops just ahead of a grounded creature, in tiles: `0` is
/// level ground (or a wall) ahead, `1` is a single-block step down, and `2`
/// means a drop of two or more blocks (a cliff). Capped at `3` so a bottomless
/// gap doesn't scan forever.
fn drop_ahead(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, vx: f32) -> i32 {
    let ahead = if vx > 0.0 { x + w + EPS } else { x - EPS };
    let tx = (ahead / TILE_SIZE).floor() as i32;
    let ground = ((y + h + EPS) / TILE_SIZE).floor() as i32;
    let mut d = 0;
    while d < 3 && !world.solid(tx, ground + d) {
        d += 1;
    }
    d
}

async fn handle_connection(incoming: quinn::Incoming, shared: Arc<Shared>) -> Result<()> {
    let connection = incoming.await.context("accepting connection")?;
    let peer_ip = connection.remote_address().ip();
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .context("accepting bidirectional stream")?;

    // Banned IPs are turned away before they can join. Done here (rather than at
    // the lower QUIC layer) so the client surfaces the human-readable reason via
    // the connection's close_reason, the same path the version check below uses.
    if shared.banned_ips.lock().contains(&peer_ip) {
        let reason = "You are banned from this server.";
        log::info!("rejecting banned peer {peer_ip}");
        connection.close(BANNED_CLOSE.into(), reason.as_bytes());
        return Ok(());
    }

    // Before anything else, check the peer speaks our wire version. Rejecting a
    // skewed client here — with a clear reason — prevents the cryptic bincode
    // "invalid variant index" errors that mismatched ClientMessage/ServerMessage
    // layouts would otherwise produce mid-session.
    let peer_version = read_version(&mut recv)
        .await
        .context("reading client protocol version")?;
    if peer_version != PROTOCOL_VERSION {
        let reason = format!(
            "protocol version mismatch: server is v{PROTOCOL_VERSION}, client is v{peer_version} — update both to the same build"
        );
        log::warn!("rejecting connection: {reason}");
        connection.close(VERSION_MISMATCH_CLOSE.into(), reason.as_bytes());
        return Ok(());
    }

    // The first message must be `Hello`; authenticate it before admitting the
    // player. Anything else (or a dropped stream) ends the connection here,
    // before any entity, client handle, or other state is registered — so a
    // rejected login leaves nothing to clean up.
    let (name, password, creator_token) = match read_msg::<ClientMessage>(&mut recv).await {
        Ok(ClientMessage::Hello {
            name,
            password,
            creator_token,
        }) => (name.trim().to_string(), password, creator_token),
        Ok(_) => {
            connection.close(AUTH_FAILED_CLOSE.into(), b"expected a login first");
            return Ok(());
        }
        Err(e) => {
            log::debug!("connection from {peer_ip} closed before login: {e:#}");
            return Ok(());
        }
    };

    // Validate the name, reject a duplicate of an already-connected player, and
    // either register a new account or verify the password of an existing one. On
    // any rejection, close with a human-readable reason the client surfaces (the
    // same path the version and ban checks use).
    if let Err(reason) = shared.authenticate(&name, &password) {
        log::info!("rejecting login from {peer_ip} as '{name}': {reason}");
        connection.close(AUTH_FAILED_CLOSE.into(), reason.as_bytes());
        return Ok(());
    }

    // Authorized. This connection is the server admin only if it presented the
    // correct per-server token (the host's own client holds it). Admin powers
    // (admin commands) are reserved for it. Creator mode is broader: it is allowed
    // for the admin always, and for everyone on a creator-type server.
    let is_admin = creator_token == Some(shared.creator_token);
    let creator_allowed = shared.creator_world || is_admin;
    if is_admin {
        log::info!("login '{name}' authorized as admin");
    }

    let id = shared.alloc_id();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    // Writer task: drains outbound messages to the send stream.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_msg(&mut send, &msg).await.is_err() {
                break;
            }
        }
        let _ = send.finish();
    });

    // Welcome the now-authenticated player.
    let (sx, sy) = shared.spawn;
    let _ = tx.send(ServerMessage::Welcome {
        entity_id: id,
        spawn_x: sx,
        spawn_y: sy,
        creator_allowed,
        voice: shared.voice.lock().clone(),
        webcam: shared.webcam.lock().clone(),
        map: shared.map.lock().clone(),
    });
    // Tell the owner its starting health (its own avatar is never mirrored via
    // EntitySpawn) and the current time of day.
    let _ = tx.send(ServerMessage::EntityHealth {
        id,
        health: crate::entity::PLAYER_MAX_HEALTH,
        max_health: crate::entity::PLAYER_MAX_HEALTH,
    });
    let _ = tx.send(ServerMessage::TimeOfDay {
        t: shared.time_of_day(),
    });

    // Send the newcomer a snapshot of every existing entity, then register and
    // announce their own (already-named) player entity to everyone else. Fresh
    // connections begin in the overworld; a returning player may be moved to the
    // underworld by the state restore below if that's where they left off.
    let player = Entity::new(id, EntityKind::Player { name: name.clone() }, sx, sy);
    shared.client_dim.lock().insert(id, Dimension::Overworld);
    {
        let mut entities = shared.entities(Dimension::Overworld).lock();
        for e in entities.values() {
            let _ = tx.send(ServerMessage::EntitySpawn { entity: e.clone() });
            // The serialized entity doesn't carry the (runtime-only) boat/horse pose,
            // so follow up for anyone already afloat or mounted to draw their boat or
            // ride them on their horse from the start.
            if e.boating {
                let _ = tx.send(ServerMessage::EntityBoating { id: e.id, on: true });
            }
            if let Some(horse) = e.riding {
                let _ = tx.send(ServerMessage::EntityRiding {
                    id: e.id,
                    horse: Some(horse),
                });
            }
        }
        entities.insert(player.clone());
    }
    shared.clients.lock().insert(
        id,
        ClientHandle {
            tx: tx.clone(),
            addr: peer_ip,
            conn: connection.clone(),
        },
    );
    shared.broadcast_dim_except(
        Dimension::Overworld,
        id,
        ServerMessage::EntitySpawn { entity: player },
    );
    // Start with an empty inventory and no mana; a returning player gets theirs
    // restored below. Either way the owner is told its contents.
    shared.inventories.lock().insert(id, Inventory::new());
    shared.send_inventory(id);
    shared.mana.lock().insert(id, 0);
    shared.send_mana(id);
    log::info!("player {id} ('{name}') connected");

    // Restore any saved state for this name: position, health, inventory, last
    // campfire respawn point, waypoints, and the dimension they logged out in. A
    // brand-new account simply has none of this.
    {
        let restored = shared.saved_players.lock().remove(&name);
        if let Some(sp) = &restored
            && let Some(e) = shared.entities(Dimension::Overworld).lock().get_mut(id)
        {
            e.x = sp.x;
            e.y = sp.y;
            e.health = sp.health;
        }
        if let Some(sp) = restored {
            if let Some(rp) = sp.respawn {
                shared.respawn_points.lock().insert(id, rp);
            }
            shared.inventories.lock().insert(id, sp.inventory.clone());
            shared.send_inventory(id);
            shared.mana.lock().insert(id, sp.mana.clamp(0, MANA_MAX));
            shared.send_mana(id);
            shared.waypoints.lock().insert(id, sp.waypoints.clone());
            if sp.dim == Dimension::Overworld {
                // Teleport the owner's avatar and resync its health. This is a
                // reconnect, not a death, so no death marker drops.
                let _ = tx.send(ServerMessage::Respawn {
                    x: sp.x,
                    y: sp.y,
                    died: false,
                });
                shared.broadcast_dim_except(
                    Dimension::Overworld,
                    id,
                    ServerMessage::EntityMoved {
                        id,
                        x: sp.x,
                        y: sp.y,
                        vx: 0.0,
                        vy: 0.0,
                    },
                );
            } else {
                // They logged out in the underworld: move the avatar there (this
                // clears and re-streams the client's world).
                shared.enter_dimension(id, sp.dim, sp.x, sp.y);
            }
            shared.broadcast_all(ServerMessage::EntityHealth {
                id,
                health: sp.health,
                max_health: crate::entity::PLAYER_MAX_HEALTH,
            });
        }
        // Sync waypoints + home now that any saved state is restored (a fresh
        // player just gets an empty list and the world spawn).
        shared.send_waypoints(id);
    }

    // Reader loop.
    let read_result: Result<()> = async {
        loop {
            let msg: ClientMessage = read_msg(&mut recv).await?;
            match msg {
                // The login was consumed and authenticated above; a second Hello
                // mid-session is unexpected and ignored.
                ClientMessage::Hello { .. } => {
                    log::debug!("ignoring repeat Hello from player {id}");
                }
                ClientMessage::RequestChunk { dim, cx, cy } => {
                    // Only serve chunks for the dimension the player is actually in;
                    // a stale request from just before a transition is ignored.
                    let cur = shared.dim_of(id);
                    if dim != cur {
                        continue;
                    }
                    let (blocks, fresh) = shared.world(dim).lock().chunk_blocks(cx, cy);
                    let _ = tx.send(ServerMessage::Chunk {
                        dim,
                        cx,
                        cy,
                        blocks,
                    });
                    // Hand over any sign / quest-board text written within this chunk
                    // so the newly streamed boards show their writing right away.
                    let x_range = cx * CHUNK_SIZE..(cx + 1) * CHUNK_SIZE;
                    let y_range = cy * CHUNK_SIZE..(cy + 1) * CHUNK_SIZE;
                    let texts: Vec<(i32, i32, crate::protocol::BlockText)> = shared
                        .block_text
                        .lock()
                        .iter()
                        .filter(|((d, x, y), _)| {
                            *d == dim && x_range.contains(x) && y_range.contains(y)
                        })
                        .map(|((_, x, y), text)| (*x, *y, text.clone()))
                        .collect();
                    for (x, y, text) in texts {
                        let _ = tx.send(ServerMessage::BlockText { dim, x, y, text });
                    }
                    // A chunk coming into existence for the first time has a chance
                    // to seed dimension-appropriate creatures into its terrain.
                    if fresh {
                        match dim {
                            Dimension::Overworld => {
                                maybe_spawn_in_chunk(&shared, cx, cy);
                                maybe_spawn_spiders(&shared, cx, cy);
                                maybe_spawn_snakes(&shared, cx, cy);
                                maybe_spawn_cats(&shared, cx, cy);
                                maybe_spawn_puppies(&shared, cx, cy);
                                maybe_spawn_horses(&shared, cx, cy);
                                maybe_spawn_knights(&shared, cx, cy);
                                maybe_spawn_musketeers(&shared, cx, cy);
                                maybe_spawn_farmers(&shared, cx, cy);
                                maybe_realize_structures(&shared, cx, cy);
                            }
                            Dimension::Underworld => {
                                maybe_spawn_charred_skeletons(&shared, cx, cy);
                                maybe_spawn_demons(&shared, cx, cy);
                                maybe_spawn_gargoyles(&shared, cx, cy);
                                maybe_spawn_orcs(&shared, cx, cy);
                                maybe_spawn_orc_mages(&shared, cx, cy);
                                maybe_spawn_dragon(&shared, cx, cy);
                                maybe_spawn_minotaur(&shared, cx, cy);
                                maybe_spawn_ash_twisters(&shared, cx, cy);
                                maybe_spawn_necromancers_ash(&shared, cx, cy);
                            }
                            // The arena seeds no ambient life from its terrain; its
                            // lone boss is raised by `ensure_arena_boss` in the tick
                            // loop instead.
                            Dimension::Arena => {}
                        }
                    }
                }
                ClientMessage::SetBlock {
                    x,
                    y,
                    block: _,
                    held,
                } => {
                    let dim = shared.dim_of(id);
                    // Mining is gated by the player's melee reach — the same limit
                    // that governs attacks and placement.
                    let in_reach = {
                        let entities = shared.entities(dim).lock();
                        entities.get(id).is_some_and(|p| {
                            let (pw, ph) = p.size();
                            aabb_gap(
                                p.x,
                                p.y,
                                pw,
                                ph,
                                x as f32 * TILE_SIZE,
                                y as f32 * TILE_SIZE,
                                TILE_SIZE,
                                TILE_SIZE,
                            ) <= PLAYER_ATTACK_REACH
                        })
                    };
                    if !in_reach {
                        // Out of range: resync the cell so the client's optimistic
                        // break is undone.
                        let actual = shared.world(dim).lock().get(x, y);
                        shared.send_to(
                            id,
                            ServerMessage::BlockUpdate {
                                dim,
                                x,
                                y,
                                block: actual,
                            },
                        );
                        continue;
                    }
                    // A locked chest resists breaking: only a player who has
                    // unlocked it this session (knows its password) may break it.
                    // Otherwise reject and resync the cell.
                    let target = shared.world(dim).lock().get(x, y);
                    if crate::block::is_locked_chest(target)
                        && !shared.chest_authed(id, dim, x, y)
                    {
                        shared.send_to(
                            id,
                            ServerMessage::BlockUpdate {
                                dim,
                                x,
                                y,
                                block: target,
                            },
                        );
                        continue;
                    }
                    // Breaking: clear the cell and drop its block on the ground
                    // for the player to walk over and collect.
                    let mined = {
                        let mut w = shared.world(dim).lock();
                        let prev = w.get(x, y);
                        if prev != crate::block::AIR && w.set(x, y, crate::block::AIR) {
                            Some(prev)
                        } else {
                            None
                        }
                    };
                    if let Some(prev) = mined {
                        shared.broadcast_dim(
                            dim,
                            ServerMessage::BlockUpdate {
                                dim,
                                x,
                                y,
                                block: crate::block::AIR,
                            },
                        );
                        // A mined campfire forgets its burn timer (it drops as a
                        // plain unlit campfire via mined_drop). A snuffed trail-fire
                        // cell likewise stops being tracked.
                        if crate::block::is_campfire(prev) {
                            shared.campfires.lock().remove(&(dim, x, y));
                        }
                        if crate::block::is_fire(prev) {
                            shared.trail_fires.lock().remove(&(dim, x, y));
                        }
                        // A mined sign or quest board forgets whatever was written on
                        // it, so a fresh board placed in the same cell starts blank.
                        if crate::block::is_text_block(prev) {
                            shared.block_text.lock().remove(&(dim, x, y));
                        }
                        // A mined chest (plain or locked) spills its stored items
                        // onto the ground and drops its block (a locked chest reverts
                        // to a plain chest via mined_drop; the gold is lost).
                        if crate::block::is_any_chest(prev) {
                            shared.spill_chest(dim, x, y);
                        }
                        // Tool-gated blocks (stone, iron ore) only yield a drop
                        // when mined with a strong enough pickaxe; broken with too
                        // weak a tool they crumble to nothing. Leaves roll between
                        // a stick, an apple, or nothing.
                        if crate::block::drops_when_mined(prev, held)
                            && let Some(drop) =
                                crate::block::mined_drop_rolled(prev, shared.rand_unit())
                        {
                            spawn_drop(&shared, dim, x, y, drop);
                        }
                        // Logs are special: breaking one updates the placed-log
                        // bookkeeping, an axe fells the whole tree, and any leaves
                        // it leaves stranded decay (still shedding their drops).
                        if prev == crate::block::LOG {
                            let was_placed = shared.placed_logs.lock().remove(&(dim, x, y));
                            let mut removed_logs = vec![(x, y)];
                            // An axe sweeps through a naturally-grown trunk, felling
                            // every connected log at once; a player-placed log is
                            // exempt and just breaks on its own.
                            if !was_placed && crate::block::is_axe(held) {
                                for (lx, ly) in shared.chop_connected_logs(dim, x, y) {
                                    shared.broadcast_dim(
                                        dim,
                                        ServerMessage::BlockUpdate {
                                            dim,
                                            x: lx,
                                            y: ly,
                                            block: crate::block::AIR,
                                        },
                                    );
                                    spawn_drop(&shared, dim, lx, ly, crate::block::LOG);
                                    removed_logs.push((lx, ly));
                                }
                            }
                            for (lx, ly, drop) in
                                shared.decay_unsupported_leaves(dim, &removed_logs)
                            {
                                shared.broadcast_dim(
                                    dim,
                                    ServerMessage::BlockUpdate {
                                        dim,
                                        x: lx,
                                        y: ly,
                                        block: crate::block::AIR,
                                    },
                                );
                                if let Some(drop) = drop {
                                    spawn_drop(&shared, dim, lx, ly, drop);
                                }
                            }
                        }
                        // Breaking any part of a rope ladder reels in the whole
                        // dangling run: the cells below (and above) collapse with
                        // it. Only the cell the player struck drops an item (handled
                        // above), so a long run yields one rope ladder, not many.
                        if prev == crate::block::ROPE_LADDER {
                            for (cx, cy) in shared.collapse_rope_ladder(dim, x, y) {
                                shared.broadcast_dim(
                                    dim,
                                    ServerMessage::BlockUpdate {
                                        dim,
                                        x: cx,
                                        y: cy,
                                        block: crate::block::AIR,
                                    },
                                );
                            }
                        }
                        // A door is two cells tall: breaking either half takes its
                        // partner with it. The struck cell already dropped a single
                        // door (above); the partner is cleared silently so a door
                        // never yields two items.
                        if crate::block::is_door(prev) {
                            let other_y = if crate::block::is_door_bottom(prev) {
                                y - 1 // struck the lower half: clear the top above
                            } else {
                                y + 1 // struck the upper half: clear the lower below
                            };
                            let cleared = {
                                let mut w = shared.world(dim).lock();
                                crate::block::is_door(w.get(x, other_y))
                                    && w.set(x, other_y, crate::block::AIR)
                            };
                            if cleared {
                                shared.broadcast_dim(
                                    dim,
                                    ServerMessage::BlockUpdate {
                                        dim,
                                        x,
                                        y: other_y,
                                        block: crate::block::AIR,
                                    },
                                );
                            }
                        }
                        // Mining wears the held tool: a pickaxe's intended job
                        // costs little, a sword or axe used to dig wears twice as fast.
                        shared.wear_tool(id, held, crate::block::mine_wear(held));
                    }
                }
                ClientMessage::PlaceBlock { x, y, slot } => {
                    // A block may only be placed into an empty cell that is
                    // orthogonally adjacent to an existing block, so players build
                    // off the world rather than dropping blocks into open air. A
                    // ladder is stricter: it mounts on the side of a wall, so it
                    // needs a solid block to the left or right (or a ladder
                    // directly above, to extend a run downward).
                    let dim = shared.dim_of(id);
                    let held_slot = shared.peek_slot(id, slot as usize);
                    let placing_ladder = held_slot.is_some_and(crate::block::is_climbable);
                    // A rope ladder may additionally hang from a solid block
                    // directly above (anchored to the ground at a shaft's mouth),
                    // since it unrolls down into open air rather than clinging to a
                    // wall.
                    let placing_rope = held_slot.is_some_and(crate::block::is_rope_ladder);
                    let supported = {
                        let mut world = shared.world(dim).lock();
                        if world.get(x, y) != crate::block::AIR {
                            false
                        } else if placing_ladder {
                            world.solid(x - 1, y)
                                || world.solid(x + 1, y)
                                || crate::block::is_climbable(world.get(x, y - 1))
                                || (placing_rope && world.solid(x, y - 1))
                        } else {
                            [(1, 0), (-1, 0), (0, 1), (0, -1)]
                                .iter()
                                .any(|(dx, dy)| world.get(x + dx, y + dy) != crate::block::AIR)
                        }
                    };
                    // Placement is also gated by the player's melee reach, so a
                    // block can only go where the player could swing — the same
                    // limit that governs attacks.
                    let in_reach = {
                        let entities = shared.entities(dim).lock();
                        entities.get(id).is_some_and(|p| {
                            let (pw, ph) = p.size();
                            aabb_gap(
                                p.x,
                                p.y,
                                pw,
                                ph,
                                x as f32 * TILE_SIZE,
                                y as f32 * TILE_SIZE,
                                TILE_SIZE,
                                TILE_SIZE,
                            ) <= PLAYER_ATTACK_REACH
                        })
                    };
                    if !supported || !in_reach {
                        // Reject: resync the cell's true contents and the inventory
                        // so the client's optimistic placement is undone.
                        let actual = shared.world(dim).lock().get(x, y);
                        shared.send_to(
                            id,
                            ServerMessage::BlockUpdate {
                                dim,
                                x,
                                y,
                                block: actual,
                            },
                        );
                        shared.send_inventory(id);
                        continue;
                    }
                    // Read (without removing) the block to place from the player's
                    // own slot, so they can only place what they actually hold —
                    // and so a non-placeable item (e.g. a worn tool) is never
                    // taken out and refunded, which would reset its durability.
                    match shared.peek_slot(id, slot as usize) {
                        // Plain items (bark, sticks, tools) can't be placed —
                        // leave the slot untouched and just resync it.
                        Some(block) if !shared.world(dim).lock().registry.is_placeable(block) => {
                            shared.send_inventory(id);
                        }
                        Some(_) => {
                            // Now commit: spend the block from the slot and place it.
                            if let Some(block) = shared.take_from_slot(id, slot as usize) {
                                if block == crate::block::ROPE_LADDER {
                                    // A rope ladder unrolls downward from the target
                                    // cell, filling the shaft until it bottoms out or
                                    // its rope runs out. One placed item, many cells.
                                    let filled = shared.roll_rope_ladder(dim, x, y);
                                    if filled.is_empty() {
                                        shared.add_item(id, block); // nothing placed: refund
                                    } else {
                                        for (fx, fy) in filled {
                                            shared.broadcast_dim(
                                                dim,
                                                ServerMessage::BlockUpdate {
                                                    dim,
                                                    x: fx,
                                                    y: fy,
                                                    block,
                                                },
                                            );
                                        }
                                    }
                                } else if block == crate::block::DOOR {
                                    // A door stands two cells tall: the lower half
                                    // at the target and an upper half directly
                                    // above. Both must be clear (the lower cell's
                                    // emptiness and support were checked above; the
                                    // cell above is checked here).
                                    let placed = {
                                        let mut world = shared.world(dim).lock();
                                        crate::world::in_bounds(x, y - 1)
                                            && world.get(x, y - 1) == crate::block::AIR
                                            && world.set(x, y, crate::block::DOOR)
                                            && {
                                                world.set(x, y - 1, crate::block::DOOR_TOP);
                                                true
                                            }
                                    };
                                    if placed {
                                        for (cy, b) in [
                                            (y, crate::block::DOOR),
                                            (y - 1, crate::block::DOOR_TOP),
                                        ] {
                                            shared.broadcast_dim(
                                                dim,
                                                ServerMessage::BlockUpdate {
                                                    dim,
                                                    x,
                                                    y: cy,
                                                    block: b,
                                                },
                                            );
                                        }
                                    } else {
                                        // No room for the upper half: refund and
                                        // resync the lower cell the client guessed.
                                        shared.add_item(id, block);
                                        let actual = shared.world(dim).lock().get(x, y);
                                        shared.send_to(
                                            id,
                                            ServerMessage::BlockUpdate {
                                                dim,
                                                x,
                                                y,
                                                block: actual,
                                            },
                                        );
                                    }
                                } else if shared.world(dim).lock().set(x, y, block) {
                                    // Remember player-placed logs so an axe's
                                    // tree-felling spares what the player built.
                                    if block == crate::block::LOG {
                                        shared.placed_logs.lock().insert((dim, x, y));
                                    }
                                    shared.broadcast_dim(
                                        dim,
                                        ServerMessage::BlockUpdate { dim, x, y, block },
                                    );
                                } else {
                                    // Cell was occupied: refund the spent block.
                                    shared.add_item(id, block);
                                }
                            }
                            shared.send_inventory(id);
                        }
                        None => {
                            // Empty slot: correct the client's optimistic guess by
                            // resending the cell's true contents and inventory.
                            let actual = shared.world(dim).lock().get(x, y);
                            shared.send_to(
                                id,
                                ServerMessage::BlockUpdate {
                                    dim,
                                    x,
                                    y,
                                    block: actual,
                                },
                            );
                            shared.send_inventory(id);
                        }
                    }
                }
                ClientMessage::UseBucket { x, y, slot } => {
                    let dim = shared.dim_of(id);
                    // Gated by the player's melee reach, the same limit governing
                    // mining and placement.
                    let in_reach = {
                        let entities = shared.entities(dim).lock();
                        entities.get(id).is_some_and(|p| {
                            let (pw, ph) = p.size();
                            aabb_gap(
                                p.x,
                                p.y,
                                pw,
                                ph,
                                x as f32 * TILE_SIZE,
                                y as f32 * TILE_SIZE,
                                TILE_SIZE,
                                TILE_SIZE,
                            ) <= PLAYER_ATTACK_REACH
                        })
                    };
                    if in_reach {
                        shared.use_bucket(id, x, y, slot as usize);
                    } else {
                        // Out of range: resync the cell and inventory to undo the
                        // client's optimistic use.
                        let actual = shared.world(dim).lock().get(x, y);
                        shared.send_to(
                            id,
                            ServerMessage::BlockUpdate {
                                dim,
                                x,
                                y,
                                block: actual,
                            },
                        );
                        shared.send_inventory(id);
                    }
                }
                ClientMessage::UseFireKey { slot } => {
                    shared.use_fire_key(id, slot as usize);
                }
                ClientMessage::UseArenaKey { slot } => {
                    shared.use_arena_key(id, slot as usize);
                }
                ClientMessage::CastSpell { slot, tx, ty } => {
                    shared.cast_spell(id, slot as usize, tx, ty);
                }
                ClientMessage::DragonBreath { tx, ty } => {
                    shared.dragon_breath(id, tx, ty);
                }
                ClientMessage::FireMusket { slot, tx, ty } => {
                    shared.fire_musket(id, slot as usize, tx, ty);
                }
                ClientMessage::EmptyHandBreath { tx, ty } => {
                    shared.empty_hand_breath(id, tx, ty);
                }
                ClientMessage::ToggleDoor { x, y } => {
                    let dim = shared.dim_of(id);
                    // Gated by the player's melee reach, the same limit governing
                    // mining, placement, and bucket use.
                    let in_reach = {
                        let entities = shared.entities(dim).lock();
                        entities.get(id).is_some_and(|p| {
                            let (pw, ph) = p.size();
                            aabb_gap(
                                p.x,
                                p.y,
                                pw,
                                ph,
                                x as f32 * TILE_SIZE,
                                y as f32 * TILE_SIZE,
                                TILE_SIZE,
                                TILE_SIZE,
                            ) <= PLAYER_ATTACK_REACH
                        })
                    };
                    if in_reach {
                        for (cx, cy, block) in shared.toggle_door(dim, x, y) {
                            shared.broadcast_dim(
                                dim,
                                ServerMessage::BlockUpdate {
                                    dim,
                                    x: cx,
                                    y: cy,
                                    block,
                                },
                            );
                        }
                    } else {
                        // Out of range: resync the cell to undo the client's
                        // optimistic toggle.
                        let actual = shared.world(dim).lock().get(x, y);
                        shared.send_to(
                            id,
                            ServerMessage::BlockUpdate {
                                dim,
                                x,
                                y,
                                block: actual,
                            },
                        );
                    }
                }
                ClientMessage::MoveItem { from, to } => {
                    shared.move_item(id, from as usize, to as usize);
                    shared.send_inventory(id);
                }
                ClientMessage::DropItem { slot, all, dir } => {
                    shared.drop_item(id, slot as usize, all, dir);
                    shared.send_inventory(id);
                }
                ClientMessage::Craft { recipe } => {
                    shared.craft(id, recipe as usize);
                    shared.send_inventory(id);
                }
                ClientMessage::Smelt {
                    recipe,
                    count,
                    fuel,
                } => {
                    shared.smelt(id, recipe as usize, count, fuel);
                    shared.send_inventory(id);
                }
                ClientMessage::Repair { item } => {
                    shared.repair(id, item);
                    shared.send_inventory(id);
                }
                ClientMessage::Eat { slot } => {
                    let dim = shared.dim_of(id);
                    let (msgs, respawn) = shared.eat(id, slot as usize);
                    for m in msgs {
                        shared.broadcast_dim(dim, m);
                    }
                    if let Some(info) = respawn {
                        shared.finish_respawn(info, dim);
                    }
                    shared.send_inventory(id);
                }
                ClientMessage::SetRespawn { x, y } => {
                    shared.set_respawn(id, x, y);
                    // The home waypoint follows the respawn point; resync it.
                    shared.send_waypoints(id);
                }
                ClientMessage::AddWaypoint { x, y, color } => {
                    shared.add_waypoint(id, Waypoint { x, y, color });
                }
                ClientMessage::RemoveWaypoint { x, y } => {
                    shared.remove_waypoint(id, x, y);
                }
                ClientMessage::FuelCampfire { x, y, fuel } => {
                    shared.fuel_campfire(id, x, y, fuel);
                }
                ClientMessage::WriteBlockText { x, y, text } => {
                    shared.write_block_text(id, x, y, text);
                }
                ClientMessage::OpenChest { x, y, password } => {
                    shared.open_chest(id, x, y, password);
                }
                ClientMessage::CloseChest => {
                    shared.close_chest(id);
                }
                ClientMessage::MoveChestItem { x, y, from, to } => {
                    shared.move_chest_item(id, x, y, from, to);
                }
                ClientMessage::ReinforceChest { x, y, password } => {
                    shared.reinforce_chest(id, x, y, password);
                }
                ClientMessage::Cook {
                    x,
                    y,
                    recipe,
                    count,
                } => {
                    shared.cook(id, x, y, recipe as usize, count);
                }
                ClientMessage::PlayerMove { x, y, vx, vy } => {
                    let dim = shared.dim_of(id);
                    if let Some(e) = shared.entities(dim).lock().get_mut(id) {
                        e.x = x;
                        e.y = y;
                        e.vx = vx;
                        e.vy = vy;
                    }
                    // Falling off the bottom of the overworld (or climbing out the
                    // top of the underworld) moves the player across dimensions; a
                    // transition supersedes the stale position broadcast.
                    if shared.maybe_transition(id, dim, x, y) {
                        continue;
                    }
                    // Pass the reported velocity through so remote clients flip the
                    // avatar to the right facing (`vx`'s sign) and play its walk cycle.
                    shared.broadcast_dim_except(
                        dim,
                        id,
                        ServerMessage::EntityMoved { id, x, y, vx, vy },
                    );
                }
                ClientMessage::SetBoating { on } => {
                    let dim = shared.dim_of(id);
                    // Record the rider state on the player entity, but only tell the
                    // others when it actually flips (a boat toggle), not every frame.
                    let changed = {
                        let mut entities = shared.entities(dim).lock();
                        match entities.get_mut(id) {
                            Some(e) if e.boating != on => {
                                e.boating = on;
                                true
                            }
                            _ => false,
                        }
                    };
                    // Share just the pose change so every other client draws or
                    // stows this player's boat (the rider simulates it locally).
                    if changed {
                        shared.broadcast_dim_except(
                            dim,
                            id,
                            ServerMessage::EntityBoating { id, on },
                        );
                    }
                }
                ClientMessage::SetRiding { horse } => {
                    let dim = shared.dim_of(id);
                    // Resolve the request to the horse the player ends up on. A mount
                    // (`Some`) must name a horse this player has tamed, in this
                    // dimension, and within reach; a dismount (`None`) always clears.
                    // Anything that fails validation leaves the rider as they were.
                    let new_riding: Option<Option<EntityId>> = {
                        let entities = shared.entities(dim).lock();
                        match horse {
                            None => Some(None),
                            Some(hid) => {
                                let rider = match entities.get(id).map(|a| (&a.kind, a.x, a.y, a.size())) {
                                    Some((EntityKind::Player { name }, ax, ay, (aw, ah)))
                                        if !name.is_empty() =>
                                    {
                                        Some((name.clone(), ax, ay, aw, ah))
                                    }
                                    _ => None,
                                };
                                match (rider, entities.get(hid)) {
                                    // A tamed horse or a summoned white-dragon steed the
                                    // player owns, within reach, can be mounted.
                                    (Some((name, ax, ay, aw, ah)), Some(b))
                                        if (matches!(&b.kind, EntityKind::Horse { owner: Some(o) } if *o == name)
                                            || matches!(&b.kind, EntityKind::WhiteDragon { owner: Some(o) } if *o == name))
                                            && aabb_gap(
                                                ax, ay, aw, ah, b.x, b.y, b.size().0, b.size().1,
                                            ) <= PLAYER_ATTACK_REACH =>
                                    {
                                        Some(Some(hid))
                                    }
                                    _ => None,
                                }
                            }
                        }
                    };
                    if let Some(riding) = new_riding {
                        let changed = {
                            let mut entities = shared.entities(dim).lock();
                            match entities.get_mut(id) {
                                Some(e) if e.riding != riding => {
                                    e.riding = riding;
                                    true
                                }
                                _ => false,
                            }
                        };
                        // Share the pose with everyone — including the rider, who
                        // learns its mount is confirmed — so all clients draw the rider
                        // on the combined horse sprite and hide the now-ridden horse.
                        // The tick loop glues the horse beneath its rider.
                        if changed {
                            shared.broadcast_dim(dim, ServerMessage::EntityRiding { id, horse: riding });
                        }
                    }
                }
                ClientMessage::SetControlling { dragon } => {
                    let dim = shared.dim_of(id);
                    // Resolve the request. Beginning control (`Some`) must name one of this
                    // player's own white-dragon steeds, present in their dimension and not
                    // currently ridden by anyone (you can't pilot a mount someone's aboard).
                    // Stopping (`None`) always clears. Anything invalid leaves them as they were.
                    let new_controlling: Option<Option<EntityId>> = {
                        let entities = shared.entities(dim).lock();
                        match dragon {
                            None => Some(None),
                            // Telepathic piloting is barred in the arena: a challenger
                            // must *ride* their steed up to fight the airborne boss,
                            // not pilot it from safety on the floor. (Stopping still works.)
                            Some(_) if dim == Dimension::Arena => None,
                            Some(did) => {
                                let owner = match entities.get(id).map(|a| &a.kind) {
                                    Some(EntityKind::Player { name }) if !name.is_empty() => {
                                        Some(name.clone())
                                    }
                                    _ => None,
                                };
                                let ridden = entities.values().any(|e| e.riding == Some(did));
                                match (owner, entities.get(did)) {
                                    (Some(name), Some(b))
                                        if matches!(&b.kind, EntityKind::WhiteDragon { owner: Some(o) } if *o == name)
                                            && !ridden =>
                                    {
                                        Some(Some(did))
                                    }
                                    _ => None,
                                }
                            }
                        }
                    };
                    if let Some(controlling) = new_controlling {
                        // Apply the change and report which steed changed hands: the new
                        // one when starting, the previously-piloted one when stopping
                        // (read before we overwrite it).
                        let steed = {
                            let mut entities = shared.entities(dim).lock();
                            match entities.get_mut(id) {
                                Some(e) if e.controlling != controlling => {
                                    let steed = controlling.or(e.controlling);
                                    e.controlling = controlling;
                                    e.control_dx = 0.0;
                                    e.control_dy = 0.0;
                                    steed
                                }
                                _ => None,
                            }
                        };
                        // Echo the link (or its release) back so the piloting client adopts
                        // it as the authoritative control state. `id` in the message is the
                        // steed; a non-`None` controller is the pilot's entity id.
                        if let Some(steed) = steed {
                            shared.broadcast_dim(
                                dim,
                                ServerMessage::EntityControlled {
                                    id: steed,
                                    controller: controlling.map(|_| id),
                                },
                            );
                        }
                    }
                }
                ClientMessage::ControlDragon { dx, dy } => {
                    // Record the latest movement intent for the tick loop to fly the steed.
                    // Clamp to the unit range so a misbehaving client can't fling it about.
                    let dim = shared.dim_of(id);
                    if let Some(e) = shared.entities(dim).lock().get_mut(id) {
                        e.control_dx = dx.clamp(-1.0, 1.0);
                        e.control_dy = dy.clamp(-1.0, 1.0);
                    }
                }
                ClientMessage::Attack { target, held } => {
                    let dim = shared.dim_of(id);
                    // Pets (cats, puppies and horses) are sacrosanct: a swing at one
                    // never deals damage. A wild pet can only be fed (which tames
                    // it); a player's own cat/puppy is sat down or stood up by the
                    // click, while a tamed horse is instead mounted by right-click
                    // (handled in the client's interact path), so a swing at one just
                    // bounces off; anyone else's pet ignores the swing. Either way the
                    // damage path is skipped.
                    let pet = shared
                        .entities(dim)
                        .lock()
                        .get(target)
                        .and_then(|e| match &e.kind {
                            EntityKind::Cat { owner, .. } | EntityKind::Puppy { owner, .. } => {
                                Some((false, owner.clone()))
                            }
                            EntityKind::Horse { owner } => Some((true, owner.clone())),
                            _ => None,
                        });
                    if let Some((is_horse, owner)) = pet {
                        match (is_horse, owner) {
                            // Wild horse: an apple tames it. Tamed horse: nothing on a
                            // swing (mounting is a right-click interact).
                            (true, None) => shared.try_feed_horse(id, target, held, dim),
                            (true, Some(_)) => {}
                            // Wild cat/puppy: cooked meat tames it. Tamed: sit toggle.
                            (false, None) => shared.try_feed_pet(id, target, held, dim),
                            (false, Some(_)) => shared.toggle_pet_sit(id, target, dim),
                        }
                        continue;
                    }
                    // Knights are sacrosanct too: a swing never harms one. A wild
                    // knight is recruited by offering it a tungsten ingot; a recruited
                    // one just shrugs off the swing. Either way the damage path is
                    // skipped.
                    let knight_owner = shared
                        .entities(dim)
                        .lock()
                        .get(target)
                        .and_then(|e| match &e.kind {
                            EntityKind::Knight { owner } => Some(owner.clone()),
                            _ => None,
                        });
                    if let Some(owner) = knight_owner {
                        if owner.is_none() {
                            shared.try_recruit_knight(id, target, held, dim);
                        }
                        continue;
                    }
                    // Musketeers are sacrosanct too, and recruited the same way: a wild
                    // one is won with a tungsten ingot, a recruited one shrugs the swing
                    // off. Either way the damage path is skipped.
                    let musketeer_owner = shared
                        .entities(dim)
                        .lock()
                        .get(target)
                        .and_then(|e| match &e.kind {
                            EntityKind::Musketeer { owner } => Some(owner.clone()),
                            _ => None,
                        });
                    if let Some(owner) = musketeer_owner {
                        if owner.is_none() {
                            shared.try_recruit_musketeer(id, target, held, dim);
                        }
                        continue;
                    }
                    // Mages are sacrosanct too: nothing harms a mage — it can't be
                    // recruited by hand (only the restore spell makes one), so a swing
                    // at one simply does nothing.
                    let is_mage = shared
                        .entities(dim)
                        .lock()
                        .get(target)
                        .is_some_and(|e| e.kind.is_mage());
                    if is_mage {
                        continue;
                    }
                    // A white-dragon steed is sacrosanct too: a swing never harms one
                    // (it is mounted by right-click, like a horse), so it just bounces.
                    let is_white_dragon = shared
                        .entities(dim)
                        .lock()
                        .get(target)
                        .is_some_and(|e| e.kind.is_white_dragon());
                    if is_white_dragon {
                        continue;
                    }
                    // A gargoyle is hewn from stone: every weapon but a **pickaxe**
                    // glances off its hide, dealing nothing. Only a mining tool chips it
                    // apart, so a swing with anything else (or a bare fist) just bounces.
                    let is_gargoyle = shared
                        .entities(dim)
                        .lock()
                        .get(target)
                        .is_some_and(|e| matches!(e.kind, EntityKind::Gargoyle));
                    if is_gargoyle && !crate::block::is_pickaxe(held) {
                        continue;
                    }
                    // A farmer is a friendly bystander: a swing never harms one. Offer it
                    // an iron ingot instead and it trades back a windfall of food (apples
                    // or cooked meat, its choice); any other swing simply does nothing.
                    // Either way the damage path is skipped.
                    let is_farmer = shared
                        .entities(dim)
                        .lock()
                        .get(target)
                        .is_some_and(|e| e.kind.is_farmer());
                    if is_farmer {
                        shared.try_trade_farmer(id, target, held, dim);
                        continue;
                    }
                    // Validate reach and, if good, compute the knockback shoving
                    // the target away from the attacker.
                    let knockback = {
                        let entities = shared.entities(dim).lock();
                        match (entities.get(id), entities.get(target)) {
                            (Some(a), Some(b)) => {
                                let (aw, ah) = a.size();
                                let (bw, bh) = b.size();
                                if aabb_gap(a.x, a.y, aw, ah, b.x, b.y, bw, bh)
                                    <= PLAYER_ATTACK_REACH
                                {
                                    let dir = if b.x + bw * 0.5 >= a.x + aw * 0.5 {
                                        1.0
                                    } else {
                                        -1.0
                                    };
                                    Some((dir * KNOCKBACK_X, -KNOCKBACK_Y))
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        }
                    };
                    if let Some(kb) = knockback {
                        // Snapshot the victim's kind and position before the hit,
                        // so a fatal blow can spill its loot where it fell.
                        let victim = {
                            let entities = shared.entities(dim).lock();
                            entities.get(target).map(|e| (e.kind.clone(), e.x, e.y))
                        };
                        // Mark a struck enemy as this player's recruited knight's quarry,
                        // so the knight charges whatever its owner attacks first.
                        if victim.as_ref().is_some_and(|(k, _, _)| is_hostile(k)) {
                            shared.knight_targets.lock().insert(id, target);
                        }
                        let respawn_target = shared.respawn_target(target);
                        // A bare hand wrapped in an active dragon plate ward hits harder
                        // than any forged weapon; otherwise the held item's own damage
                        // applies (a warded player swinging a weapon uses the weapon).
                        let warded_fist = held == crate::block::AIR
                            && shared
                                .entities(dim)
                                .lock()
                                .get(id)
                                .is_some_and(|e| e.dragon_plate_timer > 0.0);
                        let damage = if warded_fist {
                            crate::block::DRAGON_PLATE_FIST_DAMAGE
                        } else {
                            crate::block::attack_damage(held)
                        };
                        let (msgs, respawn) = {
                            let mut entities = shared.entities(dim).lock();
                            apply_damage(&mut entities, target, damage, kb, dim, respawn_target)
                        };
                        for m in msgs {
                            shared.broadcast_dim(dim, m);
                        }
                        if let Some(info) = respawn {
                            shared.finish_respawn(info, dim);
                        }
                        // If that killed a creature (it's no longer in the world),
                        // drop whatever it carries — animals leave raw meat, a dark
                        // knight spills a tungsten haul, and the arena boss leaves a
                        // whole chest of loot where it fell (and never rises again).
                        if let Some((kind, vx, vy)) = victim
                            && shared.entities(dim).lock().get(target).is_none()
                        {
                            // Slaying a monster banks mana for the killer (tougher
                            // foes are worth more; animals and companions nothing).
                            let reward = creature_mana(&kind);
                            if reward > 0 {
                                shared.add_mana(id, reward);
                            }
                            if matches!(kind, EntityKind::DemonKing) {
                                spawn_boss_chest(&shared, dim, vx);
                                shared.record_demon_king_slain();
                            } else if matches!(kind, EntityKind::Twinscale) {
                                shared.record_twinscale_slain();
                                let cx = (vx / TILE_SIZE) as i32;
                                let cy = (vy / TILE_SIZE) as i32;
                                for (item, n) in twinscale_loot() {
                                    for _ in 0..n {
                                        spawn_drop(&shared, dim, cx, cy, item);
                                    }
                                }
                            } else {
                                let cx = (vx / TILE_SIZE) as i32;
                                let cy = (vy / TILE_SIZE) as i32;
                                // A dark knight spills a tungsten haul; a necromancer
                                // rarely yields its summoner spellbook; everything else
                                // drops whatever its kind carries (animals: raw meat).
                                let loot = if matches!(kind, EntityKind::DarkKnight) {
                                    dark_knight_loot(target)
                                } else if matches!(kind, EntityKind::Necromancer) {
                                    necromancer_loot(target)
                                } else {
                                    creature_loot(&kind).to_vec()
                                };
                                for (item, n) in loot {
                                    for _ in 0..n {
                                        spawn_drop(&shared, dim, cx, cy, item);
                                    }
                                }
                            }
                        }
                        // A landed swing wears the weapon: a sword's intended job
                        // costs little, a pickaxe swung as a weapon wears double.
                        shared.wear_tool(id, held, crate::block::attack_wear(held));
                    }
                }
                ClientMessage::FallDamage { amount } => {
                    if amount > 0 {
                        let dim = shared.dim_of(id);
                        let respawn_target = shared.respawn_target(id);
                        let (msgs, respawn) = {
                            let mut entities = shared.entities(dim).lock();
                            apply_damage(&mut entities, id, amount, (0.0, 0.0), dim, respawn_target)
                        };
                        for m in msgs {
                            shared.broadcast_dim(dim, m);
                        }
                        if let Some(info) = respawn {
                            shared.finish_respawn(info, dim);
                        }
                    }
                }
                ClientMessage::Chat { text } => {
                    // Attribute the line to the sender's player name (falling back
                    // to a generic label before they've identified via Hello), cap
                    // its length, and fan it out to everyone — sender included.
                    let trimmed = text.trim();
                    // A `/`-prefixed line from the admin (the host) is a moderation
                    // command, handled here instead of broadcast.
                    if is_admin && trimmed.starts_with('/') {
                        shared.handle_admin_command(id, trimmed);
                    } else if !trimmed.is_empty() {
                        let text: String = trimmed.chars().take(MAX_CHAT_LEN).collect();
                        let from = shared
                            .entities(shared.dim_of(id))
                            .lock()
                            .get(id)
                            .and_then(|e| match &e.kind {
                                EntityKind::Player { name } if !name.is_empty() => {
                                    Some(name.clone())
                                }
                                _ => None,
                            })
                            .unwrap_or_else(|| format!("Player {id}"));
                        shared.broadcast_all(ServerMessage::Chat { from, text });
                    }
                }
                // Toggle the sender's creator mode. Tracked so hostile creatures
                // skip creators when choosing a target.
                ClientMessage::SetCreator { on } if creator_allowed => {
                    if on {
                        shared.creators.lock().insert(id);
                    } else {
                        shared.creators.lock().remove(&id);
                    }
                }
                // --- Creator-mode commands: honored only when creator-allowed.
                ClientMessage::SetTime { t } if creator_allowed => {
                    let t = t.rem_euclid(1.0);
                    // Rewind the clock's origin so `elapsed()` now reads `t` of a day.
                    let elapsed = Duration::from_secs_f32(t * daylight::DAY_LENGTH_SECS);
                    let new_start = Instant::now()
                        .checked_sub(elapsed)
                        .unwrap_or_else(Instant::now);
                    *shared.start.lock() = new_start;
                    shared.broadcast_all(ServerMessage::TimeOfDay { t });
                }
                ClientMessage::AdvanceDay if creator_allowed => {
                    // Move the clock's origin back a full day so total elapsed in-world
                    // time jumps forward one day/night cycle (the time-of-day phase is
                    // unchanged, but countdowns like Twinscale's five days advance).
                    {
                        let mut start = shared.start.lock();
                        *start = start
                            .checked_sub(Duration::from_secs_f32(daylight::DAY_LENGTH_SECS))
                            .unwrap_or(*start);
                    }
                    shared.broadcast_all(ServerMessage::TimeOfDay {
                        t: shared.time_of_day(),
                    });
                }
                ClientMessage::SpawnEntity { kind, x, y } if creator_allowed => {
                    // Never let a creator spawn a player avatar (those are owned by
                    // a connection); only server-simulated creatures, into the
                    // dimension the creator is currently in.
                    if !kind.is_player() {
                        let dim = shared.dim_of(id);
                        let eid = shared.alloc_id();
                        let mut entity = Entity::new(eid, kind, x, y);
                        // A skull (hostile or friendly) rides its life on the `flee`
                        // timer; a creator-spawned one needs it primed or it would
                        // wink out on its first tick.
                        if matches!(entity.kind, EntityKind::Skull | EntityKind::FriendlySkull) {
                            entity.flee = SKULL_LIFETIME;
                        }
                        shared.entities(dim).lock().insert(entity.clone());
                        shared.broadcast_dim(dim, ServerMessage::EntitySpawn { entity });
                    }
                }
                ClientMessage::CreatorSetBlock { x, y, block } if creator_allowed => {
                    let dim = shared.dim_of(id);
                    if shared.world(dim).lock().set(x, y, block) {
                        shared.broadcast_dim(dim, ServerMessage::BlockUpdate { dim, x, y, block });
                    }
                }
                ClientMessage::CreatorSetBlocks { cells } if creator_allowed => {
                    // Stamp a structure: apply every cell that lands in a loaded
                    // chunk, then rebroadcast just those so all clients mirror it.
                    let dim = shared.dim_of(id);
                    let applied: Vec<(i32, i32, BlockId)> = {
                        let mut world = shared.world(dim).lock();
                        cells
                            .into_iter()
                            .filter(|&(x, y, block)| world.set(x, y, block))
                            .collect()
                    };
                    if !applied.is_empty() {
                        shared.broadcast_dim(
                            dim,
                            ServerMessage::BlocksUpdate {
                                dim,
                                cells: applied,
                            },
                        );
                    }
                }
                ClientMessage::GiveItem { item, count } if creator_allowed => {
                    // Reject unknown ids (and air) so a typo can't inject a bogus
                    // stack; the registry treats out-of-range ids as air.
                    let known = {
                        let dim = shared.dim_of(id);
                        let world = shared.world(dim).lock();
                        item != crate::block::AIR && (item as usize) < world.registry.len()
                    };
                    if known {
                        shared
                            .inventories
                            .lock()
                            .entry(id)
                            .or_default()
                            .add(item, count.max(1));
                        shared.send_inventory(id);
                    }
                }
                // Creator commands from a player without creator access are ignored.
                ClientMessage::SetCreator { .. }
                | ClientMessage::SetTime { .. }
                | ClientMessage::AdvanceDay
                | ClientMessage::SpawnEntity { .. }
                | ClientMessage::GiveItem { .. }
                | ClientMessage::CreatorSetBlock { .. }
                | ClientMessage::CreatorSetBlocks { .. } => {
                    log::debug!("ignoring creator command from unauthorized player {id}");
                }
            }
        }
    }
    .await;

    // Cleanup. Preserve the player's state so they resume where they left off.
    let dim = shared.dim_of(id);
    shared.clients.lock().remove(&id);
    shared.client_dim.lock().remove(&id);
    shared.fire_cd.lock().remove(&id);
    shared.fire_marks.lock().remove(&id);
    shared.knight_targets.lock().remove(&id);
    shared.musket_cd.lock().remove(&id);
    shared.spectate_return.lock().remove(&id);
    shared.creators.lock().remove(&id);
    // Forget any chest this player had open and the session unlocks they earned
    // (passwords must be re-entered after a reconnect).
    shared.chest_viewers.lock().remove(&id);
    shared.chest_auth.lock().remove(&id);
    let removed = shared.entities(dim).lock().remove(id);
    let inventory = shared.inventories.lock().remove(&id).unwrap_or_default();
    let mana = shared.mana.lock().remove(&id).unwrap_or(0);
    let respawn = shared.respawn_points.lock().remove(&id);
    let waypoints = shared.waypoints.lock().remove(&id).unwrap_or_default();
    if let Some(Entity {
        kind: EntityKind::Player { name },
        x,
        y,
        health,
        ..
    }) = removed
        && !name.is_empty()
    {
        shared.saved_players.lock().insert(
            name.clone(),
            SavedPlayer {
                name,
                x,
                y,
                health,
                inventory,
                dim,
                respawn,
                waypoints,
                mana,
            },
        );
    }
    shared.broadcast_dim_except(dim, id, ServerMessage::EntityDespawn { id });
    writer.abort();
    log::info!("player {id} disconnected");
    read_result
}

/// Read the persisted ban list from `path`, returning an empty set if the file
/// is absent or unreadable (a missing list simply means nobody is banned yet).
/// Lines that don't parse as an IP address are skipped, so a hand-edited file
/// with a stray comment or typo still loads the valid entries.
fn load_bans(path: &Path) -> HashSet<IpAddr> {
    let mut set = HashSet::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return set;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Ok(ip) = line.parse::<IpAddr>() {
            set.insert(ip);
        }
    }
    set
}

/// Default bind address for a publicly-hosted server.
pub fn host_bind(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))
}

/// Loopback bind with an ephemeral port, for the embedded singleplayer server.
pub fn local_bind() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_normalization_requires_a_real_password() {
        // A blank or whitespace-only password is rejected, so the "password
        // required" gate can't be bypassed with spaces.
        assert!(normalize_password("").is_err());
        assert!(normalize_password("   ").is_err());
        assert!(normalize_password("\t \n").is_err());
        // A real password is accepted and returned trimmed, matching how the
        // client sends it — so existing accounts still verify.
        assert_eq!(normalize_password("hunter2").unwrap(), "hunter2");
        assert_eq!(normalize_password("  hunter2  ").unwrap(), "hunter2");
    }

    #[test]
    fn restore_turns_foes_into_allies() {
        // An orc and a dark knight both become a knight recruited to the caster.
        let me = Some("ada".to_string());
        assert_eq!(
            restored_kind(&EntityKind::Orc, &me),
            Some(EntityKind::Knight { owner: me.clone() })
        );
        assert_eq!(
            restored_kind(&EntityKind::DarkKnight, &me),
            Some(EntityKind::Knight { owner: me.clone() })
        );
        // An orc mage becomes a mage recruited to the caster.
        assert_eq!(
            restored_kind(&EntityKind::OrcMage, &me),
            Some(EntityKind::Mage { owner: me.clone() })
        );
        // An enchanted demon is calmed back into an ordinary, ownerless demon.
        assert_eq!(
            restored_kind(&EntityKind::EnchantedDemon, &me),
            Some(EntityKind::Demon)
        );
        // A wild caster (None owner) leaves the restored knight/mage wild.
        assert_eq!(
            restored_kind(&EntityKind::Orc, &None),
            Some(EntityKind::Knight { owner: None })
        );
        // Everything else is beyond the spell's reach.
        for kind in [EntityKind::Zombie, EntityKind::Demon, EntityKind::Slime] {
            assert_eq!(restored_kind(&kind, &me), None);
            assert!(!is_restorable(&kind));
        }
        // The restorable set is exactly what `restored_kind` accepts.
        for kind in [
            EntityKind::Orc,
            EntityKind::DarkKnight,
            EntityKind::OrcMage,
            EntityKind::EnchantedDemon,
        ] {
            assert!(is_restorable(&kind));
        }
    }

    #[test]
    fn transform_swaps_kind_and_heals_to_new_full() {
        let mut entities = Entities::new();
        // An enchanted demon (flying, mounted-state irrelevant) restored to a demon.
        let mut ed = Entity::new(1, EntityKind::EnchantedDemon, 0.0, 0.0);
        ed.health = 3;
        ed.lunge = 0.5;
        entities.insert(ed);
        let msgs = transform_entity(&mut entities, 1, EntityKind::Demon);
        let e = entities.get(1).unwrap();
        assert!(matches!(e.kind, EntityKind::Demon));
        assert_eq!(e.health, EntityKind::Demon.max_health());
        assert_eq!(e.max_health, EntityKind::Demon.max_health());
        assert_eq!(e.lunge, 0.0);
        // It broadcasts a full respawn plus its fresh health.
        assert_eq!(msgs.len(), 2);
        // A missing id is a no-op.
        assert!(transform_entity(&mut entities, 999, EntityKind::Demon).is_empty());
    }
}
