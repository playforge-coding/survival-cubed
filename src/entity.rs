//! Entities: anything that lives in the world but isn't a block.
//!
//! Blocks are static cells on the world grid; entities are free-moving objects
//! addressed by a unique [`EntityId`] and positioned in pixel/world space. Both
//! client and server share these types so an entity can be described once and
//! sent over the wire (see [`crate::protocol`]).
//!
//! The player is "just" an entity — see [`EntityKind::Player`] — but a *special*
//! one: its position is authoritative from the client that owns it and the
//! server never runs AI on it. Every other kind (e.g. [`EntityKind::Slime`])
//! is simulated by the server's tick loop. That distinction is the whole point
//! of [`EntityKind::is_player`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::protocol::BlockId;

/// Unique identifier of a live entity. Allocated by the server; `0` is never
/// used so it can double as "no entity".
pub type EntityId = u32;

/// Collision/draw size (width, height) in pixels of a player avatar. Matches the
/// sprite art's native proportions (~11x16) so it draws unstretched, about a tile
/// tall and roughly zombie-sized.
pub const PLAYER_SIZE: (f32, f32) = (11.0, 16.0);
/// Collision/draw size (width, height) in pixels of a slime.
pub const SLIME_SIZE: (f32, f32) = (12.0, 12.0);
/// Collision/draw size (width, height) in pixels of a chicken.
pub const CHICKEN_SIZE: (f32, f32) = (12.0, 14.0);
/// Collision/draw size (width, height) in pixels of a goat.
pub const GOAT_SIZE: (f32, f32) = (16.0, 16.0);
/// Collision/draw size (width, height) in pixels of a cat — a small, low critter.
pub const CAT_SIZE: (f32, f32) = (15.0, 13.0);
/// Collision/draw size (width, height) in pixels of a puppy — a small, low critter
/// a touch longer than the cat, matching its art's proportions.
pub const PUPPY_SIZE: (f32, f32) = (18.0, 13.0);
/// Collision/draw size (width, height) in pixels of a horse — a tall, sturdy
/// grazer a touch larger than a goat, matching its art's proportions.
pub const HORSE_SIZE: (f32, f32) = (17.0, 14.0);
/// Collision/draw size (width, height) in pixels of a zombie.
pub const ZOMBIE_SIZE: (f32, f32) = (14.0, 19.0);
/// Collision/draw size (width, height) in pixels of a spider — low and wide.
pub const SPIDER_SIZE: (f32, f32) = (14.0, 10.0);
/// Collision/draw size (width, height) in pixels of a snake — a low, coiled
/// ambusher drawn from a 16x14 sheet.
pub const SNAKE_SIZE: (f32, f32) = (15.0, 11.0);
/// Collision/draw size (width, height) in pixels of a skeleton — a lanky
/// humanoid, the same build as the player.
pub const SKELETON_SIZE: (f32, f32) = (11.0, 16.0);
/// Collision/draw size (width, height) in pixels of a charred skeleton — the
/// same lanky build as the ordinary skeleton, scorched black.
pub const CHARRED_SKELETON_SIZE: (f32, f32) = (11.0, 16.0);
/// Collision/draw size (width, height) in pixels of a demon — a small, hunched
/// underworld fiend, shorter than a skeleton, matching its art's proportions.
pub const DEMON_SIZE: (f32, f32) = (10.0, 15.0);
/// Collision/draw size (width, height) in pixels of an enchanted demon — a demon
/// an [`EntityKind::OrcMage`] has empowered. Drawn from the same proportions as the
/// ordinary demon, lit with the mage's purple glamour.
pub const ENCHANTED_DEMON_SIZE: (f32, f32) = (10.0, 15.0);
/// Collision/draw size (width, height) in pixels of the demon king — the arena
/// boss. A towering fiend, drawn far larger than the rank-and-file demon so it
/// reads as the monarch of the depths.
pub const DEMON_KING_SIZE: (f32, f32) = (22.0, 30.0);
/// Collision/draw size (width, height) in pixels of a dragon — the underworld's
/// rare flying miniboss. A long, winged serpent drawn far wider than any other
/// creature, matching its art's low, broad proportions.
pub const DRAGON_SIZE: (f32, f32) = (31.0, 17.0);
/// Collision/draw size (width, height) in pixels of an orc mage — a robed
/// underworld spellcaster, leaner than the hulking brute it shares the depths with.
pub const ORC_MAGE_SIZE: (f32, f32) = (10.0, 13.0);
/// Collision/draw size (width, height) in pixels of an orc — a stocky underworld
/// brute, broader than the lanky skeletons it shares the depths with.
pub const ORC_SIZE: (f32, f32) = (12.0, 15.0);
/// Collision/draw size (width, height) in pixels of an ash twister — a tall,
/// narrow column of whirling ash drawn from a 16x16 sheet.
pub const ASH_TWISTER_SIZE: (f32, f32) = (12.0, 16.0);
/// Collision/draw size (width, height) in pixels of a necromancer — a hooded
/// ranged caster, the lean build of its art.
pub const NECROMANCER_SIZE: (f32, f32) = (9.0, 13.0);
/// Collision/draw size (width, height) in pixels of a skull — a small, bouncing
/// skeleton skull a necromancer summons.
pub const SKULL_SIZE: (f32, f32) = (10.0, 11.0);
/// Collision/draw size (width, height) in pixels of a mage — a robed spellcaster
/// conjured by the [`crate::block::RESTORE_SPELL`], the lean build of its art.
pub const MAGE_SIZE: (f32, f32) = (9.0, 12.0);
/// Collision/draw size (width, height) in pixels of a knight — a compact armoured
/// humanoid on foot. When mounted it is drawn from its larger horse sheet, but its
/// collision box stays this on-foot size (as a ridden player keeps their own box).
pub const KNIGHT_SIZE: (f32, f32) = (10.0, 13.0);
/// Collision/draw size (width, height) in pixels of a thrown bone — a small
/// tumbling projectile.
pub const BONE_SIZE: (f32, f32) = (12.0, 12.0);
/// Collision/draw size (width, height) in pixels of a hurled fireball — a small,
/// low bolt of flame.
pub const FIREBALL_SIZE: (f32, f32) = (10.0, 7.0);
/// Collision/draw size (width, height) in pixels of a hurled magic fireball — a
/// bolt of purple flame an [`EntityKind::EnchantedDemon`] flings, the same low,
/// small bolt as the ordinary [`FIREBALL_SIZE`].
pub const MAGIC_FIREBALL_SIZE: (f32, f32) = (10.0, 7.0);
/// Collision/draw size (width, height) in pixels of a summoner fireball — the bolt
/// a [`EntityKind::Necromancer`] hurls, the same low, small bolt as the others.
pub const SUMMONER_FIREBALL_SIZE: (f32, f32) = (10.0, 7.0);
/// Collision/draw size (width, height) in pixels of a friendly summoner fireball —
/// the bolt a player's summoner spell looses, the same low, small bolt as the
/// necromancer's [`SUMMONER_FIREBALL_SIZE`].
pub const FRIENDLY_SUMMONER_FIREBALL_SIZE: (f32, f32) = (10.0, 7.0);
/// Collision/draw size (width, height) in pixels of a friendly skull — a player's
/// summoned helper, the same small bouncing skull as the necromancer's [`SKULL_SIZE`].
pub const FRIENDLY_SKULL_SIZE: (f32, f32) = (10.0, 11.0);
/// Collision/draw size (width, height) in pixels of a dark knight — a broad,
/// black-armoured humanoid, bulkier across the shoulders than the [`KNIGHT_SIZE`]
/// man-at-arms it preys on.
pub const DARK_KNIGHT_SIZE: (f32, f32) = (12.0, 13.0);
/// Collision/draw size (width, height) in pixels of a thrown axe — a small
/// tumbling projectile, like the [`BONE_SIZE`] bone but a touch smaller.
pub const AXE_SIZE: (f32, f32) = (8.0, 8.0);
/// Collision/draw size (width, height) in pixels of a dropped block item.
pub const ITEM_SIZE: (f32, f32) = (8.0, 8.0);

