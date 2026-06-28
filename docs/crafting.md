# Crafting & Tools

## Inventory

Your inventory holds **36 slots**: a **9-slot hotbar** (selected with keys
<kbd>1</kbd>–<kbd>9</kbd>) and **27 storage slots** shown on the inventory screen
(<kbd>E</kbd>). Most items stack to **64**; tools and full water buckets don't
stack. Tools remember their durability even when dropped and picked up again.

## Crafting

Open the inventory (<kbd>E</kbd>) to see the crafting list, which **scrolls** (with
the scroll wheel or its scrollbar) when there are more recipes than fit. Recipes are
shapeless — you just need the listed ingredients somewhere in your inventory —
and clicking a recipe crafts it if you have the materials. Crafting is
server-authoritative, so the result is always consistent in multiplayer.

### All crafting recipes

| Recipe | Ingredients | Output |
|---|---|---|
| Split logs | 1 Log | 1 Wood + 4 Bark |
| Wooden Pickaxe | 3 Stick | 1 Pickaxe |
| Stone Pickaxe | 3 Stone + 2 Stick | 1 Stone Pickaxe |
| Iron Pickaxe | 3 Iron Ingot + 2 Stick | 1 Iron Pickaxe |
| Tungsten Pickaxe | 3 Tungsten Ingot + 2 Stick | 1 Tungsten Pickaxe |
| Wood Sword | 2 Wood + 1 Stick | 1 Wood Sword |
| Stone Sword | 2 Stone + 1 Stick | 1 Stone Sword |
| Iron Sword | 2 Iron Ingot + 1 Stick | 1 Iron Sword |
| Tungsten Sword | 2 Tungsten Ingot + 1 Stick | 1 Tungsten Sword |
| Wood Axe | 3 Wood + 2 Stick | 1 Wood Axe |
| Stone Axe | 3 Stone + 2 Stick | 1 Stone Axe |
| Iron Axe | 3 Iron Ingot + 2 Stick | 1 Iron Axe |
| Tungsten Axe | 3 Tungsten Ingot + 2 Stick | 1 Tungsten Axe |
| Musket | 2 Wood + 1 Iron Ingot | 1 Musket |
| Bullet | 1 Iron Ingot | 8 Bullet |
| Stone Bricks | 4 Stone | 4 Stone Bricks |
| Forge | 8 Stone | 1 Forge |
| Campfire | 1 Stone + 5 Bark | 1 Campfire |
| Ladder | 1 Wood + 2 Stick | 3 Ladder |
| Rope | 2 Bark | 1 Rope |
| Rope Ladder | 3 Rope | 1 Rope Ladder |
| Paper | 2 Bark | 1 Paper |
| Clone Summoner Spell | 1 Summoner Spell + 12 Paper | 2 Summoner Spell |
| Clone Sunburst Spell | 1 Sunburst Spell + 12 Paper | 2 Sunburst Spell |
| Clone Restore Spell | 1 Restore Spell + 12 Paper | 2 Restore Spell |
| Clone Dragonian Steed Spell | 1 Dragonian Steed Spell + 12 Paper | 2 Dragonian Steed Spell |
| Clone Dragon Plate Spell | 1 Dragon Plate Spell + 12 Paper | 2 Dragon Plate Spell |
| Bucket | 3 Iron Ingot | 1 Bucket |
| Boat | 5 Wood + 3 Stick | 1 Boat |
| Door | 6 Wood | 1 Door |
| Fire Key | 1 Charred Rock + 1 Tungsten Ingot | 1 Fire Key |
| Arena Key | 2 Tungsten Ingot + 2 Gold Ingot + 5 Dragon Scale + 5 Minotaur Horn | 1 Arena Key |
| Sign | 1 Wood + 1 Stick | 1 Sign |
| Quest Board | 4 Wood + 2 Stick | 1 Quest Board |
| Chest | 1 Iron Ingot + 8 Wood | 1 Chest |
| Iron Armor | 24 Iron Ingot | 1 Iron Armor |
| Tungsten Armor | 24 Tungsten Ingot | 1 Tungsten Armor |

