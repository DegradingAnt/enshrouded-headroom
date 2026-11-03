# ember-climb

Free climbing for Enshrouded, in the style of Breath of the Wild. This experimental build grabs automatically when you walk, sprint or jump into a steep rock, cliff or wall. Deliberate hold-to-grab and a separate optional climb binding are requested follow-ups; they are not implemented yet. The mod uses the game's own climb moves, stamina and collision.

**This is an experimental build for Patch 1.0.1.0 (internal build 1283531). Its first offline play test failed acceptance.** Grabs and climbing started, but the climber fell and was pushed away after a few metres. The cause is under investigation. A later offline test also found overly easy grabs beside a wall, intermittent perpendicular corner transfers and inaccessible overhead ledges. Natural top-outs are deliberately disabled. The previous installation was restored after both tests. Keep this build out of a shared client/server setup until offline acceptance passes. Fixture, native-file and startup checks do not establish gameplay reliability.

## Install

The mod needs [Ultimate ASI Loader](https://github.com/ThirteenAG/Ultimate-ASI-Loader) as `dinput8.dll` in the game folder.

1. **Disable ClimbAnywhere first.** The two must never run together. Back up and rename `climb-anywhere.asi` and any `dinput8.dll` from `plugins\`, and `dinput8Hooked.dll` from the game folder. Keep its settings and log in the backup so the previous installation can be restored. If you also used its server build, see "On a dedicated server" below.
2. Put `ember-climb.asi` and `ember-climb.ini` in `plugins\` in the game folder.
3. Start the game. `plugins\ember-climb.log` shows what happened.

**No `plugins\ember-climb.log` after a game start?** The loader did not load the plugin. Check that `dinput8.dll` in the game folder is Ultimate ASI Loader. Other mods ship a file with the same name.

ember-climb refuses to patch while it finds a conflicting installed binary name in the standard loader folders or sees ClimbAnywhere loaded, and the log says why. It goes by the names of loaded modules: anything with "climb anywhere" in its name in any spelling, `dinput8Hooked.dll`, or a `dinput8.dll` inside `plugins\`. If another mod uses one of those names, the log names the file. It looks again ten seconds after start-up, because a plugin can load after this one. It only reads directory and module names. It never opens another mod's binary. An unreadable module list refuses startup. The late check disables custom climbing, but the startup code patches, including the hanging-stamina floor, remain until exit; restart after resolving a late conflict. Arbitrary hot-loading of another climbing mod is unsupported.

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
wrote the wall-lost return at exe+0x24f4e5, the attachment store at exe+0x41fd08, the wall-climb effort at exe+0x2f0789
hooks: 13 enabled
native Jump observations are on: three read-only hooks; correlation candidates only
climbing is on: surface mask 0x124, plane tolerance 0.45 m, walkable below 50 degrees, stamina x1, idle effort 0.5 m/s, a climb starts by walk, jump, sprint, diagnostics on
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

With diagnostics on, the existing log thread also writes at most one latest `climb move:` record per second. One invocation supplies its feet, fresh fit, raw velocity, steered velocity, native check output, duration, contact flag and native AL. The line distinguishes missing and non-finite fits. `check output V` is captured before the native caller zeros a rejected velocity; it is not measured post-collision movement. Busy records are dropped without waiting. These observations never make movement decisions.

Diagnostics also records native Jump acceptance and the original calculated Jump delta. The existing log thread drains up to 32 copied records per second, labelled `climb jump correlation candidate:`. `Accepted` includes the unchanged return byte, final request type/time and upward-input bits. `CalculatedDelta` includes the native error byte and, only on success, the calculated float3 bits. This is before native application and collision. It is not measured displacement or a completed upward climbing hop.

Checks with zero native return and no matching publication accessor are omitted and counted separately before native identity reads. This does not infer idle input or a refusal reason. Accepted results without an accessor, attempted publications and scope/missing-field errors remain observable. The queue keeps at most 32 records. Busy/full queues drop new records without waiting; a separate line reports losses. Missing identity/input and repeated replay keys remain ambiguous. These records cannot establish a unique paid action or justify a stamina charge. The hooks keep no native pointer after its owner returns, add no accessor call and leave native arguments/results unchanged. `Diagnostics = 0` skips all three new targets and their hooks.

About a second into each climb that moves, the log gets one body line, once per climb:

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
| Climb | StaminaMultiplier | 1 | 0 to 10 | Multiplier on the native climbing cost, including the minimum hanging effort. 1 keeps the base cost per metre. |
| Start | Walk | 1 | 0 or 1 | Walking into a surface starts a climb. |
| Start | Jump | 1 | 0 or 1 | Jumping or falling into one starts a climb. |
| Start | Sprint | 1 | 0 or 1 | Sprinting into one starts a climb. |
| Start | Glide | 0 | 0 or 1 | Gliding into one starts a climb. Experimental, see below. |
| Tuning | WalkableAngle | 50 | 30 to 75 | Surfaces less steep than this, in degrees, can be walked on. They start no climb, and a climb can end on them. |
| Tuning | OverhangAngle | 110 | 90 to 160 | The farthest a face may hang over to start a climb, in degrees. 90 is a sheer wall. A running climb gets up to 15 degrees more, capped at 160. Values above 135 are experimental; native body clearance and gameplay acceptance remain pending. |
| Tuning | EntryReach | 0.95 | 0.6 to 1.4 | How close you must be to start a climb, in metres. |
| Tuning | HoldReach | 1 | 0.8 to 1.6 | How far the mod looks for the surface during a climb, in metres. Under an overhang you hang farther out, and it looks that much farther. |
| Tuning | GraceSeconds | 0 | 0 to 1 | Reserved. Native seam forgiveness is unavailable; nonzero values warn and remain inactive. |
| Debug | Diagnostics | 1 | 0 or 1 | Per-second counters, paired movement records and native Jump observations. It adds eight diagnostic hooks. |

`SurfaceMask` is a sum of these bits, in hex or decimal: `0x4` terrain and player walls, `0x20` rock props, `0x100` iron lattices, `0x8` other solid props such as trees. Other bits are refused. Bits 0 and 1 are other copies of the terrain, and asking for them gives a different surface per ray.


## Stamina

Stationary clinging now has a minimum effort equivalent to moving at 0.5 m/s. At the unmodified native cost of 8 points/metre, that is 4 stamina points/second before the game's food, equipment, skill and cost modifiers. Slow movement pays at least this effort. Normal movement above the minimum keeps its existing charge, without an extra hanging fee. The mod does not increase movement speed or replace the stamina bar.

This uses the game's simulation duration and its existing depletion accumulator, fractional carry, exhaustion and recharge rules. It does not write the movement component or current stamina directly. The same rule applies to ordinary wall lattices; ladders and other stamina programs stay unchanged. Normal stop/cooldown clears the native wall-climb state, so it no longer pays the hanging cost. A possible native exit-cleanup scheduling edge is not separately overridden.

The inline calculation needs SSE4.1. Startup refuses every climbing patch on a CPU without it. The whole native calculation is checked against the four admitted executable images before any write. Gameplay balance and client/server stamina agreement still need the first test.

## Controller input

Movement uses the game's own processed input, including an analogue stick. No keyboard emulation or extra grab button is added. Existing PlayStation controller support through the game or Steam Input stays in use. Controller feel and jump-off still need the first play test.

## Gliding

`Glide = 1` is off by default because three things about it are unproven: whether a climb can replace the glider sequence, whether the glider state ends cleanly, and whether the game gives move input while gliding. It also changes how the game requests every climb, lattices too. Turn it on to test it, and keep the log.

## What it does

The game already climbs, but only on flat iron lattices. Its wall probe wants four rays with one exact normal. Rock never gives that.

- The game's probe always runs first, so its successful lattice answer is kept. No mask or tolerance in the game is changed. The attachment patch skips only the synthetic voxel-hold marker; ordinary IDs, including high-bit IDs, use the native accessor/store. That store also runs when the ID is unchanged.
- When the probe fails, the mod casts four rays of its own at the surface, averages what they hit, and asks whether it is steep and flat enough.
- The game moves a climber along world up, which walks straight off any face that leans. The mod turns the movement into the surface, keeps the speed, and holds the game's 0.55 m distance. Physical distance uses the fresh fitted plane while body direction eases separately; a changing ray-hit centroid must not invent an inward or outward pull. If a confirmed hold is beyond normal movement reach, a fresh probe can use the existing hold margin while moving closer to it. This avoids a permanent hold/move deadlock; it adds no timer or permission to move away.
- Automatic climb-up animations on natural rock and walls are disabled in this test build. The current native head-sphere check does not prove that the whole character or animation path is clear. Use the game's jump-off to leave the surface. Ordinary iron lattices keep their native exits. The proposed area-based top-out model remains in inert tests for later integration with a proven body/path guard.
- A half-pressed stick fails the game's start test however straight you run at a wall. The mod tests only the heading.
- The persistent seam-forgiveness model is tested on initialized simulated state, but its native binding stays unavailable. This build never reads or writes the game's spare grace byte. The hold and movement fits still use their longer reach and relaxed surface checks.

Evidence for every game fact is in [docs/re-notes.md](../../docs/re-notes.md), "Free climbing".

Corner transfers use one shared limit of 36 actual custom raycasts per native move, including prechecks, failed discovery and full/vertical/horizontal candidates. A denied ray refuses the whole candidate, even if three earlier rays hit. The current-frame hints are keyed to the exact context, entity, world and feet. Only owned scalar state enters the lexical move scope; no borrowed frame pointer or cross-frame corner cache is retained. Nested moves forward natively and restore the outer custom state. Native successful probe answers remain unchanged. These are source/fixture properties, not measured native physics or multiplayer acceptance.

The [balance and animation research](DESIGN.md) records the hanging-stamina and IK work. The minimum hanging effort is implemented in this test candidate. Surface IK remains under development and is not enabled.

## First play test

Start with a private offline world. Check a tall vertical wall, a leaning rock face, sideways movement, jump-off near a flat top, and an ordinary iron lattice. Near the ground, verify that the stamina bar drains while still, that slow and normal movement cost appropriately, that exhaustion releases the hold, and that food/equipment modifiers and recovery behave normally. Natural rock must not trigger an automatic climb-up animation; leave it with the normal Jump control. Lattices should still top out normally. Repeat with the controller. Keep `ember-climb.log`, especially if a grab fails, a hold drops, or the character tries to mantle too soon. Failed entry attempts are logged at most once every thirty seconds; idle checks stay quiet.

Near the ground, move sideways around a simple outside corner, then an inside corner. Try both directions, slow stick movement and reversing before the turn finishes. Check that grip continues onto the next face and that stopping the stick stops movement. Try upward/downward movement during the turn. These are first-session checks for the new corner path; keep a normal Jump escape available.

The client and server use the same plugin and settings. Test multiplayer only after the offline session works. This package is a candidate; native-file checks and simulated climbs do not establish runtime reliability.

## Limits

- Automatic natural-wall top-outs are unavailable. This includes a valid-looking flat top; use Jump to leave the wall. Whole-body landing and animation-path checks must be proven before this feature can be enabled. The staged prevention does not establish the cause of a different mod's reported terrain/spawn recovery.
- Static vertical inside and outside corners up to 90 degrees now have a bounded runtime transfer path. It requests velocity, recasts at the exact candidate chosen by the native move check, and changes facing only after native success. Three distinct noncollinear real hits on one face remain required. Sloping, overhanging, dynamic and unavailable corner geometry can still stop movement. Gameplay feel and reliability need a later offline test.
- Seam forgiveness is unavailable. A complete loss of the hold rays drops the climber without a timed grace period. The pure proposed grace model needs proven initialization and lifecycle ownership before a later build can enable it.

- A surface must reach 2 m above your feet to start a climb. A lower wall starts nothing, and neither does a ledge you jump at when it is less than 2 m above your feet at that moment. Use the game's jump there. This may change after the first play test.
- Rough or short shelves can still stop upward movement; the native controller/collision test remains authoritative. The former 1.2 m hold/move reach mismatch is corrected, including the clean 0.7 to 0.8 m bench cases in the model. Further geometry limits need a play test.
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
- **A client with the mod on a vanilla server** gets a stuck start and a snap back at natural walls. This is a hypothesis until it is tested. Lattice geometry and exit answers stay native, but the candidate's hanging effort needs the matching server too.

## Build and test

```text
cargo asi-climb
cargo test -p ember-climb
cargo test -p ember-climb --test real_exe -- --ignored
```

`cargo asi-climb` writes `target/release/ember-climb.asi`. Copy `ember-climb.default.ini` as `ember-climb.ini`, or let the mod create it on first start.

The additional `trace_exe` file tests validate the complete three-observer target set, both accessor call sites, initial instruction-copy spans and original unwind headers in all four exact images. They require explicit `ENSHROUDED_PREVIOUS_STEAM_EXE`, `ENSHROUDED_STEAM_EXE`, `ENSHROUDED_PREVIOUS_SERVER_EXE` and `ENSHROUDED_SERVER_EXE` paths. Run `cargo test -p ember-climb --test trace_exe -- --ignored --test-threads=1`. They check file bytes only and do not execute copied native code.

The ignored tests read the real exes, and they fail if a file is missing. They set `ENSHROUDED_STEAM_EXE` and `ENSHROUDED_SERVER_EXE`, or use the default Steam folders. They never start the game.

## Licence

MIT.