/// Seconds a zombie spends playing its death animation (when caught by daylight)
/// before it despawns. Shared by both sides so the server's despawn timing and
/// the client's animation playback agree.
pub const ZOMBIE_DEATH_TIME: f32 = 0.8;

/// Seconds a snake's wind-up lunge runs end to end: it coils through the
/// telegraphed wind-up and then springs forward. Shared by both sides so the
/// server's lunge timing and the client's attack-animation playback agree.
pub const SNAKE_LUNGE_TIME: f32 = 0.7;

/// Seconds an orc's slam attack runs end to end: it heaves its arms up through a
/// slow telegraph and then crashes them down. Shared by both sides so the server's
/// slam timing and the client's attack-animation playback agree. The blow only
/// lands partway through (see [`crate::server`]'s `ORC_SLAM_STRIKE_TIME`), on the
/// frame where the fists hit the ground — so an alert player can back out of reach
/// during the wind-up. Reuses the [`Entity::lunge`] timer the snake strike rides on.
pub const ORC_SLAM_TIME: f32 = 1.1;

/// Seconds an orc mage's enchant cast animation plays end to end: it raises its
/// staff through the gesture that empowers a nearby demon. Shared by both sides so
/// the server's cast timing and the client's cast-animation playback agree. Like
/// the orc slam it rides on the [`Entity::lunge`] timer; it is purely cosmetic —
/// the demon is enchanted server-side when the cast is kicked off.
pub const ORC_MAGE_CAST_TIME: f32 = 0.8;

/// Seconds the demon king's attack animation plays end to end, for every one of
/// its attacks (a fireball volley, a magic-fireball spread, a summoned bolt, or a
/// melee slam). Shared by both sides so the server's attack timing and the
/// client's attack-animation playback agree. The attack resolves partway through
/// (see [`crate::server`]'s `DEMON_KING_STRIKE_TIME`) — the boss winds up, then
/// looses its bolts or brings its fists down. Rides on the [`Entity::lunge`] timer
/// like the orc slam and orc-mage cast.
pub const DEMON_KING_ATTACK_TIME: f32 = 1.0;

/// Seconds a mage's spell-casting animation plays end to end: it raises its staff
/// through the gesture that looses a spell. Shared by both sides so the server's cast
/// timing and the client's cast-animation playback agree. Like the orc-mage cast it
/// rides on the [`Entity::lunge`] timer and is purely cosmetic — the spell resolves
/// server-side when the cast is kicked off.
pub const MAGE_CAST_TIME: f32 = 0.8;

/// Seconds a knight's attack swing animation plays. Like the snake lunge and orc
/// slam it rides on the [`Entity::lunge`] timer: the server kicks it off (broadcasting
/// [`crate::protocol::ServerMessage::EntityLunging`]) each time the knight lands a
/// blow, and the client plays the attack sheet for this long. Purely cosmetic — the
/// damage is dealt server-side on the [`Entity::attack_cd`] cadence.
pub const KNIGHT_ATTACK_TIME: f32 = 0.45;

/// Seconds a dragon's fireball-breathing animation plays end to end. Like the
/// other attack poses it rides on the [`Entity::lunge`] timer: the server kicks
/// it off (broadcasting [`crate::protocol::ServerMessage::EntityLunging`]) each
/// time the dragon looses a fireball, and the client plays the attack sheet for
/// this long. Purely cosmetic — the fireball is spawned server-side.
pub const DRAGON_ATTACK_TIME: f32 = 0.5;

/// Seconds a snake spends writhing through its death animation when killed,
/// before it despawns. Shared by both sides so the server's despawn timing and
/// the client's animation playback agree.
pub const SNAKE_DEATH_TIME: f32 = 0.6;

