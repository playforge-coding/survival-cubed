# Saves & Files

Survival Cubed stores worlds, screenshots, structures, and saved logins under
your operating system's standard data and config directories. The examples below
use the Linux paths; macOS and Windows use their platform equivalents.

## Where things live

| Path (Linux) | Contents |
|---|---|
| `~/.local/share/survival-cubed/saves/<world>/` | A singleplayer/host world |
| `~/.local/share/survival-cubed/server-<port>/` | A dedicated server's world |
| `~/.local/share/survival-cubed/structures/` | Saved `.scst` structures |
| `~/.config/survival-cubed/credentials` | Remembered per-server logins |

## What a world contains

```
saves/<world>/
  world.dat              # metadata: seed, clock, spawn, players, creatures
  chunks/                # modified overworld chunks (<cx>_<cy>.dat)
  chunks_underworld/     # modified underworld chunks
  bans.txt               # banned IPs (servers only), one per line
```

`world.dat` holds everything that isn't terrain:

- The **seed** and the in-world **clock** (so day/night resumes where it left off).
- Every **player's** name, position, dimension, health, inventory, respawn point,
  and waypoints — and a hashed password for registered accounts.
- All server-simulated **creatures** (type, position, health).
- Lit **campfires** (position and remaining burn time) and player-placed logs.
- Whether the server is creator-enabled, and which natural structures have
  already spawned their creatures.

## What is and isn't saved

- **Saved:** only the chunks you've **modified**, plus all the metadata above.
- **Not saved:** untouched terrain — it regenerates identically from the seed, so
  unexplored areas cost nothing on disk.

## Autosave & shutdown

The server saves the world **automatically at intervals**, and again on a clean
shutdown (<kbd>Ctrl</kbd>+<kbd>C</kbd> or `SIGTERM`). Saves are written
atomically (to a temporary file, then renamed), so a crash mid-write can't
corrupt your world.

## Screenshots

Pressing <kbd>F2</kbd> saves a HUD-free screenshot to your data directory as both
a lossless **PNG** and a compressed **JPEG**.

## Saved logins

To save retyping your credentials, the client can remember your **per-server**
name and password in `~/.config/survival-cubed/credentials`. This file is stored
in plaintext for convenience, so treat it like any other local secret. The
server only ever stores your password **hashed** — see
[Multiplayer › Accounts](multiplayer.md#accounts).

## Structures

Structures saved in [creator mode](creator-mode.md#the-structure-tool) are written
as `.scst` files in the `structures/` directory and can be loaded into any world.
