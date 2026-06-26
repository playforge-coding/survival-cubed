# The Player

You are an entity like any other — but a *special* one. Your client drives your
movement and the server trusts it; the server never runs AI on you, only on
everything around you.

| Stat | Value |
|---|---|
| **Health** | 20 HP |
| **Size** | 11 × 16 px |
| **Walk speed** | 150 px/s |
| **Jump** | 440 px/s upward |
| **Fall damage** | Safe to 10 blocks, then 1 HP per extra block |

## Health & death

Your 20 HP is shown by the health bar at the top of the screen. Damage comes
from falling and from hostile creatures. Restore it by eating **cooked food**
(see [Food](../crafting.md#food)) — raw meat makes you sick.

When you die, a death marker 💀 drops where you fell and you respawn at your
**home point**. Walk back to the marker to recover what you were carrying. Your
home point starts at world spawn; right-click a **campfire** to set it as your new
respawn point. See [Gameplay & Survival](../gameplay.md#health) for the full
rules.

## Movement

You walk at 150 px/s — **faster than any monster can move on foot**, so in open
ground you can outrun almost anything. You jump, swim (slowly — you sink unless
you paddle), ride [boats](../gameplay.md#boats) across water, and climb
[ladders](../gameplay.md#ladders).

You can also **ride a [horse](horse.md)** to gallop noticeably faster than you
run — the quickest way across the open plains.

## Combat

Left-click within ~80 px to attack; each hit knocks the target back and flashes
it red. Better [weapons](../crafting.md#weapons) deal far more damage — a tungsten
sword drops a zombie in a fraction of the hits a wooden one needs. See
[Combat tips](../creatures.md#combat-tips).

In **[creator mode](../creator-mode.md)** you are invisible to monsters and take
no damage from them.
