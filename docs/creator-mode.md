# Creator Mode

Creator mode is a build-and-experiment mode that removes survival constraints. It
grants flight, unlimited free blocks, control over time, creature spawning, item
giving, and a tool for saving and pasting structures. Monsters ignore you and you
take no damage from them.

## Enabling it

Creator mode must be **allowed by the server**:

- On a **creator-type server** (started with the `creator` argument, e.g.
  `survival-cubed server 5000 creator`), **every player** may enter it.
- On a normal **survival server**, only the **host/admin** may enter it.

When it's available, a **Creator Tools** window appears at the top-right of the
screen. Tick the **creator mode** checkbox to switch in or out. Leaving creator
mode also turns off flight (you won't be left hovering).

## Flight

While in creator mode you fly:

- Gravity is off. Rise with <kbd>Space</kbd>/<kbd>W</kbd>, descend with
  <kbd>S</kbd>, move sideways as usual.
- **Scroll the mouse wheel** to set the fly-speed multiplier from **1.0× to
  8.0×**.

## Infinite blocks

Pick a block from the Creator Tools block picker (stone, dirt, grass, log,
leaves, charred rock, fire, …) and **right-click to place it for free** — no
inventory cost. Mining still works normally with left-click.

## Time control

Drag the **time-of-day slider**, or use the preset buttons — **Dawn**, **Noon**,
**Dusk**, **Midnight** — to set the lighting and day/night state instantly.

## Spawning creatures

Buttons in the Creator Tools window spawn any creature at your location: Slime,
Chicken, Goat, Cat, Puppy, Horse, Zombie, Spider, Snake, Skeleton, Charred
Skeleton, Demon, Orc, Orc Mage, Enchanted Demon, Necromancer, Skull, Knight, Dark
Knight, and Dragon. The row of buttons **scrolls horizontally**, so drag or scroll
sideways to reach the ones off the edge. See **[Creatures](creatures.md)** for what
each one does.

## Giving items

Enter an item id or name and a quantity to give yourself any item — handy for
testing recipes or stocking a build with materials.

## The structure tool

The structure tool lets you save part of the world and paste it elsewhere — even
into other worlds.

1. Switch the Creator Tools to the **Selection** tool.
2. **Drag with the left mouse button** to mark a rectangular region (up to
   256×256 cells). Right-click to clear the selection.
3. Press **Save** to store the region as a `.scst` file. Any creatures inside the
   region are captured with it.
4. Saved structures appear in a library with **Load** and **Delete** buttons.
5. Switch to the **Build** tool and use the paste preview outline to stamp a
   saved structure into the world.

Structures are stored in the `.scst` ("Survival Cubed STructure") format — a
compact grid of blocks plus an optional list of captured creatures — under your
data directory (see [Saves & Files](saves.md)). The same format is used for the
ruins that generate naturally in the world.

!!! note "Survival players are protected"
    On survival servers only the host can build with creator tools, so these
    powers can't be used to cheat in a normal multiplayer game.