/// Maximum health of a player, in hit points.
pub const PLAYER_MAX_HEALTH: i32 = 20;
/// Maximum health of a slime, in hit points.
pub const SLIME_MAX_HEALTH: i32 = 10;
/// Maximum health of a chicken, in hit points.
pub const CHICKEN_MAX_HEALTH: i32 = 8;
/// Maximum health of a goat, in hit points. Sturdier than the surface critters.
pub const GOAT_MAX_HEALTH: i32 = 16;
/// Maximum health of a cat, in hit points. Frail — a tamed cat that dies simply
/// returns to its owner's respawn point rather than being gone for good.
pub const CAT_MAX_HEALTH: i32 = 8;
/// Maximum health of a puppy, in hit points. Hardier than the cat — it picks
/// fights with skeletons and chickens — but, like the cat, a tamed puppy that
/// dies simply returns to its owner's respawn point rather than being gone for good.
pub const PUPPY_MAX_HEALTH: i32 = 14;
/// Maximum health of a horse, in hit points. Sturdier than the small pets — but,
/// like the cat and puppy, a tamed horse that dies simply returns to its owner's
/// respawn point rather than being gone for good.
pub const HORSE_MAX_HEALTH: i32 = 30;
/// Maximum health of a zombie, in hit points. Far tougher than anything else
/// that walks the surface — it soaks up many hits before going down.
pub const ZOMBIE_MAX_HEALTH: i32 = 40;
/// Maximum health of a spider, in hit points. Frail — it relies on speed and
/// numbers rather than soaking up hits.
pub const SPIDER_MAX_HEALTH: i32 = 12;
/// Maximum health of a skeleton, in hit points. Sturdier than a spider but
/// frailer than a zombie — it survives by keeping its distance and pelting the
/// player with bones rather than soaking up blows.
pub const SKELETON_MAX_HEALTH: i32 = 24;
/// Maximum health of a snake, in hit points. Frail like the spider — it leans on
/// its telegraphed lunge rather than soaking up blows.
pub const SNAKE_MAX_HEALTH: i32 = 14;
/// Maximum health of a charred skeleton, in hit points. Sturdier than the surface
/// skeleton — a relentless underworld brawler that closes for melee and soaks up
/// blows on the way in.
pub const CHARRED_SKELETON_MAX_HEALTH: i32 = 36;
/// Maximum health of a demon, in hit points. Sturdier than the surface skeleton
/// but frailer than the charred skeleton it shares the underworld with — it
/// survives by keeping its distance and pelting the player with fireballs rather
/// than wading into melee.
pub const DEMON_MAX_HEALTH: i32 = 28;
/// Maximum health of an enchanted demon, in hit points. An [`EntityKind::OrcMage`]'s
/// glamour makes it sturdier than the ordinary demon it was — and the enchant heals
/// it to this new full as it is empowered.
pub const ENCHANTED_DEMON_MAX_HEALTH: i32 = 40;
/// Maximum health of an orc mage, in hit points. A robed support caster — frailer
/// than the brute orc, since it hangs back and empowers demons rather than wading in.
pub const ORC_MAGE_MAX_HEALTH: i32 = 30;
/// Maximum health of an orc, in hit points. The toughest thing in the underworld —
/// a slow brute that soaks up punishment and answers with a devastating slam.
pub const ORC_MAX_HEALTH: i32 = 50;
/// Maximum health of an ash twister, in hit points. A whirling column of ash —
/// frailer than the underworld's brawlers, since it threatens by flinging the
/// player skyward (for a punishing fall) rather than by soaking up blows.
pub const ASH_TWISTER_MAX_HEALTH: i32 = 18;
/// Maximum health of a necromancer, in hit points. A frail ranged caster — like the
/// skeleton it relies on keeping its distance and summoning skulls rather than on
/// soaking up blows.
pub const NECROMANCER_MAX_HEALTH: i32 = 26;
/// Maximum health of a skull, in hit points. Very frail — a bouncing summoned skull
/// pops after a hit or two.
pub const SKULL_MAX_HEALTH: i32 = 8;
/// Maximum health of a knight, in hit points. A sturdy man-at-arms — hardier than
/// any pet, so a recruited knight can trade blows with the monsters it hunts.
pub const KNIGHT_MAX_HEALTH: i32 = 40;
/// Maximum health of a mage, in hit points. A robed spellcaster conjured by the
/// restore spell. It is never harmed (nothing attacks it — see [`crate::server`]),
/// so this is mostly nominal, but it keeps a caster's frail figure on the books.
pub const MAGE_MAX_HEALTH: i32 = 30;
/// Maximum health of a dark knight, in hit points. The toughest thing that stalks
/// the overworld night — a shade harder to fell than even the [`KNIGHT_MAX_HEALTH`]
/// man-at-arms it hunts, fitting a rare foe that drops tungsten when it falls.
pub const DARK_KNIGHT_MAX_HEALTH: i32 = 44;
/// Maximum health of a dragon, in hit points. A miniboss: far tougher than the
/// rank-and-file underworld monsters, so felling one is a genuine fight — its
/// health drives the miniboss bar the client shows while a dragon is near.
pub const DRAGON_MAX_HEALTH: i32 = 200;
/// Maximum health of the demon king, in hit points. A boss: vastly tougher than
/// anything else in the world, so felling it is a real campaign rather than a
/// brief scrap. Its health drives the boss bar the client shows during the fight.
pub const DEMON_KING_MAX_HEALTH: i32 = 1000;

