# Creatures

The world is populated by animals you can farm or tame and monsters that hunt you
at night. Creatures spawn in a biome-appropriate mix, stay near a home area, and
are all simulated by the server (only your own player is driven by your client).

## Passive animals

| Animal | HP | Where | Drops | Notes |
|---|---|---|---|---|
| **Chicken** | 8 | Plains, forest, desert | 1 Raw Meat | Pecks the ground, bolts when struck |
| **Goat** | 16 | Mountains | 2 Raw Meat | Calm grazer |
| **Cat** | 8 | Forest | — | Wild until tamed |
| **Puppy** | 14 | Forest | — | Wild until tamed; hunts skeletons and chickens |
| **Horse** | 30 | Plains | — | Wild until tamed **with an apple**; can be ridden |

Cook the raw meat from chickens and goats before eating it — raw meat makes you
sick. See [Food](crafting.md#food).

## Pets

Wild **cats** and **puppies** can be **tamed by feeding them cooked meat**. Once
tamed:

- They belong to you — you can't accidentally attack your own pet.
- They follow you, and a puppy will hunt nearby skeletons and chickens.
- **Click your pet to toggle sitting.** A sitting pet stays put and won't wander,
  hunt, or follow.
- They never wander off for good — if they get too far they teleport back to you.
- If killed, a tamed pet **respawns at your respawn point**.
- The only thing that can kill a tamed pet for good is **fire**, so keep them
  away from the underworld's flames.

## Mounts

The **horse** is a tall, peaceable grazer that roams the **plains**. It's a pet
like the cat and puppy — immune to your attacks, never lost (it teleports to you
if you stray too far), and respawning at your respawn point if it ever dies — with
two differences:

- **Tame it with an apple**, not cooked meat. Hold an [apple](crafting.md#food)
  and left-click a wild horse to befriend it. (Apples are shed now and then by
  leaves.)
- Once tamed, you can **ride it**. Walk up to your horse and **right-click** to
  climb on; **right-click again** to hop off. While mounted you gallop
  noticeably faster than you can run, and you still jump and fall normally — handy
  for crossing the open plains in a hurry.

A horse can't be ridden between dimensions: stepping through a fire key (or
falling to the underworld) drops you back on foot and leaves the horse behind in
the world you left.

## Hostile monsters

Most monsters only **hunt at night** (or in dark caves). Build shelter and keep a
[weapon](crafting.md#weapons) handy before dusk.

| Monster | HP | Where | Behaviour |
|---|---|---|---|
| **Slime** | 10 | Common in mountains | Wanders by day, hunts at night |
| **Zombie** | 40 | All biomes, night | Slow, tough, hard-hitting; **burns up at daybreak** |
| **Spider** | 12 | Forests & caves | Fast and fragile; climbs walls |
| **Snake** | 14 | Desert | Ambusher; coils for ~0.7 s, then lunges |
| **Skeleton** | 24 | All biomes, night | Keeps its distance and throws **bone projectiles**; burns at daybreak |
| **Charred Skeleton** | 36 | Underworld | Charges into melee and **leaves a trail of fire**; active at all hours |
| **Demon** | 28 | Underworld (rare) | Keeps its distance and hurls **fireballs** that burst into flame; active at all hours |
| **Orc** | 50 | Underworld | Slow, hulking brute; plants its feet for a telegraphed **slam** that hits brutally hard; active at all hours |

!!! danger "Skeletons and demons shoot back"
    Skeletons fire bones and demons hurl fireballs in a straight line. The
    projectile flies until it hits you or a wall — strafe behind cover, or close
    the distance fast. A demon's fireball leaves a lick of fire where it bursts,
    so don't linger where it lands. Demons are rarer than charred skeletons, but
    far deadlier at range.

!!! danger "Don't stand under an orc's slam"
    An orc is slow and easy to outrun, but once it plants its feet and rears back
    it is winding up a **slam** that can gut you in two blows. The hit only lands
    as its fists crash down, so step out of reach during the wind-up — then close
    in and strike while it recovers.

In **creator mode** you are invisible to monsters and take no damage from them.

## Combat tips

- Left-click within ~80 pixels to attack; each hit knocks the target back and
  flashes it red.
- Better [weapons](crafting.md#weapons) deal far more damage — a tungsten sword
  drops a zombie in a fraction of the hits a wooden one needs.
- Zombies and skeletons self-destruct at dawn, so sometimes the smart move is
  simply to survive the night behind a wall.
- A tamed puppy is a genuine ally against skeletons.

## Other entities

- **Bone** — the projectile a skeleton throws; it disappears on impact or after a
  short flight.
- **Fireball** — the projectile a demon hurls; it disappears on impact or after a
  short flight, leaving a tongue of fire where it bursts.
- **Dropped items** — block and tool stacks lying on the ground (from mining,
  drops, or the <kbd>Q</kbd> key). Walk into them to pick them up; dropped tools
  keep their durability.
