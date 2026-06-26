# Survival Cubed

A multiplayer-first 2D voxel survival game written in Rust. Mine, craft, build,
fight, and explore an infinite procedurally generated world — solo, on your LAN,
or over the internet. Descend into a fiery underworld for rare ore, tame a puppy,
and fortify against the creatures that come out at night.

> 📖 **Full documentation:** <https://playforge-coding.github.io/survival-cubed/>

## Features

- **Infinite, seeded worlds** — plains, forests, mountains, and deserts on the
  surface; a charred **underworld** below, reached with a crafted **fire key**;
  and a stone-brick **arena** for the boss fight, reached with an **arena key**.
- **Boss fight** — a flying **Demon King** holds the arena (one per world); slay it
  for a chest of loot.
- **Mining & crafting** — four tool tiers (wood → stone → iron → tungsten),
  smelting at a **forge**, and cooking at a **campfire**.
- **Survival** — health, fall damage, hunger for cooked food, and hostile mobs
  (zombies, skeletons, spiders, snakes, slimes) that hunt at night.
- **Creatures & pets** — chickens, goats, and wild **cats** and **puppies** you
  can tame with cooked meat. Pets follow you, hunt, sit on command, and respawn.
- **Multiplayer over QUIC** — host a dedicated server, auto-discover games on
  your LAN, password-protected accounts, chat, and admin moderation.
- **Creator mode** — flight, infinite blocks, time control, creature spawning,
  and a structure tool to save and paste your builds (`.scst` files).
- **Persistent worlds** — chunks, players, creatures, and inventories are saved
  automatically; clean shutdowns lose nothing.

## Quick start

### Play

Download a prebuilt binary from the [Releases](https://github.com/playforge-coding/survival-cubed/releases)
page, or build from source (below). Then just run it:

```sh
survival-cubed
```

This opens the graphical client, where you can start a **singleplayer** world,
**host** a game on your LAN, or **join** a discovered or manually entered server.

### Host a dedicated server

```sh
survival-cubed server [port] [creator]
```

- `port` — listening port (default `5000`).
- `creator` — optional keyword; if present, every player may use creator mode.
  Omit it for a survival-only server.

The server prints its listen address and a certificate fingerprint, then runs
until you press <kbd>Ctrl</kbd>+<kbd>C</kbd> (it saves the world on shutdown).

## Building from source

You need a recent stable [Rust toolchain](https://rustup.rs/). Game assets
(textures and structures) are stored in **Git LFS**, so make sure
[git-lfs](https://git-lfs.com/) is installed and the files are pulled before
building — otherwise `include_bytes!` will embed LFS pointer files instead.

```sh
git clone https://github.com/playforge-coding/survival-cubed.git
cd survival-cubed
git lfs pull
cargo run --release
```

On Linux you also need the windowing/input system libraries plus ALSA (for
audio):

```sh
sudo apt-get install -y libxkbcommon-dev libwayland-dev libx11-dev \
  libxcursor-dev libxi-dev libxrandr-dev libasound2-dev
```

## Documentation

The full game guide lives in [`docs/`](docs/) and is published to GitHub Pages.

- **[Getting Started](docs/getting-started.md)** — install, build, and launch.
- **[Controls & HUD](docs/controls.md)** — every key and on-screen element.
- **[Gameplay & Survival](docs/gameplay.md)** — health, day/night, swimming.
- **[Blocks](docs/blocks.md)** and **[Crafting & Tools](docs/crafting.md)**.
- **[The World](docs/world.md)** — biomes, ores, caves, the underworld.
- **[Creatures](docs/creatures.md)** — mobs, pets, and combat.
- **[Multiplayer](docs/multiplayer.md)** and **[Creator Mode](docs/creator-mode.md)**.
- **[Saves & Files](docs/saves.md)** — where everything is stored.
- **[Credits](docs/credits.md)** — third-party asset attributions.

To preview the docs locally:

```sh
pip install zensical
zensical serve
```

## Credits

Survival Cubed bundles third-party assets. The background music is from
OpenGameArt.org: the first overworld track (`assets/music/overworld/0.ogg`) by
[bart](https://opengameart.org/users/bart), and the remaining tracks by
[remaxim](https://opengameart.org/users/remaxim). The dual-licensed tracks are
used under their GPL-3.0 option (rather than CC BY-SA 3.0) so they can be embedded
in the binary. See [docs/credits.md](docs/credits.md) for the full list.

## License

- **Game code** is licensed under the [GNU AGPL-3.0-only](LICENSE).
- **Documentation** (the `docs/` directory) is licensed under
  [CC BY-NC-SA 4.0](LICENSE-DOCS).