/// What an entity *is*. Adding a new creature/object means adding a variant
/// here plus (for server-simulated kinds) a branch in the server tick loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EntityKind {
    /// A player avatar driven by a connected client. Special: client-authoritative
    /// position, never touched by server AI. Carries the player's display name.
    Player { name: String },
    /// A small creature that wanders the surface. Server-simulated.
    Slime,
    /// A harmless bird that pecks around the surface and bolts away from a
    /// player that hits it. Server-simulated.
    Chicken,
    /// A sturdy mountain grazer that calmly roams the stone slopes. Native to
    /// the mountains biome. Server-simulated.
    Goat,
    /// A small, peaceable critter that spawns rarely in the forest. A wild cat
    /// (`owner` = `None`) just wanders; feeding it cooked meat tames it, stamping
    /// the feeding player's **name** into `owner`. The name (not a volatile entity
    /// id) is what's stored so the bond survives a server restart, where players
    /// are re-allocated fresh ids but keep their names — the cat resolves its
    /// owner's live id by name each tick it needs one. Players can never attack a
    /// cat. A tamed cat that dies (to anything but fall damage, which never
    /// touches a server creature) doesn't vanish — it reappears at its owner's
    /// respawn point — and it never despawns for distance, teleporting to its
    /// owner when they wander too far. `sitting` is toggled by the owner clicking
    /// their own cat: a sitting cat stays put where it was left — it stops wandering
    /// and stops follow-teleporting — until clicked again to stand back up.
    /// Server-simulated.
    Cat {
        owner: Option<String>,
        sitting: bool,
    },
    /// A small, loyal critter that spawns rarely in the forest like the cat, but
    /// unlike the placid cat it is a hunter: it chases down nearby skeletons and
    /// chickens, biting them, then trots over to any raw meat that drops and eats
    /// it. Taming, sitting, respawning and never-despawning all work exactly as for
    /// the [`EntityKind::Cat`] — a wild puppy (`owner` = `None`) just wanders and
    /// hunts; feeding it cooked meat tames it, stamping the feeder's **name** into
    /// `owner` (stored by name so the bond survives a server restart). Players can
    /// never attack a puppy. A tamed puppy that dies reappears at its owner's
    /// respawn point, never despawns for distance (teleporting to its owner when
    /// they wander too far), and `sitting` is toggled by the owner clicking it.
    /// Server-simulated.
    Puppy {
        owner: Option<String>,
        sitting: bool,
    },
    /// A slow, tough, hard-hitting undead that only roams at night and burns up
    /// in daylight (playing a death animation before despawning). Spawns in any
    /// biome after dark. Server-simulated.
    Zombie,
    /// A fast, fragile predator that scuttles after players and scales sheer
    /// walls to reach them. Lurks only in the forest's shade and in the caverns
    /// deep underground. Server-simulated.
    Spider,
    /// A desert ambusher that hunts on sight. Rather than biting on contact like
    /// the spider, it attacks in a telegraphed wind-up **lunge**: it coils in
    /// place (playing its strike animation) before springing forward to bite, so
    /// an alert player can dodge the strike. Server-simulated.
    Snake,
    /// A stack of items lying on the ground (mined, spilled by crafting, or
    /// discarded/gifted by a player), waiting to be walked into and picked up.
    /// Server-simulated (falls under gravity); carries the block id, how many are
    /// in the stack, and a tool's remaining `durability` (`0` for items that have
    /// none) so worn tools keep their wear when dropped and picked back up.
    DroppedItem {
        block: BlockId,
        count: u32,
        durability: u16,
    },
    /// A lanky undead archer that roams at night like the zombie but keeps its
    /// distance, lobbing [`EntityKind::Bone`] projectiles at players instead of
    /// closing for melee. Burns up at daybreak (despawning outright — it has no
    /// death animation). Server-simulated.
    Skeleton,
    /// A bone thrown by a [`EntityKind::Skeleton`], flying in a straight line
    /// until it strikes a player or a wall (or its short life runs out). Its
    /// [`Entity::vx`]/[`Entity::vy`] carry its flight velocity. Server-simulated.
    Bone,
    /// A charred skeleton: the underworld's signature undead. Unlike the surface
    /// skeleton it doesn't throw bones — it charges into melee, hitting harder than
    /// a zombie, and lays down a trail of [`crate::block::FIRE`] behind it while it
    /// is closing on a target. Native to the underworld's **ash valleys**, which it
    /// roams at all hours. Server-simulated.
    CharredSkeleton,
    /// A tall, peaceable grazer that wanders the plains and — unlike the other
    /// pets, which are tamed with cooked meat — is tamed by feeding it an
    /// **apple**, stamping the feeding player's **name** into `owner` (stored by
    /// name, not a volatile id, so the bond survives a server restart). Once tamed
    /// it is a [pet](`EntityKind::is_pet`): players can never attack it, it never
    /// despawns for distance (teleporting to its owner when they stray too far),
    /// and a horse that dies reappears at its owner's respawn point. Its party
    /// trick is that it can be **ridden**: right-click your tamed horse to mount
    /// and gallop faster than you can run, and right-click again to dismount. While
    /// mounted the rider drives the horse (which is glued under them and drawn as
    /// the combined `player/horse` sprite, like a boat carries its rider). Native
    /// to the plains. Server-simulated. Appended last so older saves and the wire
    /// format keep their variant indices.
    Horse { owner: Option<String> },
    /// A demon: a winged underworld fiend that roams the charred depths like the
    /// [`EntityKind::CharredSkeleton`] but, rather than charging into melee, keeps
    /// its distance and hurls [`EntityKind::Fireball`] projectiles at players. It
    /// spawns in the underworld at all hours — but more rarely than the charred
    /// skeleton. Server-simulated. Appended last so older saves and the wire format
    /// keep their variant indices.
    Demon,
    /// A bolt of flame hurled by a [`EntityKind::Demon`], flying in a straight line
    /// until it strikes a player or a wall (or its short life runs out), leaving a
    /// lick of [`crate::block::FIRE`] where it bursts. Its [`Entity::vx`]/
    /// [`Entity::vy`] carry its flight velocity. Server-simulated. Appended last so
    /// older saves and the wire format keep their variant indices.
    Fireball,
    /// An orc: a hulking underworld brute. It lumbers slowly after players — slower
    /// than even a zombie — but rather than a quick bite it commits to a telegraphed
    /// **slam**, heaving its arms up and crashing them down for heavy damage. Like
    /// the snake's lunge the slam is dodgeable: the blow only lands on the frame the
    /// fists hit the ground, so a player who backs away during the wind-up escapes
    /// it. Roams the underworld at all hours. Server-simulated. Appended last so
    /// older saves and the wire format keep their variant indices.
    Orc,
    /// A knight: a wandering man-at-arms that spawns rarely on the **plains**. A wild
    /// knight (`owner` = `None`) just roams and **cannot be attacked** by players;
    /// giving it a **tungsten ingot** recruits it, stamping the giver's **name** into
    /// `owner` (stored by name, not a volatile id, so the bond survives a restart). A
    /// recruited knight follows its owner everywhere — it even crosses dimensions with
    /// them — teleporting over when they stray too far (like a pet), and charges into
    /// battle against whatever enemy its owner last struck. If a *wild* (untamed) horse
    /// is nearby it will mount up, riding into the fray; the horse soaks blows on the
    /// knight's behalf until it is slain, after which the knight fights on foot. Unlike
    /// a pet it does **not** respawn loyal: a slain knight reappears at its owner's
    /// respawn point as a *wild* knight that must be recruited afresh. Server-simulated.
    /// Appended last so older saves and the wire format keep their variant indices.
    Knight { owner: Option<String> },
    /// An ash twister: a whirling column of ash that forms in the underworld's
    /// **ash valleys**. It drifts toward players and, on contact, flings them high
    /// into the air — the punishing fall back to the ground does the real damage,
    /// not the buffeting itself. Roams the ash valleys at all hours. Server-simulated.
    /// Appended last so older saves and the wire format keep their variant indices.
    AshTwister,
    /// An orc mage: a robed underworld spellcaster. Unlike the brute
    /// [`EntityKind::Orc`] it lands no blows of its own — it is a **support**
    /// creature that hangs back, shying away from players, and instead empowers
    /// nearby ordinary [`EntityKind::Demon`]s, turning them into
    /// [`EntityKind::EnchantedDemon`]s. It spawns in the underworld's charred
    /// expanse, more rarely than the demons it shepherds, at all hours.
    /// Server-simulated. Appended last so older saves and the wire format keep their
    /// variant indices.
    OrcMage,
    /// An enchanted demon: a [`EntityKind::Demon`] an [`EntityKind::OrcMage`] has
    /// empowered. Where an ordinary demon kites along the ground, the enchanted one
    /// **flies**, chasing the player through the air, and hurls
    /// [`EntityKind::MagicFireball`]s that fly farther and hit harder than ordinary
    /// fireballs. It never spawns on its own — only a mage's enchant (live, or one a
    /// freshly spawned mage already worked offscreen) creates one. Server-simulated.
    /// Appended last so older saves and the wire format keep their variant indices.
    EnchantedDemon,
    /// A bolt of purple (magic) flame hurled by an [`EntityKind::EnchantedDemon`],
    /// flying in a straight line until it strikes a player or a wall (or its life
    /// runs out). It flies farther and deals more damage than an ordinary
    /// [`EntityKind::Fireball`], but — like the ordinary one — leaves only an
    /// ordinary lick of [`crate::block::FIRE`] where it bursts. Its [`Entity::vx`]/
    /// [`Entity::vy`] carry its flight velocity. Server-simulated. Appended last so
    /// older saves and the wire format keep their variant indices.
    MagicFireball,
    /// A necromancer: a hooded ranged caster. Like the [`EntityKind::Skeleton`] it
    /// keeps its distance and kites the player, but instead of bones it hurls
    /// [`EntityKind::SummonerFireball`]s that burst into bouncing [`EntityKind::Skull`]s.
    /// It haunts the underworld's **ash valleys** and the overworld's **deserts**; in
    /// the overworld it **burns up at daybreak** (the underworld is always dark, so it
    /// roams there around the clock). Server-simulated. Appended last so older saves
    /// and the wire format keep their variant indices.
    Necromancer,
    /// A skull: a bouncing skeleton skull a [`EntityKind::Necromancer`] summons — it
    /// never spawns on its own. It caroms around under gravity, bounding off floors and
    /// walls and hopping toward nearby players to gnash at them, and gives out after a
    /// short life. Like the necromancer it **burns up at daybreak** in the overworld
    /// (but roams the always-dark underworld freely). Server-simulated. Appended last
    /// so older saves and the wire format keep their variant indices.
    Skull,
    /// A bolt hurled by a [`EntityKind::Necromancer`], flying in a straight line until
    /// it strikes a player or a wall (or its short life runs out) — where it bursts it
    /// summons a bouncing [`EntityKind::Skull`] rather than leaving fire. Its
    /// [`Entity::vx`]/[`Entity::vy`] carry its flight velocity. Server-simulated.
    /// Appended last so older saves and the wire format keep their variant indices.
    SummonerFireball,
    /// A dark knight: a black-armoured warrior that stalks the **overworld** night in
    /// any biome, rare and dangerous. Like the [`EntityKind::Skeleton`] it is a ranged
    /// kiter — it keeps its distance and hurls [`EntityKind::Axe`] projectiles rather
    /// than closing for melee — but it is hardier and hits harder, and it makes war on
    /// the [`EntityKind::Knight`] as readily as on players (throwing axes at both). It
    /// **burns up at daybreak** like the other overworld night undead, and a slain one
    /// spills **tungsten** gear — the only way to win tungsten without braving the
    /// underworld. Server-simulated. Appended last so older saves and the wire format
    /// keep their variant indices.
    DarkKnight,
    /// An axe hurled by a [`EntityKind::DarkKnight`], flying in a straight line until
    /// it strikes a player or knight or a wall (or its short life runs out), tumbling
    /// end over end as it flies. Its [`Entity::vx`]/[`Entity::vy`] carry its flight
    /// velocity. Server-simulated. Appended last so older saves and the wire format
    /// keep their variant indices.
    Axe,
    /// The demon king: the boss of the [`crate::world::Dimension::Arena`], and the
    /// only one of its kind in a world. A towering winged fiend that fights **on foot**
    /// the whole bout, striding after the player, and wields the whole demonic arsenal:
    /// it looses a fan of five ordinary [`EntityKind::Fireball`]s, a tighter spread of
    /// three [`EntityKind::MagicFireball`]s, a single [`EntityKind::SummonerFireball`]
    /// (which bursts into a bouncing [`EntityKind::Skull`]), or — at close range —
    /// brings its fists down in a heavy melee **slam** (like the [`EntityKind::Orc`]).
    /// It picks among these at random as it attacks, and past two-thirds health it
    /// **enrages**, summoning a host of dark knights. Slaying it drops a **chest** of
    /// loot where it falls rather than loose items, and no new king is ever raised in
    /// that world (see [`crate::server`]). Server-simulated. Appended last so older
    /// saves and the wire format keep their variant indices.
    DemonKing,
    /// A dragon: the underworld's rare flying miniboss. It spawns extremely rarely
    /// in the charred expanse, high in an open cavern where it is plainly visible,
    /// and — like the [`EntityKind::EnchantedDemon`] — it **flies**, chasing the
    /// player through the air and hurling [`EntityKind::Fireball`]s from range. It
    /// is far tougher than anything else in the depths, and a nearby dragon raises
    /// its own miniboss music and health bar on the client. Server-simulated.
    /// Appended last so older saves and the wire format keep their variant indices.
    Dragon,
    /// A friendly summoner fireball: the bolt a player's **summoner spell** looses
    /// (see [`crate::block::SUMMONER_SPELL`]). It flies in a straight line like the
    /// necromancer's [`EntityKind::SummonerFireball`], but where it bursts — on a
    /// wall, on a monster, or when its short life runs out — it summons a *friendly*
    /// [`EntityKind::FriendlySkull`] rather than a hostile one. Its [`Entity::vx`]/
    /// [`Entity::vy`] carry its flight velocity. Never hostile, so knights and
    /// monsters ignore it. Server-simulated. Appended last so older saves and the
    /// wire format keep their variant indices.
    FriendlySummonerFireball,
    /// A friendly skull: a bouncing skull a player's summoner spell conjured. It
    /// caroms around under gravity exactly like the necromancer's
    /// [`EntityKind::Skull`], but it **helps the caster** — it hunts and gnashes at
    /// nearby *monsters* instead of players, and gives out after a short life. It is
    /// never hostile, so [`EntityKind::Knight`]s won't attack it (nor it them), and
    /// monsters pay it no mind. Server-simulated. Appended last so older saves and
    /// the wire format keep their variant indices.
    FriendlySkull,
    /// A mage: a robed spellcaster that **only** ever comes into being through the
    /// [`crate::block::RESTORE_SPELL`] — cast on an [`EntityKind::OrcMage`], it
    /// restores the brute caster to this gentler one. A mage casts the world's
    /// spells of its own accord — the summoner spell, the sunburst spell, and even
    /// the restore spell itself — to aid whoever it serves. Like a
    /// [`EntityKind::Knight`] it can be **recruited** (`owner` = the caster's name,
    /// stored by name so the bond survives a restart) or **wild** (`owner` = `None`);
    /// a recruited one follows its owner and, when *it* casts restore, recruits the
    /// result on its owner's behalf. Nothing attacks a mage (it is never hostile, and
    /// players cannot strike it). Server-simulated. Appended last so older saves and
    /// the wire format keep their variant indices.
    Mage { owner: Option<String> },
}

