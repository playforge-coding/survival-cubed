# Magic & Spellbooks

Magic is the world's newest art: **spellbooks** found out in the world, cast by
spending **mana** you bank from slaying monsters. The first spellbook is the
**summoner spell** — the necromancer's own trick, turned to your side.

## Mana

Mana is your magic resource, shown as a blue **✦** bar beside your health on the
HUD. You bank it by **killing monsters** — tougher, rarer foes are worth more —
up to a cap of **100**. It is spent casting spellbooks and persists across
disconnects and restarts.

| Monster | Mana | Monster | Mana |
|---|---:|---|---:|
| Skull | 1 | Demon | 9 |
| Slime | 2 | Necromancer | 11 |
| Spider | 3 | Orc mage | 11 |
| Snake | 4 | Enchanted demon | 12 |
| Skeleton | 5 | Orc | 13 |
| Ash twister | 6 | Dark knight | 16 |
| Zombie | 7 | Dragon | 60 |
| Charred skeleton | 9 | Demon king | 100 |

Peaceable animals (chickens, goats), companions (cats, puppies, horses, knights)
and your own summons give **no** mana — only the slaying of *monsters* does.

!!! note "Mana is won by your own hand"
    Mana is banked when **you** land the killing blow. A monster felled by a knight,
    a pet, or your own summoned skull spills its loot but pays no mana.

## Casting

Hold a spellbook on your hotbar, aim with the mouse, and **right-click to cast**
toward the cursor. The book is a reusable artifact — casting never consumes it,
only mana. If you can't afford the spell, nothing happens.

## The summoner spell

| Stat | Value |
|---|---|
| **Source** | Rarely dropped by a slain **[necromancer](entities/necromancer.md)** (~1 in 12) |
| **Mana cost** | 25 per cast |
| **Effect** | Looses a friendly summoner fireball that bursts into a friendly skull |

Casting looses a **friendly summoner fireball** that flies toward your cursor
(130 px/s, ~2.5 s) and, where it bursts — on a wall, on a monster, or when its
life runs out — conjures a **friendly skull**: the same bouncing skull a
necromancer summons, but it fights **for you**.

The friendly skull hops toward the nearest **monster** and gnashes at it (5 HP a
bite), lasting about **14 seconds** before it gives out. Unlike the necromancer's
undead skull, it does **not** burn up at daybreak — it is your magic, not a
creature of the night. It is drawn in a cool blue glamour so it reads as yours.

!!! tip "Knights and your summons leave each other be"
    A friendly skull is never hostile, so a **[knight](entities/knight.md)** will
    not attack it, nor it the knight — they fight side by side. Monsters pay your
    summons no mind either, so a skull can pelt a foe freely while you press the
    attack.

## The sunburst spell

| Stat | Value |
|---|---|
| **Source** | The loot **chest** inside a **[ruin](world.md)** |
| **Mana cost** | 50 per cast |
| **Effect** | Instantly slays every daylight-burning undead within 10 blocks |

A panic button against the undead. Casting unleashes a burst of sunlight centered
on you that **instantly kills every creature that burns in daylight** within a
**10-block** radius — **[zombies](entities/zombie.md)**,
**[skeletons](entities/skeletons.md)**, **[dark knights](entities/dark-knight.md)**,
**[necromancers](entities/necromancer.md)** and their **skulls**. It works at any
hour and in **any dimension** (the burst supplies its own sunlight, so it clears
the always-dark depths too).

It spares everything that doesn't fear the sun — slimes, spiders, snakes, demons,
orcs, dragons, the demon king — and it never harms your own friendly summons,
animals, pets, or knights. There is no projectile and no aiming: the blast simply
radiates from where you stand.

!!! warning "No spoils from the sun"
    Foes burned away by a sunburst leave **no loot and grant no mana** — the spell
    is an escape hatch and a crowd-clearer, not a way to farm. Bank the mana for it
    by fighting the *other* monsters by hand.

## The restore spell

| Stat | Value |
|---|---|
| **Source** | The loot **chest** the **[demon king](entities/demon-king.md)** leaves |
| **Mana cost** | 60 per cast |
| **Effect** | Turns the creature under your cursor into an ally (or a calmer form) |

The deepest magic, won only by felling the boss. Aim at a creature and cast to
**restore** it:

| Cast on… | Becomes… |
|---|---|
| An **[orc](entities/orcs.md)** | A **[knight](entities/knight.md)**, recruited to you |
| A **[dark knight](entities/dark-knight.md)** | A **[knight](entities/knight.md)**, recruited to you |
| An **[orc mage](entities/orcs.md)** | A **[mage](entities/mage.md)**, recruited to you |
| An **enchanted [demon](entities/demons.md)** | An ordinary (ground-walking) demon |

A knight or mage you restore is **recruited** — it serves you just like a knight
recruited with a tungsten ingot, following you everywhere (even across dimensions).
Casting on anything else does nothing, and a wasted cast **refunds its mana**, so
you only ever pay for a restore that lands.

!!! tip "Mages return the favor"
    A restored **[mage](entities/mage.md)** casts the world's spells of its own
    accord — the summoner, sunburst, and restore spells — to aid you. When *it*
    restores a foe, the new knight or mage is recruited **on your behalf**, so a
    single mage can build you a whole retinue.

## The dragonian steed spell

| Stat | Value |
|---|---|
| **Source** | The loot **chest** the **[demon king](entities/demon-king.md)** leaves |
| **Mana cost** | 80 per cast |
| **Effect** | Summons a friendly white dragon that fights for you and can be ridden |

The grandest magic in the world, won only by felling the boss — far too potent to
leave to a mere miniboss's spoils. Casting summons a friendly **white dragon** at
your side — a peaceable twin of the [dragon](entities/dragon.md), of the same stock
the demon king once kept as steeds. The summoned steed:

- **Never despawns.** Like a pet it stays with you forever, teleporting to your side
  if you stray too far and crossing **dimensions** with you.
- **Fights for you.** Left to its own wings it soars after you and breathes
  **friendly fireballs** at nearby monsters, scorching them (and leaving a lick of
  fire) without ever harming you.
- **Can be ridden.** Right-click your steed to **mount** it and **fly** — rise on
  **jump**, descend on **down**, and steer with left/right. While riding, hold **B**
  to **breathe a fireball at your cursor** on the steed's cadence. Right-click again
  to dismount.

Unlike a pet, a fallen steed does **not** come back on its own: only **recasting the
spell** raises a new one. Casting again also replaces a steed you already have, so a
lost or distant dragon is always one cast away — and you never accumulate more than
one at a time.

!!! tip "The king's deepest secrets, side by side"
    The demon king's chest holds **both** of the world's greatest spellbooks — the
    [restore spell](#the-restore-spell) and the dragonian steed — so a single, hard-won
    victory in the arena arms you with the two rarest magics at once.

## Lore

*To be written.*