!!! tip "Where to start"
    Punch a tree for logs, split them into **wood + bark**, harvest **sticks**
    from leaves, and craft a **wooden pickaxe**. From there mine stone for a
    stone pickaxe and a **forge**, then work up through iron and tungsten.

## Tools

There are four tiers of every tool, with rising durability, damage, and mining
power. Each higher pickaxe tier can mine the ores the previous tier couldn't.

### Pickaxes

| Pickaxe | Durability | Mines up to | Attack |
|---|---|---|---|
| Pickaxe (wood) | 60 | Coal ore, stone | 4 |
| Stone Pickaxe | 132 | Iron ore | 5 |
| Iron Pickaxe | 251 | Tungsten ore | 6 |
| Tungsten Pickaxe | 480 | Everything (fastest) | 8 |

### Weapons

Swords are the best pure weapons; axes hit hard and fell trees fast but wear out
twice as quickly.

| Weapon | Durability | Attack |
|---|---|---|
| Wood Sword | 60 | 8 |
| Stone Sword | 132 | 11 |
| Iron Sword | 251 | 14 |
| Tungsten Sword | 480 | 18 |
| Wood Axe | 60 | 10 |
| Stone Axe | 132 | 13 |
| Iron Axe | 251 | 16 |
| Tungsten Axe | 480 | 20 |

### The musket

The **musket** is a ranged firearm, crafted from **2 wood + 1 iron ingot**. Hold it
and **right-click** to fire a bullet toward your cursor — a fast lead ball that flies
straight until it strikes a monster or a wall, dealing a heavy **30 HP** — far more
than any melee swing. The trade-off is the **slow reload**: a musket fires only about
once every **1.4 seconds**, so it can't be fanned like a sword. Each shot spends one
**bullet**; cast a batch of **8 bullets from 1 iron ingot** at the crafting table and
keep them stocked. The musket never wears out — only the bullets are consumed — but an
empty musket can't fire, so mind your ammunition. It's the same weapon a wild
[musketeer](../entities/musketeer.md) carries.

### Repairing tools

Open a **forge** and spend a tool's crafting material (an ingot, for example) to
restore its durability rather than crafting a brand-new one.

### Armor

A **suit of armor** is forged whole — there are no separate helmets, greaves, or
boots, just the one piece — from a hoard of **24 ingots** (all metal: no wood or
stone). It costs nothing to wear: simply **carrying a suit** protects you, and if
you carry more than one the sturdiest applies.

Armor raises your **defense**, never your health. Your maximum health stays at
20 — instead, each blow an enemy lands is **blunted** by a percentage before it
touches you. A hit always lands for at least 1, so armor softens fights without
making you invincible. (Defense reduces enemy *attacks*; environmental burns like
standing in fire still bite through.)

Like a tool, a suit **wears down** — it loses a point of durability for every blow
it soaks, and a suit worn to nothing **shatters**. Mend it at a **forge** with its
own metal (an ingot per repair) before it breaks, exactly as you repair a tool. Its
remaining durability shows as a bar under its inventory icon.

| Armor | Ingredients | Damage blunted | Durability (hits) |
|---|---|---|---|
| Iron Armor | 24 Iron Ingot | 35% | 300 |
| Tungsten Armor | 24 Tungsten Ingot | 55% | 600 |

Tungsten armor can also be **looted** rather than crafted: the
**[Demon King](entities/demon-king.md)** always leaves a suit in his chest, and a
**[Dark Knight](entities/dark-knight.md)** rarely spills one.

