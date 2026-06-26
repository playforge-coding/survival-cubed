# Gameplay & Survival

## Health

You have **20 HP**, shown as the health bar in the top of the screen. Damage
comes from falling and from hostile creatures. Restore health by eating cooked
food (see [Crafting & Tools](crafting.md#food)).

Wearing a suit of **[armor](crafting.md#armor)** raises your **defense**: it
blunts a percentage of every enemy attack before it reaches your health. It does
not raise your maximum HP — it just makes each blow hurt less. A hit always lands
for at least 1, so armor softens fights without making you invincible.

When you die, a death marker 💀 is dropped where you fell and you respawn at your
**home point**. Walk back to the marker to clear it.

### Respawning

Your home point starts at the world spawn. Open a **campfire** (right-click it) to
set that campfire as your new respawn point. After dying you reappear there.

## Fall damage

Falls are safe up to **10 blocks**. Beyond that you take **1 HP per extra block**:

- A 10-block fall: no damage.
- A 12-block fall: 2 HP.

Entering water or grabbing a ladder resets your fall, so you can break a long
drop by landing in water or catching a ladder on the way down.

## Swimming

When your body overlaps water you start swimming:

- Gravity is reduced, but you're denser than water and **sink steadily** unless you
  paddle — you can't just hold a direction and skim across the surface.
- <kbd>Space</kbd>/<kbd>W</kbd> paddles you upward; <kbd>S</kbd> dives.
- Horizontal movement is dragged hard, so swimming is **much slower than walking**.
  Crossing a wide lake on foot is a slow, sinking slog — build a [boat](#boats).
- Water breaks falls — you take no fall damage when you splash down.

Water is otherwise inert: it doesn't flow or spread. You can pick it up and place
it with a [bucket](crafting.md#tools-and-utility-items).

## Boats

Craft a [boat](crafting.md) from wood and sticks to travel across water properly:

- **Right-click** while holding the boat to climb aboard; right-click again to
  step back out. The boat stays in your inventory — it's a vehicle, not used up.
- While riding, you **sit on the water surface** and glide across at full walking
  speed (instead of sinking and crawling along as a swimmer does). Drop into deep
  water and the boat bobs back up to the top.
- Steer with <kbd>A</kbd>/<kbd>D</kbd>. A boat floats only on water; carried onto
  dry land it just walks like normal.
- Lose the boat (drop or trade it away) and you automatically step out.

## Ladders

Ladders and rope ladders let you climb vertically:

- Climb up with <kbd>Space</kbd>/<kbd>W</kbd> and down with <kbd>S</kbd>.
- Gravity is suspended while you're on a ladder.
- You can't mine while climbing — step off or descend first.

Rope ladders unroll downward when placed, making them handy for descending into
caves.

## The day/night cycle

A full day lasts **20 minutes** of real time and flows smoothly between four
points:

| Time | What's happening |
|---|---|
| Sunrise | Day begins |
| Noon | Brightest point |
| Sunset | Day ends |
| Midnight | Darkest point |

The sky tints from bright blue at noon toward near-black at midnight, but it
**never goes fully dark** — you can always see a little. Once brightness drops
below roughly dusk level it counts as **night**, and that's when hostile
creatures start hunting.

!!! warning "Night is dangerous"
    Zombies, skeletons, spiders, snakes, and slimes turn hostile at night.
    Zombies and skeletons burn up at daybreak, but until then they roam, and
    skeletons fire bone projectiles from a distance. Build shelter, keep a
    weapon ready, and a lit campfire nearby. See **[Creatures](creatures.md)**.

The **underworld** has no day/night cycle — it sits in a permanent dim, warm
glow, and its creatures are active at all hours.

## Combat

Left-click a creature within reach to attack it. Each swing has a short cooldown,
deals damage based on your equipped weapon, and knocks the target back; struck
creatures flash red briefly. Your weapon choice matters — see the
[weapon tiers](crafting.md#weapons). Without a weapon you can still punch, but
tools and swords hit much harder.

On defense, a worn suit of **[armor](crafting.md#armor)** turns aside a share of
the damage hostile creatures deal you — iron blunts 35%, tungsten 55%.

## Game modes

### Survival (default)

Resources are limited: you gather, craft, and manage durability and health.
Hostile mobs can hurt you. Only the host/admin can switch to creator mode here.

### Creator mode

When enabled by the server, creator mode grants flight, infinite free blocks,
time control, creature spawning, item-giving, and the structure tool — and
monsters ignore you entirely. See **[Creator Mode](creator-mode.md)** for the
full toolset and how to enable it.