impl EntityKind {
    /// Draw/collision size (width, height) in pixels for this kind.
    pub fn size(&self) -> (f32, f32) {
        match self {
            EntityKind::Player { .. } => PLAYER_SIZE,
            EntityKind::Slime => SLIME_SIZE,
            EntityKind::Chicken => CHICKEN_SIZE,
            EntityKind::Goat => GOAT_SIZE,
            EntityKind::Cat { .. } => CAT_SIZE,
            EntityKind::Puppy { .. } => PUPPY_SIZE,
            EntityKind::Zombie => ZOMBIE_SIZE,
            EntityKind::Spider => SPIDER_SIZE,
            EntityKind::Snake => SNAKE_SIZE,
            EntityKind::Skeleton => SKELETON_SIZE,
            EntityKind::CharredSkeleton => CHARRED_SKELETON_SIZE,
            EntityKind::Demon => DEMON_SIZE,
            EntityKind::Horse { .. } => HORSE_SIZE,
            EntityKind::Bone => BONE_SIZE,
            EntityKind::Fireball => FIREBALL_SIZE,
            EntityKind::Orc => ORC_SIZE,
            EntityKind::Knight { .. } => KNIGHT_SIZE,
            EntityKind::AshTwister => ASH_TWISTER_SIZE,
            EntityKind::OrcMage => ORC_MAGE_SIZE,
            EntityKind::EnchantedDemon => ENCHANTED_DEMON_SIZE,
            EntityKind::MagicFireball => MAGIC_FIREBALL_SIZE,
            EntityKind::Necromancer => NECROMANCER_SIZE,
            EntityKind::Skull => SKULL_SIZE,
            EntityKind::SummonerFireball => SUMMONER_FIREBALL_SIZE,
            EntityKind::DarkKnight => DARK_KNIGHT_SIZE,
            EntityKind::Axe => AXE_SIZE,
            EntityKind::DemonKing => DEMON_KING_SIZE,
            EntityKind::Dragon => DRAGON_SIZE,
            EntityKind::FriendlySummonerFireball => FRIENDLY_SUMMONER_FIREBALL_SIZE,
            EntityKind::FriendlySkull => FRIENDLY_SKULL_SIZE,
            EntityKind::Mage { .. } => MAGE_SIZE,
            EntityKind::DroppedItem { .. } => ITEM_SIZE,
        }
    }

