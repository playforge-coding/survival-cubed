# Getting Started

## Installing

The easiest way to play is to grab a prebuilt binary for your platform from the
[Releases page](https://github.com/playforge-coding/survival-cubed/releases).
Each release ships a single self-contained executable for Windows, macOS
(Intel and Apple Silicon), and Linux — all game assets are embedded, so there
is nothing else to download.

| Platform | Download |
|---|---|
| Windows | `survival-cubed-<version>-x86_64-pc-windows-msvc.zip` |
| macOS (Apple Silicon) | `survival-cubed-<version>-aarch64-apple-darwin.tar.gz` |
| macOS (Intel) | `survival-cubed-<version>-x86_64-apple-darwin.tar.gz` |
| Linux | `survival-cubed-<version>-x86_64-unknown-linux-gnu.tar.gz` |

Unpack the archive and run the `survival-cubed` executable.

## Building from source

You need a recent stable [Rust toolchain](https://rustup.rs/) (the project uses
the 2024 edition). Game textures and structures live in **Git LFS**, so install
[git-lfs](https://git-lfs.com/) and pull them before building — otherwise the
build embeds LFS pointer files instead of the real assets.

```sh
git clone https://github.com/playforge-coding/survival-cubed.git
cd survival-cubed
git lfs pull
cargo run --release
```

### Linux build dependencies

The client needs the system windowing and input libraries:

```sh
sudo apt-get install -y libxkbcommon-dev libwayland-dev libx11-dev \
  libxcursor-dev libxi-dev libxrandr-dev
```

## Launching the game

Run the executable with no arguments to open the graphical client:

```sh
survival-cubed
```

From the menu you can:

- **Singleplayer** — start a private local world.
- **Host on LAN** — start a world and advertise it so others on your network
  can join without typing an address.
- **Join** — pick a server discovered on your LAN, or enter an address manually.

See **[Multiplayer](multiplayer.md)** for joining over the internet and for
running a dedicated server.

## Running a dedicated server

For an always-on, headless server use the `server` subcommand:

```sh
survival-cubed server [port] [creator]
```

| Argument | Default | Meaning |
|---|---|---|
| `port` | `5000` | TCP/UDP port to listen on |
| `creator` | *(off)* | If present, **every** player may enter creator mode |

Examples:

```sh
survival-cubed server                 # survival server on port 5000
survival-cubed server 5001            # survival server on port 5001
survival-cubed server 5001 creator    # creator-enabled server on port 5001
```

On startup the server prints its listen address, the world save directory, its
mode, and a **certificate fingerprint** that clients can verify on first
connection. Stop the server with <kbd>Ctrl</kbd>+<kbd>C</kbd> (or `SIGTERM`); it
flushes the world to disk on a clean shutdown, so nothing is lost.

!!! note "Where are my files?"
    Worlds, screenshots, and saved login credentials are stored under your
    platform's standard data and config directories. See **[Saves & Files](saves.md)**
    for exact locations.

## Next steps

- Learn the **[Controls & HUD](controls.md)**.
- Understand **[Gameplay & Survival](gameplay.md)** before nightfall.
- Plan your tools with **[Crafting & Tools](crafting.md)**.
