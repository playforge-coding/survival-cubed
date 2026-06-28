# Multiplayer

Survival Cubed is multiplayer-first. Games run on a server — which can be the
one your own client hosts, or a separate dedicated process — and players connect
over an encrypted **QUIC** connection.

## Hosting

### From the client

Choose **Host on LAN** in the menu. Your world is advertised over your local
network so others can find and join it without typing an address. To also let
players outside your network in, tick **Forward port via UPnP (internet)** — see
[UPnP port forwarding](#upnp-port-forwarding) below.

### Dedicated server

For an always-on server, run the headless `server` subcommand:

```sh
survival-cubed server [port] [creator] [upnp]
```

| Argument | Default | Meaning |
|---|---|---|
| `port` | `5000` | Port to listen on |
| `creator` | *(off)* | If present, all players may use creator mode |
| `upnp` | *(off)* | If present, forward the port on your router via UPnP |

`creator` and `upnp` are order-independent flags, so
`survival-cubed server 5000 upnp` and `survival-cubed server 5000 creator upnp`
both work.

The server binds to all network interfaces (`0.0.0.0`) so it's reachable from
other machines. On startup it prints its address, save directory, mode, whether
UPnP is on, and a **certificate fingerprint**. Stop it cleanly with
<kbd>Ctrl</kbd>+<kbd>C</kbd> (it saves on shutdown).

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

## Under the hood

- **Transport:** QUIC over UDP, encrypted with TLS 1.3 — lower latency than TCP
  and secure by default.
- **Certificates:** each machine has a single stable self-signed certificate,
  stored in the user config dir and shared by every world it hosts; clients pin
  it on first connection or auto-trust it via LAN discovery.
- **Authority:** the server simulates the world, creatures, health, damage, and
  crafting, so results stay consistent for everyone. Your own movement is driven
  by your client and synced out to others.

There is no fixed player cap and no team/guild system — chat is global, and the
server is built to scale with the host's resources.
