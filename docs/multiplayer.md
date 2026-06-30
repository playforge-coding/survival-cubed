# Multiplayer

Survival Cubed is multiplayer-first. Games run on a server — which can be the
one your own client hosts, or a separate dedicated process — and players connect
over an encrypted **QUIC** connection.

## Hosting

### From the client

Choose **Host on LAN** in the menu. Your world is advertised over your local
network so others can find and join it without typing an address. To also let
players outside your network in, tick **Forward port via UPnP (internet)** — see
[UPnP port forwarding](#upnp-port-forwarding) below. Tick **Enable voice chat
(push-to-talk)** to let connected players talk to each other — see
[Voice chat](#voice-chat) — and/or **Enable webcam video (press K)** to let them
show their cameras above their characters — see [Webcam video](#webcam-video).

### Dedicated server

For an always-on server, run the headless `server` subcommand:

```sh
survival-cubed server [port] [creator] [upnp] [voice] [webcam] [voice-port=N]
```

| Argument | Default | Meaning |
|---|---|---|
| `port` | `5000` | Port to listen on |
| `creator` | *(off)* | If present, all players may use creator mode |
| `upnp` | *(off)* | If present, forward the port on your router via UPnP |
| `voice` | *(off)* | If present, enable [voice chat](#voice-chat) over a MOQ relay |
| `webcam` | *(off)* | If present, enable [webcam video](#webcam-video) over the same relay |
| `voice-port=N` | `port + 1` | UDP port for the voice/webcam relay (with `voice` or `webcam`) |

`creator`, `upnp`, `voice`, and `webcam` are order-independent flags, so
`survival-cubed server 5000 upnp` and
`survival-cubed server 5000 creator voice webcam` both work. Voice and webcam are
**independent toggles** that share one relay on the voice port, so enabling both
adds no second port.

The server binds to all network interfaces (`0.0.0.0`) so it's reachable from
other machines. On startup it prints its address, save directory, mode, whether
UPnP is on, whether voice and webcam are on, and a **certificate fingerprint**.
Stop it cleanly with <kbd>Ctrl</kbd>+<kbd>C</kbd> (it saves on shutdown).

### UPnP port forwarding

To reach a server over the **internet**, the chosen port must be forwarded from
your router to your machine. You can do this manually in your router's admin
page, or let the game do it for you with **UPnP**:

- **Client:** tick **Forward port via UPnP (internet)** under *Host on LAN*. A
  dialog spells out the security implications and only enables forwarding once
  you confirm.
- **Dedicated server:** pass the `upnp` flag (the warning is printed to the log).

When enabled, the game asks your router (over UPnP-IGD) to forward the game's
**UDP** port, renews that mapping while the server runs, and removes it on
shutdown. Then share your public address as `host:port`. It's best-effort: if
your router lacks UPnP or has it disabled, hosting still works — you just have to
forward the port manually.

!!! warning "Security implications"

    UPnP opens a hole in your router's firewall, exposing the server to the
    **public internet**, not just your LAN, and it does so without any
    authentication — any program on your network can request it, and some router
    firmware handles such requests poorly. The world password still controls who
    may *join*, but the listening socket itself becomes publicly reachable.

    Only enable UPnP if you intend to host for people outside your network, keep
    a **strong world password**, and turn it off (or disable UPnP on your router)
    when you only play on the LAN. If you'd rather not rely on UPnP at all,
    forward the port manually instead.

## Joining

- **LAN auto-discovery** — servers advertise themselves via mDNS / DNS-SD, so
  games on your network appear in the join list automatically. Just pick one.
- **Manual** — enter an address like `127.0.0.1:5000` or `myhost.example:5000`.

The first time you connect to a server, your client trusts and remembers its
self-signed certificate (trust-on-first-use); the fingerprint stays stable
across server restarts — and across every world you host — so you'll be warned
if it ever changes.

## Accounts

Accounts are **per-server** and created automatically on first join:

- Enter a **name** (up to 24 characters) and a **password** (required).
- That name/password pair is your account on that server from then on — log back
  in with the same details.
- Passwords are stored **hashed** (Argon2id with a random salt), never in
  plaintext, on the server.
- Your client can remember your logins locally so you don't retype them each
  time (see [Saves & Files](saves.md)).

If a login fails — name already in use, wrong password — the connection closes
with an explanation.

When you rejoin, the server restores your saved position, health, inventory,
dimension, respawn point, and personal waypoints.

## Chat

Open chat with <kbd>Enter</kbd> or <kbd>T</kbd>, type, and press <kbd>Enter</kbd>
to send (up to 256 characters). Messages are broadcast to everyone and labelled
with your player name. Chat shows the most recent lines in the bottom-left
overlay.

A `:name:` token in a message is drawn as an inline 16px icon: any block or item
(for example `:stone:` or `:iron_pickaxe:`) and any creature's base sprite (for
example `:zombie:`, `:dragon:`, `:gargoyle:`). Hover an icon to see its name. A
token that doesn't name something drawable is left as plain text.

## Admin commands

The **host** holds an admin token (generated at server start and given only to
the host's own client), which unlocks moderation commands. Type these into chat:

| Command | Effect |
|---|---|
| `/ban <name\|ip>` | Ban a connected player by name, or ban a raw IP; kicks everyone from that IP. Persists across restarts. |
| `/unban <ip>` | Lift a ban on an IP address |
| `/banlist` (or `/bans`) | Privately list all banned IPs |
| `/spectate <name>` | Lock your camera onto a player and follow them (across dimensions) |
| `/spectate` | Stop spectating and return to where you were |
| `/help` | Show the available admin commands |

Ban and unban events are announced in chat as messages from **"Server"**. While
spectating, a banner reads **"👁 Spectating *player* — /spectate to stop"**.

Bans are stored in a `bans.txt` file in the world's save directory (one IP per
line), read on startup and rewritten whenever you ban or unban.

## Voice chat

Voice chat is **optional and controlled by the server owner**. It is off unless
the host turns it on — tick **Enable voice chat** when hosting from the client, or
pass the `voice` flag to a dedicated server. Clients show no voice UI and open no
microphone unless the server they joined offers it.

When enabled, the server runs a small in-process **[MOQ](https://moq.dev)** (Media
over QUIC) relay on its own UDP port (the **voice port**, default game port + 1).
Each player publishes their microphone as a single **Opus** stream and subscribes
to everyone else's, so audio is **global** — everyone hears everyone, at full
volume.

For players:

- **Hold <kbd>V</kbd>** to talk (push-to-talk); release to go quiet.
- **<kbd>G</kbd>** turns voice off and on locally (mutes your mic *and* playback).
- A **🎤 Talking** marker appears while you transmit; everyone you can hear is
  listed as **🔊 *name*** in the top-right.
- No microphone is needed to listen. If none is found you can still hear others.

For hosts:

- The voice port is **separate** from the game port and must also be reachable.
  When you host with both **UPnP** and **voice** on, the voice port is forwarded
  too; otherwise forward it yourself the same way as the game port.
- The relay uses its own self-signed certificate, generated at start-up. Its
  fingerprint is sent to each client inside the normal join handshake, so the
  voice connection is pinned automatically with no extra prompt.
- The relay only forwards audio between connected players — it never records or
  mixes anything server-side.

## Webcam video

Webcam video is **optional and controlled by the server owner**, and is a
**separate toggle from [voice chat](#voice-chat)** — a server can offer either,
both, or neither. Turn it on by ticking **Enable webcam video** when hosting from
the client, or by passing the `webcam` flag to a dedicated server. Clients show no
webcam UI and open no camera unless the server they joined offers it.

It rides the **same [MOQ](https://moq.dev) relay** as voice (the same voice port),
so enabling both features needs no extra port. Each player who turns their camera
on publishes a tiny **AV1** video stream (128×96, ~10 fps); everyone subscribes to
everyone else's and sees a small live thumbnail floating above that player's
character.

For players:

- **<kbd>K</kbd>** turns your camera on and off. It starts **off** — your camera
  is never opened until you press <kbd>K</kbd> (and its light stays off until then).
- A **📷 On air** marker appears while your camera transmits.
- You always *receive* others' video even without a camera of your own.

For hosts:

- Webcam shares the voice port and the same auto-pinned relay certificate, so the
  same reachability/forwarding notes as [voice chat](#voice-chat) apply.
- The relay only forwards video between connected players — it never records or
  transcodes anything server-side.
- Software AV1 encode/decode costs CPU; the tiny 128×96 ~10 fps preset keeps a
  handful of simultaneous cameras affordable.

## Live map

Every player has an in-game **minimap** — toggled with <kbd>H</kbd>, pinned to the
top-left corner — that draws the explored world from each block's average colour,
with your own position as a yellow dot (see **[Controls → Map](controls.md#map)**).
Unlike voice and webcam, the map needs no host toggle: it is **always on** in
multiplayer.

It rides the **same [MOQ](https://moq.dev) relay** as voice and webcam (sharing
the relay port), so it needs no extra port — and the relay starts for every hosted
server even when voice and webcam are off. Each player continuously publishes two
things over their own map broadcast:

- their **live position**, so everyone else sees their blue dot move in real time —
  even players far away or in chunks you haven't loaded; and
- the **chunks they've explored** (block ids, coloured on the receiver), so the map
  reveals terrain *anyone* has discovered, not just where you've personally been.

Newly-loaded chunks are shared immediately, and a slow background rotor
re-broadcasts known chunks so a player who joins later catches up over the next
little while. Only the dimension you're in is shown — positions and terrain from
other dimensions don't appear. As with voice/webcam, the relay only forwards this
between connected players; nothing is recorded server-side.

## Under the hood

- **Transport:** QUIC over UDP, encrypted with TLS 1.3 — lower latency than TCP
  and secure by default.
- **Voice:** a separate MOQ-over-QUIC relay (see [Voice chat](#voice-chat)) on its
  own port, carrying Opus audio; entirely optional and off by default.
- **Webcam:** [AV1](#webcam-video) video over the *same* relay (a separate toggle
  from voice), carrying tiny 128×96 thumbnails; also optional and off by default.
- **Live map:** player positions and explored chunks over the *same* relay (see
  [Live map](#live-map)); always on, so hosting starts the relay even with voice
  and webcam off.
- **Certificates:** each machine has a single stable self-signed certificate,
  stored in the user config dir and shared by every world it hosts; clients pin
  it on first connection or auto-trust it via LAN discovery.
- **Authority:** the server simulates the world, creatures, health, damage, and
  crafting, so results stay consistent for everyone. Your own movement is driven
  by your client and synced out to others — each update carries your position
  *and* velocity, so others see your avatar face the way you're moving and play
  its walk cycle rather than freezing in one pose.

There is no fixed player cap and no team/guild system — chat is global, and the
server is built to scale with the host's resources.
