# Controls & HUD

## Movement

| Key | Action |
|---|---|
| <kbd>A</kbd> / <kbd>←</kbd> | Move left |
| <kbd>D</kbd> / <kbd>→</kbd> | Move right |
| <kbd>Space</kbd> / <kbd>W</kbd> / <kbd>↑</kbd> | Jump · climb up a ladder · swim up · fly up (creator) |
| <kbd>S</kbd> / <kbd>↓</kbd> | Climb down a ladder · dive in water · fly down (creator) |

You can only jump while standing on the ground. On ladders gravity is suspended
and you climb at a steady pace; in water you paddle up and dive down.

## Mouse

| Input | Action |
|---|---|
| **Left button** (hold) | Mine/break the targeted block, or melee-attack a creature |
| **Right button** | Place the selected block, or interact (doors, forge, campfire, bucket, fire key, boat) |
| **Scroll wheel** | In creator flight: adjust fly-speed multiplier (1.0×–8.0×) |

Your reach is about **80 pixels** (five blocks) from your body, and both mining
hits and block placements have a short cooldown. A darkening overlay on the
targeted block shows mining progress.

## Hotbar & inventory

| Key | Action |
|---|---|
| <kbd>1</kbd>–<kbd>9</kbd> | Select hotbar slot 1–9 |
| <kbd>E</kbd> | Open / close the full inventory and crafting screen |
| <kbd>Q</kbd> | Drop one item from the selected hotbar slot |
| <kbd>F</kbd> | Eat the food in the selected hotbar slot |

The selected hotbar slot determines which block you place and which food you eat.
See **[Crafting & Tools](crafting.md)** for the inventory layout and recipes.

## Waypoints

| Key | Action |
|---|---|
| <kbd>M</kbd> | Mark a personal waypoint at your current location |
| <kbd>N</kbd> | Remove the nearest waypoint |

Waypoints, your home (last campfire) marker 🏠, and your death marker 💀 appear
around the edges of the screen with a distance readout when they're off-screen.

## Chat & UI

| Key | Action |
|---|---|
| <kbd>Enter</kbd> / <kbd>T</kbd> | Open the chat box (type, <kbd>Enter</kbd> to send, <kbd>Esc</kbd> to cancel) |
| <kbd>Esc</kbd> | Close an open menu (inventory/forge/campfire); otherwise leave the world |
| <kbd>F2</kbd> | Take a screenshot (world only — no HUD) |

Chat messages are up to 256 characters. See **[Multiplayer](multiplayer.md)** for
chat and admin commands.

## Screenshots

Press <kbd>F2</kbd> to capture the current view **without** the HUD or any open
menus. The image is encoded on a background thread (the game doesn't stutter) and
saved to your data directory as both a lossless PNG and a compressed JPEG. A
"Screenshot saved." status message confirms the capture.

## The HUD

### Top bar

- Game title and your **health bar** (out of 20 HP).
- A **day/night indicator** (☀ Day / 🌙 Night).
- Control hints and the **number of players online**.
- **Inventory** and **Leave** buttons.
- Your **world coordinates** (shown bottom-right).

### Hotbar (bottom)

Nine slots showing the blocks and items you can place or use. The selected slot
is highlighted, each shows its stack count, and tools display a **durability bar**
that shifts from green to red as they wear.

### Inventory screen (<kbd>E</kbd>)

A 27-slot storage grid plus the 9-slot hotbar row, alongside the **crafting**
recipe list. Click a slot then another to move or combine items; right-click to
drop a whole stack.

### Interaction menus

- **Forge** (right-click a forge) — smelt ores and repair tools.
- **Campfire** (right-click a campfire) — add fuel, cook meat, and set your
  respawn point.

### Creator tools

If creator mode is allowed, a **Creator Tools** window appears at the top-right.
See **[Creator Mode](creator-mode.md)** for everything it can do.
