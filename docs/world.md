# The World

Worlds are **infinite** in width and **256 cells** tall, generated from a single
numeric **seed** — the same seed always produces the same terrain. The world is
split into **16×16-cell chunks**; only chunks you modify are saved, and untouched
terrain regenerates identically from the seed.

There are three dimensions: the **overworld** on the surface, the **underworld**
below (linked by the [fire key](crafting.md#tools-and-utility-items)), and the
**arena** — a self-contained boss plane reached with the
[arena key](crafting.md#tools-and-utility-items).

## Biomes (overworld)

| Biome | Surface | Features |
|---|---|---|
| **Plains** | Grass | Gently rolling hills, sparse trees (~3% of columns); the dominant biome — broad, sweeping grasslands |
| **Forest** | Grass | Dense, near-continuous tree canopy (~34% of columns) |
| **Mountains** | Bare stone | Rugged peaks rising well above the plains; the richest iron — now an uncommon range cresting out of the grassland |
| **Desert** | Sand | Flat, treeless dunes — an uncommon feature, not a staple |
| **Ocean** | Sand seabed | Broad, deep seas — open water over a sandy floor; treeless and (for now) lifeless |

Plains cover roughly 40% of the world, with forest and ocean the next most
common; **mountains and deserts are deliberately rare** (a few percent each), so
they read as occasional landmarks punctuating the open plains rather than the
norm.

Trees are 4–6 logs tall with a rounded leaf crown. They don't grow on deserts,
mountain stone, oceans, flooded columns, or over cave mouths.

## Terrain & water

Low-lying basins flood with **water** from sea level down, forming ponds and
lakes. **Oceans** go much further — a dedicated biome where the seabed drops far
below sea level, so the whole column fills with deep open water over a sandy
floor. The water is too deep and slow to swim across comfortably; build a
**[boat](crafting.md)** to cross one (see [Boats](gameplay.md#boats)). Water
placed by the world (or by you with a bucket) is inert — it doesn't spread. The
very bottom of the world is solid bedrock, so there's no void to fall into.

The world spawn is always chosen on dry land, so you never start adrift in a sea.

## Caves

Underground you'll find:

- **Winding tunnels** that open as cave mouths in hillsides (carved on slopes, not
  on flat ground).
- **Large caverns** — open rooms found deep below the surface.

Rope ladders make descending these safe; bring a light source, because caves are
dark enough for hostile creatures even by day.

## Ores

| Ore | Where |
|---|---|
| **Coal** | Shallow (a few blocks down), fairly common |
| **Iron** | Deeper (6+ blocks down), rarer, and richest inside mountains |
| **Tungsten** | Ore is underworld-only, deep in charred rock (but **[dark knights](entities/dark-knight.md)** drop tungsten gear in the overworld) |

See [Blocks](blocks.md#ores) for the tool tier each ore requires.

## Structures

Occasionally (about 0.4% of lowland columns) the world generates a pre-built
**ruin** — a stone hut resting on the surface — with a creature guarding it.
These make natural landmarks across plains and forests.

Structures are stored in a compact `.scst` ("Survival Cubed STructure") format
that captures a grid of blocks and any creatures inside. In **creator mode** you
can select a region of the world and save your own builds as `.scst` files, then
paste them into any world — see **[Creator Mode](creator-mode.md#the-structure-tool)**.

## The underworld

Beneath the overworld lies the **underworld**: a vast vaulted hall of **charred
rock**. A solid charred **ceiling** caps it overhead, and far below stretches the
charred floor you walk on — between them opens a tall cavern with plenty of
headroom to build and climb through. The floor's *ash valley* zones are blanketed
in loose **ash**, and scattered patches of natural **fire** burn on exposed
floors — a real hazard, so watch your step. The ash valleys are also the only
haunt of the **[ash twister](creatures.md#hostile-monsters)**, a whirling column
of ash that flings you skywards for a punishing fall, and of the fire-trailing
**charred skeleton**. Below the floor, charred
rock falls away to bedrock, riddled with the same **winding tunnels** and deep
**caverns** as the overworld's caves — where the tungsten hides.

The underworld is the only source of **tungsten ore** — the top tier of gear — and
of **gold ore**, too soft for tools but the key to reinforcing a
[chest](blocks.md#special-blocks-in-detail) into a locked one. (Tungsten *gear* and
*ingots* can also be won in the overworld by hunting **[dark
knights](entities/dark-knight.md)** at night, but the ore itself is only down here.) It sits in
permanent dim, warm light (no day/night), and its creatures are active around the
clock. Because it is **eternal night** down there, undead that would burn up at
daybreak on the surface — zombies, skeletons, necromancers and their skulls — never
burn in the underworld, and creatures that only hunt after dark stay aggressive at
all times.

To get there, craft a **[fire key](crafting.md#tools-and-utility-items)** (it
needs tungsten itself, so your first trip down is the hard one) and right-click
to warp between dimensions. The key remembers where you last used it on each
side, so warping back drops you at the exact spot you keyed out from rather than
a generic landing — your first crossing into a dimension picks a fresh spot, and
every crossing after returns you to where you left.

## The arena

The **arena** is a separate combat plane built entirely of **stone bricks**: a
single, flat, level floor of dressed stone stretching out forever, with open air
above it and solid brick below. There is no terrain, no day or night (it sits
under a steady, even light), and no ambient life — it exists for one thing, the
**boss fight**.

A world has exactly one **[Demon King](entities/demon-king.md)** — a towering,
flying boss that holds court in the arena. The first time you step in with the
king still alive, it rises across the floor from you, flanked by **eight
[orc-mage](entities/orcs.md#orc-mage) guardians** scattered across the field, and a
**boss health bar** appears at the top of the screen. The king is **invulnerable
until every guardian is slain** — hunt the mages down first, then bring the king
itself to bay. Slay it and it leaves a **chest** of spoils where it falls (the full
tungsten arsenal, a hoard of ingots and gold, and a meal) — and **no new king is
ever raised** in that world. Win the arena once and it is won for good.

To get there, craft an **[arena key](crafting.md#tools-and-utility-items)** from
tungsten and gold and right-click to warp in. Right-click the key again while in
the arena to return to exactly where you entered from.

## Weather

There is no weather system — no rain, snow, or storms. The only environmental
cycle is day and night on the surface (see
[Gameplay & Survival](gameplay.md#the-daynight-cycle)).
