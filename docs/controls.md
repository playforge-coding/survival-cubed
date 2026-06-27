# Controls & HUD

## Movement

| Key | Action |
|---|---|
| <kbd>A</kbd> / <kbd>←</kbd> | Move left |
| <kbd>D</kbd> / <kbd>→</kbd> | Move right |
| <kbd>Space</kbd> / <kbd>W</kbd> / <kbd>↑</kbd> | Jump · climb up a ladder · swim up · fly up (creator · **dragon steed**) |
| <kbd>S</kbd> / <kbd>↓</kbd> | Climb down a ladder · dive in water · fly down (creator · **dragon steed**) |

You can only jump while standing on the ground. On ladders gravity is suspended
and you climb at a steady pace; in water you paddle up and dive down. Riding a
tamed **horse** lets you gallop noticeably faster than you can run (you still jump
and fall normally); riding a **[dragonian steed](magic.md#the-dragonian-steed-spell)**
lets you **fly** — rise on jump, descend on down — see **[Creatures](creatures.md#mounts)**.

## Mouse

| Input | Action |
|---|---|
| **Left button** (hold) | Mine/break the targeted block, or melee-attack a creature |
| **Right button** | Place the selected block, or interact (doors, forge, campfire, sign, quest board, chest, bucket, fire key, boat) · mount/dismount a tamed horse or **[dragonian steed](magic.md#the-dragonian-steed-spell)** · **cast a held [spellbook](magic.md)** toward the cursor |
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
| <kbd>B</kbd> | While riding a **[dragonian steed](magic.md#the-dragonian-steed-spell)**, breathe a fireball at the cursor |

The selected hotbar slot determines which block you place and which food you eat.
See **[Crafting & Tools](crafting.md)** for the inventory layout and recipes.

## View

| Key | Action |
|---|---|
| <kbd>=</kbd> / <kbd>+</kbd> (or numpad <kbd>+</kbd>) | Zoom the camera in (bigger tiles, less world on screen) |
| <kbd>-</kbd> / <kbd>_</kbd> (or numpad <kbd>-</kbd>) | Zoom the camera out (smaller tiles, more world on screen) |

Zoom ranges from **1.5×** (most zoomed out) to **6.0×** (most zoomed in) in 0.5×
steps, starting at **3.0×**. The level is per-session and a status message confirms
each change. Mining, placing, and chunk loading all follow the current zoom, so the
cursor always targets the block it's over.

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
| <kbd>Esc</kbd> | Close an open menu (inventory/forge/campfire/sign/quest board/chest); otherwise leave the world |
| <kbd>F2</kbd> | Take a screenshot (world only — no HUD) |
| <kbd>P</kbd> | Mute / unmute the background music |

Chat messages are up to 256 characters. See **[Multiplayer](multiplayer.md)** for
chat and admin commands.

## Screenshots

Press <kbd>F2</kbd> to capture the current view **without** the HUD or any open
menus. The image is encoded on a background thread (the game doesn't stutter) and
saved to your data directory as both a lossless PNG and a compressed JPEG. A
"Screenshot saved." status message confirms the capture.

## Music

Each dimension loops its own background music, played quietly under the game.
Press <kbd>P</kbd> to mute or unmute it — a "Music muted" / "Music unmuted"
status message confirms the change. The setting sticks across track and
dimension changes (and across leaving and re-entering a world) until you toggle
it back.

## The HUD

### Top bar

- Game title and your **health bar** (out of 20 HP).
- Your **mana bar** (✦, out of 100) — the magic resource for casting [spellbooks](magic.md).
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
- **Sign** (right-click a sign) — write up to 5 short lines of text; **Save** to
  share them with everyone.
- **Quest Board** (right-click a quest board) — post, edit, or delete up to 5
  notes, each its own short message; **Save** to share them.
- **Chest** (right-click a chest) — a 27-slot store. Click a slot then another to
  move stacks between the chest and your bag. A plain chest can be **reinforced**
  with gold and a password to become a **locked chest**; a locked one asks for its
  password (remembered for the session) before it opens.

### Creator tools

If creator mode is allowed, a **Creator Tools** window appears at the top-right.
See **[Creator Mode](creator-mode.md)** for everything it can do.
