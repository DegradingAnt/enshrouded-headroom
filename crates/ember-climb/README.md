# ember-climb

Free climbing for Enshrouded, in the style of Breath of the Wild. Climb any steep rock, cliff or wall. There is no button. A climb starts when you walk, sprint or jump into a steep surface. The mod uses the game's own climb moves, stamina and collision.

**This is a first test build for Patch 1.0.1.0 (internal build 1283531).** It has never run in the game. The geometry is tested in simulated worlds, and the hooks are tested against fakes. Nobody knows yet how it feels. The diagnostics line is on so that one play session shows which step fails.

## Install

The mod needs [Ultimate ASI Loader](https://github.com/ThirteenAG/Ultimate-ASI-Loader) as `dinput8.dll` in the game folder.

1. **Disable ClimbAnywhere first.** The two must never run together. Back up and rename `climb-anywhere.asi` and any `dinput8.dll` from `plugins\`, and `dinput8Hooked.dll` from the game folder. Keep its settings and log in the backup so the previous installation can be restored. If you also used its server build, see "On a dedicated server" below.
2. Put `ember-climb.asi` and `ember-climb.ini` in `plugins\` in the game folder.
3. Start the game. `plugins\ember-climb.log` shows what happened.

**No `plugins\ember-climb.log` after a game start?** The loader did not load the plugin. Check that `dinput8.dll` in the game folder is Ultimate ASI Loader. Other mods ship a file with the same name.

ember-climb refuses to patch while it finds a conflicting installed binary name in the standard loader folders or sees ClimbAnywhere loaded, and the log says why. It goes by the names of loaded modules: anything with "climb anywhere" in its name in any spelling, `dinput8Hooked.dll`, or a `dinput8.dll` inside `plugins\`. If another mod uses one of those names, the log names the file. It looks again ten seconds after start-up, because a plugin can load after this one. It only reads directory and module names. It never opens another mod's binary. An unreadable module list refuses startup. The late check disables custom climbing, but the startup code patches remain until exit; restart after resolving a late conflict. Arbitrary hot-loading of another climbing mod is unsupported.

To remove the mod, delete those two files and the log.

### On a dedicated server

The server decides where players are, so the mod must run there for its players to climb. **It has never run on a server yet.** See "Server" below for what was checked.

1. **Disable ClimbAnywhere first.** Its server build is itself a `dinput8.dll` next to `enshrouded_server.exe`, with `climb-anywhere.ini` and `climb-anywhere.log`. Back up all three outside the server folder before replacing the proxy. While that file stays there, Ultimate ASI Loader cannot take its place, so this mod does not load.
2. Put Ultimate ASI Loader as `dinput8.dll` next to `enshrouded_server.exe`.
3. Put `ember-climb.asi` and `ember-climb.ini` in `plugins\` in the server folder.
4. **Every player who climbs needs the mod too**, with the same values in `ember-climb.ini`. Other values on the two sides would pull a climber back and forth. This is the expected behaviour, and it has not been tested. A player who only watches does not need the mod. See "Who needs the mod" below.

Under Wine or Proton, the server also needs `WINEDLLOVERRIDES="dinput8=n,b"`, or Wine loads its own `dinput8`.

## What you should see

The log starts like this on a supported game:

```text
ember-climb 0.1.0 loaded
settings: <game folder>\plugins\ember-climb.ini
build 1283531-steam
all 15 signatures matched once
wrote the wall-lost return at exe+0x24f4e5, the attachment store at exe+0x41fd08
hooks: 10 enabled
climbing is on: surface mask 0x124, plane tolerance 0.45 m, walkable below 50 degrees, stamina x1, a climb starts by walk, jump, sprint, diagnostics on
```

If the mod stays off, the log says why in one line:

| Line | Meaning |
|---|---|
| `unknown build <hash>: nothing patched` | The game was updated. The mod needs a new release. |
| `climbing is off: 1 of 15 signatures did not match exactly once: <name>: ...` | The game's code differs. Nothing was changed. |
| `ember-climb is off: <path> is loaded. ...` | ClimbAnywhere is still installed. |
| `Enabled = 0 in ember-climb.ini: the mod does nothing` | You turned it off. |

## Reading the diagnostics

While a climb starts, runs or ends, the log gets one line a second, like this (shortened):

```text
climb diag: probe ok 0, 0x15 0, 0x1e 0, 0x1f 412 ... | natural entry 3/90, move 180/190, stay 200/200 (refused dir 0, hits 12, bumpy 8, walkable 0, overhang 0, far 67, blocked 0; last hits 4 spread 0.12 ny 0.21 gap +0.02) | ... | lost 0/200 (voxel 200, grace 0, no probe 0) | steer 180 | ... | updates 240/s
```

If a surface was near and no climb started, the log gets one line at most every 30 seconds. A start that failed still leaves a trace, and a walk along a cliff does not fill the log.

The counts add up over the session. `natural entry 3/90` means 90 tries to start a climb on natural rock and 3 that could start. Only `updates` is per second.

About a second into each climb that moves, the log gets one more line, once per climb:

```text
climb body: entity 1, rotation speed 8.00 mode 1 (ExponentialLerp), normal -0.940 0.342 0.000, body up 0.342 0.940 0.000, surface leans 20.0 degrees, body leans 20.0 degrees
```

This line settles whether the body really tilts. Climb a face near 70 degrees and compare the last two numbers. If `body leans` follows `surface leans`, the body tilts. If it stays near 0, the body stays upright. Until a test shows that, the idea that the body lies along the surface is a hypothesis. Under an overhang `surface leans` goes negative and `body leans` stays positive, so compare their sizes there.

| Part | What it tells you |
|---|---|
| `probe ok` | The game's own probe passed: a lattice. |
| `natural entry`, `move`, `stay` | The mod's surface fit, for starting, moving and holding. |
| `refused` | Why a surface was refused: `hits` too few rays hit, `bumpy` not one surface, `walkable` too flat, `overhang` hangs over too far, `far` out of reach, `blocked` something solid stands in the way. |
| `last hits`, `spread`, `ny`, `gap` | The last fit: rays that hit, unevenness in metres, how much the surface faces up, and how far the player is past the wanted distance. |
| `found` | How the player was moving when a surface was found. `off` counts starts that a setting refused. |
| `low` | The game's start check: `angle` the stick was too far off, `cone` the mod's own test let it through. |
| `lost` | Hold checks that dropped the player, out of all checks. `voxel` held on terrain, `grace` forgiven for a moment. |
| `exit` | How climbs ended. `refused` stopped a mantle onto steep rock, and `taken` found a way out the game did not. |
| `capsule` | The player's real collision size. |
| `updates` | Runs of the climb update in the last second, for all climbers. One climber at 60 ticks a second gives 60. A client replays old ticks, so more than that shows how long its replays are: 240 means each tick ran four times. |

Keep the log after a test, even a bad one. Without it nobody can tell which step failed.

## Settings

All settings are in `ember-climb.ini`, and all need a restart. A value that is not valid warns in the log and uses its default.

| Section | Key | Default | Range | Effect |
|---|---|---|---|---|
| Climb | Enabled | 1 | 0 or 1 | 0 leaves the game untouched. |
| Climb | SurfaceMask | 0x124 | see below | Which colliders can be climbed. |
| Climb | PlaneTolerance | 0.45 | 0.05 to 1 | How uneven a patch of surface may be to start a climb, in metres. |
| Climb | StaminaMultiplier | 1 | 0 to 10 | Climbing stamina cost, as a multiple of the game's. 1 hooks nothing. |
| Start | Walk | 1 | 0 or 1 | Walking into a surface starts a climb. |
| Start | Jump | 1 | 0 or 1 | Jumping or falling into one starts a climb. |
| Start | Sprint | 1 | 0 or 1 | Sprinting into one starts a climb. |
| Start | Glide | 0 | 0 or 1 | Gliding into one starts a climb. Experimental, see below. |
| Tuning | WalkableAngle | 50 | 30 to 75 | Surfaces less steep than this, in degrees, can be walked on. They start no climb, and a climb can end on them. |
| Tuning | OverhangAngle | 110 | 90 to 135 | The farthest a face may hang over to start a climb, in degrees. 90 is a sheer wall. A running climb gets 15 degrees more, so that one rough patch does not stop it. |
| Tuning | EntryReach | 0.95 | 0.6 to 1.4 | How close you must be to start a climb, in metres. |
| Tuning | HoldReach | 1 | 0.8 to 1.6 | How far the mod looks for the surface during a climb, in metres. Under an overhang you hang farther out, and it looks that much farther. |
| Tuning | GraceSeconds | 0 | 0 to 1 | Reserved. Native seam forgiveness is unavailable; nonzero values warn and remain inactive. |
| Debug | Diagnostics | 1 | 0 or 1 | The one-line-a-second log. It also adds five small hooks. |

`SurfaceMask` is a sum of these bits, in hex or decimal: `0x4` terrain and player walls, `0x20` rock props, `0x100` iron lattices, `0x8` other solid props such as trees. Other bits are refused. Bits 0 and 1 are other copies of the terrain, and asking for them gives a different surface per ray.

## Controller input

Movement uses the game's own processed input, including an analogue stick. No keyboard emulation or extra grab button is added. Existing PlayStation controller support through the game or Steam Input stays in use. Controller feel and jump-off still need the first play test.

## Gliding

`Glide = 1` is off by default because three things about it are unproven: whether a climb can replace the glider sequence, whether the glider state ends cleanly, and whether the game gives move input while gliding. It also changes how the game requests every climb, lattices too. Turn it on to test it, and keep the log.

## What it does

The game already climbs, but only on flat iron lattices. Its wall probe wants four rays with one exact normal. Rock never gives that.

- The game's probe always runs first, so its successful lattice answer is kept. No mask or tolerance in the game is changed. The attachment patch skips only the synthetic voxel-hold marker; ordinary IDs, including high-bit IDs, use the native accessor/store. That store also runs when the ID is unchanged.
- When the probe fails, the mod casts four rays of its own at the surface, averages what they hit, and asks whether it is steep and flat enough.
- The game moves a climber along world up, which walks straight off any face that leans. The mod turns the movement into the surface, keeps the speed, and holds the game's 0.55 m distance. If a confirmed hold is beyond normal movement reach, a fresh probe can use the existing hold margin while moving closer to it. This avoids a permanent hold/move deadlock; it adds no timer or permission to move away.
- Automatic climb-up animations on natural rock and walls are disabled in this test build. The current native head-sphere check does not prove that the whole character or animation path is clear. Use the game's jump-off to leave the surface. Ordinary iron lattices keep their native exits. The proposed area-based top-out model remains in inert tests for later integration with a proven body/path guard.
- A half-pressed stick fails the game's start test however straight you run at a wall. The mod tests only the heading.
- The persistent seam-forgiveness model is tested on initialized simulated state, but its native binding stays unavailable. This build never reads or writes the game's spare grace byte. The hold and movement fits still use their longer reach and relaxed surface checks.

Evidence for every game fact is in [docs/re-notes.md](../../docs/re-notes.md), "Free climbing".

## First play test

Start with a private offline world. Check a tall vertical wall, a leaning rock face, sideways movement, jump-off near a flat top, stamina, and an ordinary iron lattice. Natural rock must not trigger an automatic climb-up animation; leave it with the normal Jump control. Lattices should still top out normally. Repeat with the controller. Keep `ember-climb.log`, especially if a grab fails, a hold drops, or the character tries to mantle too soon. Failed entry attempts are logged at most once every thirty seconds; idle checks stay quiet.

The client and server use the same plugin and settings. Test multiplayer only after the offline session works. This package is a candidate; native-file checks and simulated climbs do not establish runtime reliability.

## Limits

- Automatic natural-wall top-outs are unavailable. This includes a valid-looking flat top; use Jump to leave the wall. Whole-body landing and animation-path checks must be proven before this feature can be enabled. The staged prevention does not establish the cause of a different mod's reported terrain/spawn recovery.
- Seam forgiveness is unavailable. A complete loss of the hold rays drops the climber without a timed grace period. The pure proposed grace model needs proven initialization and lifecycle ownership before a later build can enable it.

- A surface must reach 2 m above your feet to start a climb. A lower wall starts nothing, and neither does a ledge you jump at when it is less than 2 m above your feet at that moment. Use the game's jump there. This may change after the first play test.
- Rough or short shelves can still stop upward movement; the native controller/collision test remains authoritative. The former 1.2 m hold/move reach mismatch is corrected, including the clean 0.7–0.8 m bench cases in the model. Further geometry limits need a play test.
- Earlier model observations before the reach correction: a short walkable shelf in a face can stop you while the stick is up. In the simulation a flat shelf of 0.2 to 0.8 m in a 60 degree face does this, and so does one of 0.5 to 0.9 m in a sheer wall. On the 60 degree face a shelf of 1 m or more ends in a mantle. Go sideways or go down, or jump off.
- A long slope close to `WalkableAngle` does not end the climb. With the default of 50 that is a slope of 47 to 50 degrees. You can walk on it, but the mod reads it as the face going on, so no mantle comes. Go sideways, go down or jump off.

## Server

The mod admits the current `1283531-steam` client and `1283531-server` dedicated server, plus the previous `f68140a3-steam` and `d7baa768-server` builds. All 15 signatures match exactly once in all four executable images. The complete directly used function groups, layouts, patch guards and scheduling contracts were compared across the update; their bounded evidence is in the W52 section of `docs/re-notes.md`. **It has never run on a server**, so how it behaves there and how a modded server looks to a client are not known. If a signature does not match exactly once, the mod logs which one and installs nothing. To check an exe again:

```text
set ENSHROUDED_SERVER_EXE=<path to enshrouded_server.exe>
cargo test -p ember-climb --test real_exe -- --ignored
```

### Who needs the mod

The server decides from its own probe. The client sends only its stick and buttons. So:

- **The server needs the mod**, and so does **every player who climbs**. The client predicts its own player, and it needs the same probe to agree with the server.
- **A player who only watches does not need the mod.** A player is predicted only on that player's own client. A watcher gets the position and the animation from the server.
- **A client with the mod on a vanilla server** gets a stuck start and a snap back at natural walls. This is a hypothesis until it is tested. Lattices stay vanilla.

## Build and test

```text
cargo asi-climb
cargo test -p ember-climb
cargo test -p ember-climb --test real_exe -- --ignored
```

`cargo asi-climb` writes `target/release/ember-climb.asi`. Copy `ember-climb.default.ini` as `ember-climb.ini`, or let the mod create it on first start.

The ignored tests read the real exes, and they fail if a file is missing. They set `ENSHROUDED_STEAM_EXE` and `ENSHROUDED_SERVER_EXE`, or use the default Steam folders. They never start the game.

## Licence

MIT.