For defense beyond any forged suit, the **[dragon plate spell](magic.md#the-dragon-plate-spell)**
— dropped by [Twinscale](entities/twinscale.md) — wraps you in a temporary ward worth
**85%**, far above tungsten's 55%, though it lasts only 8 minutes per cast and replaces
(rather than stacks with) your armor while it holds.

## Smelting

Smelting happens at a **forge** (right-click it). You feed in raw ore and a
separate **fuel**, and the forge produces ingots.

| Recipe | Input | Output |
|---|---|---|
| Iron Ingot | 1 Raw Iron | 1 Iron Ingot |
| Gold Ingot | 1 Raw Gold | 1 Gold Ingot |
| Tungsten Ingot | 1 Raw Tungsten | 1 Tungsten Ingot |

**Fuel options** (per smelt): 1 Wood, **or** 1 Coal, **or** 4 Bark. Coal burns
the longest, bark is the weakest.

**Gold** smelts into ingots but crafts into no tools or weapons (it is too soft).
Its only use is reinforcing a placed [chest](blocks.md#special-blocks-in-detail)
into a **locked chest**: open the chest, type a password, and spend **3 Gold
Ingots** to seal it.

## Cooking

Cooking happens at a **lit campfire** (right-click it). Unlike smelting, cooking
costs no extra fuel beyond keeping the fire lit.

| Recipe | Input | Output |
|---|---|---|
| Cooked Meat | 1 Raw Meat | 1 Cooked Meat |

## Food

Eat the food in your selected hotbar slot with <kbd>F</kbd>.

| Food | Effect | Source |
|---|---|---|
| Apple | +4 health | Drops from leaves (15%) — also used to tame a [horse](creatures.md#mounts) |
| Cooked Meat | +8 health | Cook raw meat on a lit campfire — also tames [cats and puppies](creatures.md#pets) |
| Raw Meat | −3 health (makes you sick) | Drops from chickens and goats — **cook it first!** |

## Tools and utility items

- **Bucket** — right-click water to fill it (becoming a **Water Bucket**), then
  right-click an empty cell to pour the water out. A full bucket carries one load.
- **Boat** — right-click to climb aboard and glide across water at full speed,
  right-click again to step out. It's a reusable vehicle (never consumed) and rides
  on the water surface; see **[Boats](gameplay.md#boats)**.
- **Fire Key** — a reusable artifact that warps you between the **overworld** and
  the **underworld**. Right-click to use it. It remembers where you last fired it
  on each side and returns you there, so the two anchors act like a linked pair of
  portals; the first time you cross into a dimension (no anchor yet) the server
  picks a safe landing spot. Crafting one requires reaching the underworld for
  tungsten first — see **[The World](world.md#the-underworld)**.
- **Arena Key** — a reusable artifact that warps you into the **[arena](world.md#the-arena)**,
  the stone-brick boss plane. Right-click to enter; right-click again while inside
  to return to exactly where you left from. Crafting one needs tungsten and gold
  plus the spoils of **both** underworld minibosses — five
  **[dragon scales](entities/dragon.md)** and five
  **[minotaur horns](entities/minotaur.md)** — so it is firmly late-game gear, and
  what it opens is the **[Demon King](entities/demon-king.md)** boss fight.

## Materials reference

| Material | Source | Used for |
|---|---|---|
| Wood | Splitting logs | Planks, swords, ladders, doors |
| Bark | Splitting logs | Campfires, rope, paper, fuel |
| Stick | Leaves (70% drop) | Almost every tool and weapon |
| Coal | Coal ore | Fuel |
| Raw Iron / Iron Ingot | Iron ore → forge | Iron tools, buckets, armor, repairs |
| Raw Tungsten / Tungsten Ingot | Tungsten ore → forge | Tungsten gear, armor, fire keys, arena keys |
| Raw Gold / Gold Ingot | Gold ore → forge | Locking chests, arena keys |
| Rope | Bark | Rope ladders |
| Paper | Bark | Cloning spellbooks |
| Dragon Scale | Slain [dragon](entities/dragon.md) | Arena keys |
| Minotaur Horn | Slain [minotaur](entities/minotaur.md) | Arena keys |
