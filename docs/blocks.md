# Blocks

Blocks are the building material of the world. Each has a **hardness** (roughly
how long it takes to mine by hand) and may need a particular tool tier before it
yields anything. The right [pickaxe or axe](crafting.md) mines much faster.

## Natural blocks

| Block | Solid | Hardness | Notes |
|---|---|---|---|
| Stone | ✅ | 1.2 | The bulk of underground terrain; mine with a pickaxe |
| Dirt | ✅ | 0.5 | Quick to dig by hand |
| Grass | ✅ | 0.5 | The overworld surface layer |
| Sand | ✅ | 0.5 | Covers the desert biome |
| Log | ✅ | 1.0 | Tree trunks; an **axe** fells them ~2.5× faster |
| Leaves | ✅ | 0.3 | Drops a **stick** (70%), an **apple** (15%), or nothing |
| Charred Rock | ✅ | 1.3 | Underworld terrain |
| Ash | ✅ | 0.5 | Blankets the underworld's ash valleys |
| Water | ❌ | — | A fluid you swim through; can't be mined by hand |
| Fire | ❌ | — | An underworld hazard that burns you; yields nothing |

## Ores

| Ore | Hardness | Tool required | Drops |
|---|---|---|---|
| Coal Ore | 1.5 | Any pickaxe | Coal |
| Iron Ore | 2.0 | Stone pickaxe or better | Raw iron |
| Tungsten Ore | 2.6 | Iron pickaxe or better | Raw tungsten (underworld only) |

Smelt raw iron and raw tungsten into ingots at a [forge](crafting.md#smelting).

## Crafted & placeable blocks

| Block | Solid | Hardness | Notes |
|---|---|---|---|
| Wood | ✅ | 0.8 | Plank block crafted from logs |
| Forge | ✅ | 1.5 | Smelting station; right-click to open |
| Campfire | ❌ | 0.6 | Cooking station and respawn point; right-click to fuel/cook |
| Ladder | ❌ | 0.4 | Climbable; needs side or below support |
| Rope Ladder | ❌ | 0.4 | Climbable; unrolls downward when placed |
| Door | ✅ | 0.6 | Two cells tall; right-click to open/close |

### Special blocks in detail

- **Doors** occupy two cells (a bottom and a top half). Right-click either half to
  open or close it. A closed door blocks movement; an open one lets you pass.
  Placing a door needs clear space above.
- **Forge** — right-click to open the smelting interface. Burn fuel (wood, coal,
  or bark) to turn raw iron/tungsten into ingots, and repair worn tools by
  spending their crafting material. See [Smelting](crafting.md#smelting).
- **Campfire** — right-click to add fuel and, once lit, cook raw meat into cooked
  meat. Opening a campfire also sets it as your **respawn point**. A lit campfire
  is a light source.
- **Ladders & rope ladders** are non-solid; you climb them with the up/down keys.
  Rope ladders are made from rope and are ideal for dropping into caves.

## Block properties summary

- **Solid** blocks stop movement and can be built on; **non-solid** blocks
  (water, fire, campfire, ladders, open doors) can be passed through.
- **Hardness** is the base mining time in seconds by hand; better tools cut this
  down, and using the wrong tool (or none) on hard blocks is very slow or yields
  nothing.
- **Light sources:** a lit campfire emits light. Fire in the underworld glows but
  is purely a hazard.

See **[Crafting & Tools](crafting.md)** for what these blocks become, and
**[The World](world.md)** for where they're found.