    /// Whether this is a player avatar (the "special" entity the owning client
    /// simulates itself).
    pub fn is_player(&self) -> bool {
        matches!(self, EntityKind::Player { .. })
    }

    /// Whether this is a dropped block item lying on the ground.
    pub fn is_item(&self) -> bool {
        matches!(self, EntityKind::DroppedItem { .. })
    }

    /// Whether this is a cat (tamed or wild). Cats are special: immune to player
    /// attacks, exempt from distance despawn, and (when tamed) respawn at their
    /// owner's respawn point instead of being removed on death.
    pub fn is_cat(&self) -> bool {
        matches!(self, EntityKind::Cat { .. })
    }

    /// Whether this is a puppy (tamed or wild).
    pub fn is_puppy(&self) -> bool {
        matches!(self, EntityKind::Puppy { .. })
    }

    /// Whether this is a horse (tamed or wild). Horses are tamed with apples and,
    /// once tamed, can be ridden.
    pub fn is_horse(&self) -> bool {
        matches!(self, EntityKind::Horse { .. })
    }

    /// Whether this is a knight (recruited or wild). A knight is not a [pet](Self::is_pet)
    /// — it can't be sat and doesn't respawn loyal — but it shares some companion rules
    /// (immune to player attacks, exempt from distance despawn, follows its owner).
    pub fn is_knight(&self) -> bool {
        matches!(self, EntityKind::Knight { .. })
    }

    /// Whether this is a mage (recruited or wild) — the spellcaster the restore
    /// spell conjures. Like a knight it is not a [pet](Self::is_pet), but it is a
    /// companion: immune to harm, exempt from distance despawn, and (when recruited)
    /// following its owner.
    pub fn is_mage(&self) -> bool {
        matches!(self, EntityKind::Mage { .. })
    }

