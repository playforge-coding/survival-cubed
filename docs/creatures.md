# Creatures

The world is populated by animals you can farm or tame and monsters that hunt you
at night. Creatures spawn in a biome-appropriate mix, stay near a home area, and
are all simulated by the server (only your own player is driven by your client).

This page is the overview. Every entity also has its own page in the
**[Bestiary](entities/index.md)** with a full stat block and tactics — the names
in the tables below link to them.

## Passive animals

| Animal | HP | Where | Drops | Notes |
|---|---|---|---|---|
| **[Chicken](entities/livestock.md#chicken)** | 8 | Plains, forest, desert | 1 Raw Meat | Pecks the ground, bolts when struck |
| **[Goat](entities/livestock.md#goat)** | 16 | Mountains | 2 Raw Meat | Calm grazer |
| **[Cat](entities/pets.md#cat)** | 8 | Forest | — | Wild until tamed |
| **[Puppy](entities/pets.md#puppy)** | 14 | Forest | — | Wild until tamed; hunts skeletons and chickens |
| **[Horse](entities/horse.md)** | 30 | Plains | — | Wild until tamed **with an apple**; can be ridden |
| **[Knight](entities/knight.md)** | 40 | Plains (rare) | — | A wandering man-at-arms who **battles monsters on sight**; recruit it **with a tungsten ingot** |

Cook the raw meat from chickens and goats before eating it — raw meat makes you
sick. See [Food](crafting.md#food).

## Pets

Wild **cats** and **puppies** can be **tamed by feeding them cooked meat**. Once
tamed:

- They belong to you — you can't accidentally attack your own pet.
- They follow you, and a puppy will hunt nearby skeletons and chickens.
- **Click your pet to toggle sitting.** A sitting pet stays put and won't wander,
  hunt, or follow.
- They never wander off for good — if they get too far they teleport back to you.
- If killed, a tamed pet **respawns at your respawn point**.
- The only thing that can kill a tamed pet for good is **fire**, so keep them
  away from the underworld's flames.

## Mounts

The **horse** is a tall, peaceable grazer that roams the **plains**. It's a pet
like the cat and puppy — immune to your attacks, never lost (it teleports to you
if you stray too far), and respawning at your respawn point if it ever dies — with
two differences:

- **Tame it with an apple**, not cooked meat. Hold an [apple](crafting.md#food)
  and left-click a wild horse to befriend it. (Apples are shed now and then by
  leaves.)
- Once tamed, you can **ride it**. Walk up to your horse and **right-click** to
  climb on; **right-click again** to hop off. While mounted you gallop
  noticeably faster than you can run, and you still jump and fall normally — handy
  for crossing the open plains in a hurry.

A horse can't be ridden between dimensions: stepping through a fire key (or
falling to the underworld) drops you back on foot and leaves the horse behind in
the world you left.

## The knight

A lone **knight** wanders the **plains** — a rare sight, far scarcer than the
horses that share the grass. You **can't attack it**. Even before you recruit it,
a wild knight is no bystander: it **charges any monster** that strays near and
trades blows until one of them falls — and the monsters hunt it right back, the
same way they hunt you. A wild knight caught alone against a night swarm can be
**overwhelmed and slain**, so reach it with a tungsten ingot before the dark does.

Hold a [tungsten ingot](crafting.md#smelting) and left-click the knight to
**recruit it**, spending the ingot. A recruited knight is yours — a true companion:

- **It fights for you.** It charges the nearest monster on its own, and if you
  strike an enemy your knight makes that foe its priority — trading heavy blows
  until it falls.
- **It follows you everywhere** — even **across dimensions**. Step through a fire
  key and your knight comes too. Stray too far and it teleports to your side, like
  a pet.
- **It rides into battle.** If a **wild** (untamed) horse is near, the knight
  mounts up and gallops — and some knights are **already mounted** when you find
  them. The horse shields it — soaking every blow on the knight's behalf — until
  the horse is slain, after which the knight fights on foot. (It won't take a horse
  you've tamed.)
- **Death breaks the bond.** Unlike a pet, a slain *recruited* knight respawns at
  **your** respawn point as a *wild* knight — you must **recruit it again** with
  another tungsten ingot. A *wild* knight killed in battle is gone for good, so
  recruit a promising one before it picks a fight it can't win.

## Hostile monsters

Most monsters only **hunt at night** (or in dark caves). Build shelter and keep a
[weapon](crafting.md#weapons) handy before dusk.

| Monster | HP | Where | Behaviour |
|---|---|---|---|
| **[Slime](entities/slime.md)** | 10 | Common in mountains | Wanders by day, hunts at night |
| **[Zombie](entities/zombie.md)** | 40 | All biomes, night | Slow, tough, hard-hitting; **burns up at daybreak** |
| **[Dark Knight](entities/dark-knight.md)** | 44 | All biomes, night (overworld), rare | Black-armoured warrior; keeps its distance and throws **axes** at players *and* knights; **drops tungsten gear**; **burns up at daybreak** |
| **[Spider](entities/spider.md)** | 12 | Forests & caves | Fast and fragile; climbs walls |
| **[Snake](entities/snake.md)** | 14 | Desert | Ambusher; coils for ~0.7 s, then lunges |
| **[Skeleton](entities/skeletons.md#skeleton)** | 24 | All biomes, night | Keeps its distance and throws **bone projectiles**; burns at daybreak |
| **[Charred Skeleton](entities/skeletons.md#charred-skeleton)** | 36 | Underworld **ash valleys** | Charges into melee and **leaves a trail of fire**; active at all hours |
| **[Demon](entities/demons.md#demon)** | 28 | Underworld charred expanse (rare) | Keeps its distance and hurls **fireballs** that burst into flame; active at all hours |
| **[Orc](entities/orcs.md#orc)** | 50 | Underworld charred expanse | Slow, hulking brute; plants its feet for a telegraphed **slam** that hits brutally hard; active at all hours |
| **[Orc Mage](entities/orcs.md#orc-mage)** | 30 | Underworld charred expanse (rare) | A robed support caster; lands **no blows of its own** — it shies from you and **enchants nearby demons** instead; active at all hours |
| **[Enchanted Demon](entities/demons.md#enchanted-demon)** | 40 | Underworld charred expanse | A demon a mage has empowered: it **flies** and looses **three purple magic fireballs at once** (one straight, two fanned up and down) that reach farther and hit harder; never spawns on its own |
| **[Ash Twister](entities/ash-twister.md)** | 18 | Underworld **ash valleys** | A whirling column of ash; on contact it **flings you high into the air** — the fall does the damage; active at all hours |
| **[Necromancer](entities/necromancer.md#necromancer)** | 26 | Overworld **deserts** & underworld **ash valleys** | Hooded caster; keeps its distance and hurls **summoner fireballs** that burst into bouncing skulls; **burns up at daybreak** in the desert (roams the depths around the clock) |
| **[Skull](entities/necromancer.md#skull)** | 8 | Summoned by necromancers | A bouncing skeleton skull that hops toward you and gnashes on contact; frail and short-lived; **burns up at daybreak** in the overworld |

!!! danger "Mind the drop after an ash twister hits you"
    An **ash twister** barely scratches you on contact — but it hurls you skyward,
    and the fall back to the ground deals heavy **fall damage** when you land
    (roughly half your health from open ground, more if you come down a pit). It
    only haunts the underworld's **ash valleys**. Kill it from range or back off
    before it reaches you; if you are thrown, try to steer your landing onto a
    ledge to shorten the drop. It is frail, so a couple of solid hits put it down.

!!! danger "Skeletons and demons shoot back"
    Skeletons fire bones and demons hurl fireballs in a straight line. The
    projectile flies until it hits you or a wall — strafe behind cover, or close
    the distance fast. A demon's fireball leaves a lick of fire where it bursts,
    so don't linger where it lands. Demons are rarer than charred skeletons, but
    far deadlier at range.

!!! danger "Don't stand under an orc's slam"
    An orc is slow and easy to outrun, but once it plants its feet and rears back
    it is winding up a **slam** that can gut you in two blows. The hit only lands
    as its fists crash down, so step out of reach during the wind-up — then close
    in and strike while it recovers.

!!! danger "Kill the orc mage first"
    An **orc mage** never attacks you directly — it hangs back and **enchants the
    demons around it**, turning each into a flying **enchanted demon** that chases
    you through the air and pelts you with **purple fireballs** that reach farther
    and hit harder than ordinary ones — and it fires them **three at a time**, in a
    fan you can't simply duck under. A mage heals a demon to full as it empowers
    it, so a worn-down demon suddenly comes back stronger. If you see a mage, run
    it down before it builds an air force — but watch its enchanted demons, which can
    dive on you from any angle. Their magic fireballs still leave only ordinary fire
    where they burst.

!!! danger "Necromancers raise a tide of skulls"
    A **necromancer** never closes in — it kites you and lobs **summoner fireballs**,
    each of which bursts into a bouncing **skull** wherever it lands. The skulls hop
    after you and gnash on contact, so a necromancer left alone quickly buries you
    under them. Rush it down (it's frail) before the floor fills with skulls; the
    skulls themselves pop in a hit or two and give out on their own after a while.
    Necromancers and their skulls **burn up at daybreak** in the desert and overworld —
    but the **underworld is eternal night**, so there (in the ash valleys) they never
    burn, and neither do zombies or skeletons that wander down.

Monsters are hostile to **[knights](#the-knight)** as well as to you — they will
chase down and attack a man-at-arms whether or not you have recruited it, and a
knight gives as good as it gets. Let a knight soak the blows while you strike from
behind it.

In **creator mode** you are invisible to monsters and take no damage from them.

## Combat tips

- Left-click within ~80 pixels to attack; each hit knocks the target back and
  flashes it red.
- Better [weapons](crafting.md#weapons) deal far more damage — a tungsten sword
  drops a zombie in a fraction of the hits a wooden one needs.
- Zombies and skeletons self-destruct at dawn, so sometimes the smart move is
  simply to survive the night behind a wall.
- A tamed puppy is a genuine ally against skeletons.

## Other entities

- **Bone** — the projectile a skeleton throws; it disappears on impact or after a
  short flight.
- **Axe** — the projectile a dark knight throws (at players and knights alike); it
  tumbles end over end and disappears on impact or after a short flight.
- **Fireball** — the projectile a demon hurls; it disappears on impact or after a
  short flight, leaving a tongue of fire where it bursts.
- **Magic fireball** — the purple bolt an enchanted demon hurls; it flies farther
  and hits harder than an ordinary fireball, but likewise bursts into a tongue of
  fire where it lands.
- **Summoner fireball** — the bolt a necromancer hurls; it deals light damage on a
  direct hit but, instead of fire, **bursts into a bouncing skull** wherever it lands.
- **Dropped items** — block and tool stacks lying on the ground (from mining,
  drops, or the <kbd>Q</kbd> key). Walk into them to pick them up; dropped tools
  keep their durability.
