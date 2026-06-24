# Crafting & Tools

## Inventory

Your inventory holds **36 slots**: a **9-slot hotbar** (selected with keys
<kbd>1</kbd>–<kbd>9</kbd>) and **27 storage slots** shown on the inventory screen
(<kbd>E</kbd>). Most items stack to **64**; tools and full water buckets don't
stack. Tools remember their durability even when dropped and picked up again.

## Crafting

Open the inventory (<kbd>E</kbd>) to see the crafting list. Recipes are
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
| Forge | 8 Stone | 1 Forge |
| Campfire | 1 Stone + 5 Bark | 1 Campfire |
| Ladder | 1 Wood + 2 Stick | 3 Ladder |
| Rope | 2 Bark | 1 Rope |
| Rope Ladder | 3 Rope | 1 Rope Ladder |
| Bucket | 3 Iron Ingot | 1 Bucket |
| Door | 6 Wood | 1 Door |
| Fire Key | 1 Charred Rock + 1 Tungsten Ingot | 1 Fire Key |

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

### Repairing tools

Open a **forge** and spend a tool's crafting material (an ingot, for example) to
restore its durability rather than crafting a brand-new one.

## Smelting

Smelting happens at a **forge** (right-click it). You feed in raw ore and a
separate **fuel**, and the forge produces ingots.

| Recipe | Input | Output |
|---|---|---|
| Iron Ingot | 1 Raw Iron | 1 Iron Ingot |
| Tungsten Ingot | 1 Raw Tungsten | 1 Tungsten Ingot |

**Fuel options** (per smelt): 1 Wood, **or** 1 Coal, **or** 4 Bark. Coal burns
the longest, bark is the weakest.

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
| Apple | +4 health | Drops from leaves (15%) |
| Cooked Meat | +8 health | Cook raw meat on a lit campfire |
| Raw Meat | −3 health (makes you sick) | Drops from chickens and goats — **cook it first!** |

## Tools and utility items

- **Bucket** — right-click water to fill it (becoming a **Water Bucket**), then
  right-click an empty cell to pour the water out. A full bucket carries one load.
- **Fire Key** — a reusable artifact that warps you between the **overworld** and
  the **underworld**. Right-click to use it; the server picks a safe landing spot.
  Crafting one requires reaching the underworld for tungsten first — see
  **[The World](world.md#the-underworld)**.

## Materials reference

| Material | Source | Used for |
|---|---|---|
| Wood | Splitting logs | Planks, swords, ladders, doors |
| Bark | Splitting logs | Campfires, rope, fuel |
| Stick | Leaves (70% drop) | Almost every tool and weapon |
| Coal | Coal ore | Fuel |
| Raw Iron / Iron Ingot | Iron ore → forge | Iron tools, buckets, repairs |
| Raw Tungsten / Tungsten Ingot | Tungsten ore → forge | Tungsten gear, fire keys |
| Rope | Bark | Rope ladders |