    /// Whether this is a tameable companion (a cat, a puppy, or a horse). Pets share
    /// a bundle of special rules: immune to player attacks, exempt from distance
    /// despawn, singed by fire (their one mortal hazard), and — once tamed —
    /// respawning at their owner's respawn point and teleporting to a far-strayed
    /// owner.
    pub fn is_pet(&self) -> bool {
        matches!(
            self,
            EntityKind::Cat { .. } | EntityKind::Puppy { .. } | EntityKind::Horse { .. }
        )
    }

    /// Whether this is a pet that has been told to sit (by its owner clicking it).
    /// A sitting pet holds its position rather than wandering, hunting or following.
    pub fn is_sitting(&self) -> bool {
        matches!(
            self,
            EntityKind::Cat { sitting: true, .. } | EntityKind::Puppy { sitting: true, .. }
        )
    }

    /// The name of the player who has tamed this entity, if any. Only a tamed pet (a
    /// [`EntityKind::Cat`] or [`EntityKind::Puppy`]) has an owner; everything else
    /// returns `None`. Callers resolve this name to a live entity id on demand (see
    /// [`crate::server`]'s `find_player_by_name`).
    pub fn owner(&self) -> Option<&str> {
        match self {
            EntityKind::Cat { owner, .. }
            | EntityKind::Puppy { owner, .. }
            | EntityKind::Horse { owner }
            | EntityKind::Knight { owner }
            | EntityKind::Mage { owner } => owner.as_deref(),
            _ => None,
        }
    }

    /// Full health for this kind of entity. Players cap at
    /// [`PLAYER_MAX_HEALTH`]; other creatures have their own (see the
    /// constants above).
    pub fn max_health(&self) -> i32 {
        match self {
            EntityKind::Player { .. } => PLAYER_MAX_HEALTH,
            EntityKind::Slime => SLIME_MAX_HEALTH,
            EntityKind::Chicken => CHICKEN_MAX_HEALTH,
            EntityKind::Goat => GOAT_MAX_HEALTH,
            EntityKind::Cat { .. } => CAT_MAX_HEALTH,
            EntityKind::Puppy { .. } => PUPPY_MAX_HEALTH,
            EntityKind::Zombie => ZOMBIE_MAX_HEALTH,
            EntityKind::Spider => SPIDER_MAX_HEALTH,
            EntityKind::Snake => SNAKE_MAX_HEALTH,
            EntityKind::Skeleton => SKELETON_MAX_HEALTH,
            EntityKind::CharredSkeleton => CHARRED_SKELETON_MAX_HEALTH,
            EntityKind::Demon => DEMON_MAX_HEALTH,
            EntityKind::Horse { .. } => HORSE_MAX_HEALTH,
            // A bone is an inert projectile; 1 keeps health == max_health so no
            // health bar shows and a stray melee swing can't meaningfully "kill" it.
            EntityKind::Bone => 1,
            // A fireball is an inert projectile; 1 keeps health == max_health so no
            // health bar shows and a stray melee swing can't meaningfully "kill" it.
            EntityKind::Fireball => 1,
            EntityKind::Orc => ORC_MAX_HEALTH,
            EntityKind::Knight { .. } => KNIGHT_MAX_HEALTH,
            EntityKind::AshTwister => ASH_TWISTER_MAX_HEALTH,
            EntityKind::OrcMage => ORC_MAGE_MAX_HEALTH,
            EntityKind::EnchantedDemon => ENCHANTED_DEMON_MAX_HEALTH,
            // A magic fireball is an inert projectile; 1 keeps health == max_health so
            // no health bar shows and a stray melee swing can't meaningfully "kill" it.
            EntityKind::MagicFireball => 1,
            EntityKind::Necromancer => NECROMANCER_MAX_HEALTH,
            EntityKind::Skull => SKULL_MAX_HEALTH,
            // A summoner fireball is an inert projectile; 1 keeps health == max_health so
            // no health bar shows and a stray melee swing can't meaningfully "kill" it.
            EntityKind::SummonerFireball => 1,
            EntityKind::DarkKnight => DARK_KNIGHT_MAX_HEALTH,
            // An axe is an inert projectile; 1 keeps health == max_health so no health
            // bar shows and a stray melee swing can't meaningfully "kill" it.
            EntityKind::Axe => 1,
            EntityKind::DemonKing => DEMON_KING_MAX_HEALTH,
            EntityKind::Dragon => DRAGON_MAX_HEALTH,
            // A friendly summoner fireball is an inert projectile; 1 keeps health ==
            // max_health so no health bar shows and a stray swing can't "kill" it.
            EntityKind::FriendlySummonerFireball => 1,
            // A friendly skull is as frail as the necromancer's; nothing attacks it,
            // so its health is moot — it simply gives out when its short life ends.
            EntityKind::FriendlySkull => SKULL_MAX_HEALTH,
            EntityKind::Mage { .. } => MAGE_MAX_HEALTH,
            // Items are inert; 1 keeps health == max_health so no health bar shows.
            EntityKind::DroppedItem { .. } => 1,
        }
    }

    /// How long this kind's death animation plays before it despawns, or `None`
    /// if it simply vanishes when it dies. Drives both the server's despawn delay
    /// and the client's death-animation playback (see
    /// [`crate::protocol::ServerMessage::EntityDying`]). A zombie crumbles in
    /// daylight; a snake writhes when killed.
    pub fn death_time(&self) -> Option<f32> {
        match self {
            EntityKind::Zombie => Some(ZOMBIE_DEATH_TIME),
            EntityKind::Snake => Some(SNAKE_DEATH_TIME),
            _ => None,
        }
    }
}

/// A live entity: its identity, kind, and current motion state. Position is the
/// top-left corner in world pixels, matching how the player and tiles are drawn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub kind: EntityKind,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    /// Current health in hit points. Starts at [`EntityKind::max_health`].
    pub health: i32,
    /// Full health for this entity, mirrored from its kind for convenience on
    /// the client (so health bars know their denominator without a registry).
    pub max_health: i32,
    /// Server-only: seconds until this creature can attack again. Never sent
    /// over the wire (defaults to `0.0` on the client).
    #[serde(skip)]
    pub attack_cd: f32,
    /// Server-only: seconds a skittish creature (e.g. a [`EntityKind::Chicken`])
    /// keeps fleeing after being hit. Counts down each tick; while positive the
    /// creature runs from the nearest player. Never sent over the wire.
    #[serde(skip)]
    pub flee: f32,
    /// Server-only: the x (world px) a wandering creature treats as the center of
    /// its home range, so it loiters nearby instead of drifting off forever. Set
    /// lazily to wherever the creature first simulates (`None` until then), so it
    /// survives a reload without needing to be persisted.
    #[serde(skip)]
    pub home_x: Option<f32>,
    /// Client-only: seconds left on the red "just got hit" flash. Set when an
    /// [`crate::protocol::ServerMessage::EntityHit`] arrives and counted down each
    /// frame while the entity tints red. Never sent over the wire (defaults to
    /// `0.0` on both sides).
    #[serde(skip)]
    pub hit_flash: f32,
    /// Seconds left in a zombie's death animation while it crumbles in daylight.
    /// Set to [`ZOMBIE_DEATH_TIME`] when dying begins and counted down on both
    /// sides: the server suppresses the zombie's AI and despawns it at zero,
    /// while the client picks the death-animation frame from the remaining time.
    /// Never sent over the wire (defaults to `0.0`).
    #[serde(skip)]
    pub dying: f32,
    /// Seconds left in a wind-up melee attack — a snake's lunge strike, or an orc's
    /// slam. Set to the kind's attack duration ([`SNAKE_LUNGE_TIME`] /
    /// [`ORC_SLAM_TIME`]) when one begins and counted down on both sides: the server
    /// drives the attack (the snake coils then springs; the orc heaves then crashes
    /// down), while the client picks the attack-animation frame from the remaining
    /// time. Never sent over the wire (defaults to `0.0`).
    #[serde(skip)]
    pub lunge: f32,
    /// Server-only: the horizontal heading (`-1.0`/`1.0`) a lunging snake locked
    /// in when its wind-up began, so the strike springs the way the player *was*
    /// even if they sidestep it. Never sent over the wire.
    #[serde(skip)]
    pub lunge_dir: f32,
    /// Whether a player is riding a boat. Live runtime state, never persisted (a
    /// reloaded world shouldn't remember a mid-ride pose) and never piggybacked on
    /// the serialized entity, so it stays out of the save format like the transient
    /// fields above. It is instead synced to clients by a dedicated
    /// [`crate::protocol::ServerMessage::EntityBoating`] message (sent on toggle and
    /// alongside the entity snapshot a joining client receives). Only players set
    /// it; it stays `false` for creatures and defaults `false` on the client.
    #[serde(skip)]
    pub boating: bool,
    /// Which [`EntityKind::Horse`] this player is currently riding, if any. Like
    /// [`Self::boating`] it is live runtime state: never persisted (a reloaded
    /// world shouldn't remember a mid-ride pose) and never piggybacked on the
    /// serialized entity, so it stays out of the save format. It is synced to
    /// clients by a dedicated [`crate::protocol::ServerMessage::EntityRiding`]
    /// message (sent on mount/dismount and alongside the entity snapshot a joining
    /// client receives). Only players set it; it stays `None` for creatures and
    /// defaults `None` on the client. The server uses it each tick to glue the
    /// ridden horse beneath its rider; clients use it to draw the rider on the
    /// combined `player/horse` sprite and to hide the now-mounted horse entity.
    ///
    /// A [`EntityKind::Knight`] also sets this — to the id of the wild horse it has
    /// mounted — so clients draw it on the combined `knight/horse` sprite. (A knight
    /// *absorbs* the horse it mounts rather than gluing a live one beneath it; the
    /// id here is just a non-`None` "is mounted" marker and the absorbed horse's
    /// [`Self::mount_health`] is the shield it rides behind.)
    #[serde(skip)]
    pub riding: Option<EntityId>,
    /// Server-only: a mounted [`EntityKind::Knight`]'s remaining mount (horse) hit
    /// points — the shield it rides behind. `> 0` means the knight is mounted; blows
    /// it would take are subtracted from this until it hits `0`, at which point the
    /// knight is thrown and fights on foot. `0` for everything else. Never sent over
    /// the wire (the mount pose itself rides on [`Self::riding`], synced via
    /// [`crate::protocol::ServerMessage::EntityRiding`]).
    #[serde(skip)]
    pub mount_health: i32,
}

impl Entity {
    /// Create an entity at rest and at full health at `(x, y)`.
    pub fn new(id: EntityId, kind: EntityKind, x: f32, y: f32) -> Self {
        let max_health = kind.max_health();
        Entity {
            id,
            kind,
            x,
            y,
            vx: 0.0,
            vy: 0.0,
            health: max_health,
            max_health,
            attack_cd: 0.0,
            flee: 0.0,
            home_x: None,
            hit_flash: 0.0,
            dying: 0.0,
            lunge: 0.0,
            lunge_dir: 0.0,
            boating: false,
            riding: None,
            mount_health: 0,
        }
    }

    /// Draw/collision size (width, height) in pixels.
    pub fn size(&self) -> (f32, f32) {
        self.kind.size()
    }
}

/// A live collection of entities keyed by id. Used by the server (the
/// authority) and mirrored on each client for everything *except* its own
/// player avatar, which the client simulates locally.
#[derive(Default)]
pub struct Entities {
    map: HashMap<EntityId, Entity>,
}

impl Entities {
    pub fn new() -> Self {
        Entities {
            map: HashMap::new(),
        }
    }

    pub fn insert(&mut self, entity: Entity) {
        self.map.insert(entity.id, entity);
    }

    pub fn remove(&mut self, id: EntityId) -> Option<Entity> {
        self.map.remove(&id)
    }

    pub fn get(&self, id: EntityId) -> Option<&Entity> {
        self.map.get(&id)
    }

    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Entity> {
        self.map.get_mut(&id)
    }

    pub fn values(&self) -> impl Iterator<Item = &Entity> {
        self.map.values()
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Entity> {
        self.map.values_mut()
    }

    /// Number of player entities currently present.
    pub fn player_count(&self) -> usize {
        self.map.values().filter(|e| e.kind.is_player()).count()
    }
}
