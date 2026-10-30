# ember-climb

Free climbing for Enshrouded, in the style of Breath of the Wild. Climb any steep rock, cliff or wall. There is no button. A climb starts when you walk, sprint or jump into a steep surface. The mod uses the game's own climb moves, stamina and collision.

**This is a first test build.** It has never run in the game. The geometry is tested in simulated worlds, and the hooks are tested against fakes. Nobody knows yet how it feels. The diagnostics line is on so that one play session shows which step fails.

## Install

The mod needs [Ultimate ASI Loader](https://github.com/ThirteenAG/Ultimate-ASI-Loader) as `dinput8.dll` in the game folder.

1. **Remove ClimbAnywhere first.** The two must never run together. Delete `climb-anywhere.asi` and any `dinput8.dll` from `plugins\`, and `dinput8Hooked.dll` from the game folder. You can delete `climb-anywhere.ini` and `climb-anywhere.log` too.
2. Put `ember-climb.asi` and `ember-climb.ini` in `plugins\` in the game folder.
3. Start the game. `plugins\ember-climb.log` shows what happened.

**No `plugins\ember-climb.log` after a game start?** The loader did not load the plugin. Check that `dinput8.dll` in the game folder is Ultimate ASI Loader. Other mods ship a file with the same name.

ember-climb refuses to patch while it sees ClimbAnywhere loaded, and the log says why. It goes by the names of loaded modules: anything with "climb anywhere" in its name in any spelling, `dinput8Hooked.dll`, or a `dinput8.dll` inside `plugins\`. If another mod uses one of those names, the log names the file. It looks again ten seconds after start-up, because a plugin can load after this one. It only reads names. It never opens another mod's files.

To remove the mod, delete those two files and the log.

For a dedicated server, do the same in the server folder. The server decides where players are, so the mod must run there for its players to climb. **The server build is not tested at all yet.** See "Server" below.

## What you should see

The log starts like this on a supported game:

```text
ember-climb 0.1.0 loaded
settings: <game folder>\plugins\ember-climb.ini
build f68140a3-steam
all 15 signatures matched once
wrote the wall-lost return at exe+0x24b4a5, the attachment store at exe+0x418db8
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

While climbing happens, the log gets one line a second, like this (shortened):

```text
climb diag: probe ok 0, 0x15 0, 0x1e 0, 0x1f 412 ... | natural entry 3/90, move 180/190, stay 200/200 (refused dir 0, hits 12, bumpy 8, walkable 0, overhang 0, far 67; last hits 4 spread 0.12 ny 0.21 gap +0.02) | ... | lost 0/200 (voxel 200, grace 0, no probe 0) | steer 180 | ...
```

The counts add up over the session. `natural entry 3/90` means 90 tries to start a climb on natural rock and 3 that could start.

| Part | What it tells you |
|---|---|
| `probe ok` | The game's own probe passed: a lattice. |
| `natural entry`, `move`, `stay` | The mod's surface fit, for starting, moving and holding. |
| `refused` | Why a surface was refused: `hits` too few rays hit, `bumpy` not one surface, `walkable` too flat, `overhang` hangs over too far, `far` out of reach. |
| `last hits`, `spread`, `ny`, `gap` | The last fit: rays that hit, unevenness in metres, how much the surface faces up, and how far the player is past the wanted distance. |
| `found` | How the player was moving when a surface was found. `off` counts starts that a setting refused. |
| `low` | The game's start check: `angle` the stick was too far off, `cone` the mod's own test let it through. |
| `lost` | Hold checks that dropped the player, out of all checks. `voxel` held on terrain, `grace` forgiven for a moment. |
| `exit` | How climbs ended. `refused` stopped a mantle onto steep rock, and `taken` found a way out the game did not. |
| `capsule` | The player's real collision size. |

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
| Tuning | OverhangAngle | 110 | 90 to 135 | The farthest a face may hang over, in degrees. 90 is a sheer wall. |
| Tuning | EntryReach | 0.95 | 0.6 to 1.4 | How close you must be to start a climb, in metres. |
| Tuning | HoldReach | 1 | 0.8 to 1.6 | How far the mod looks for the surface during a climb, in metres. |
| Tuning | GraceSeconds | 0.15 | 0 to 1 | How long a lost surface is forgiven before you fall. |
| Debug | Diagnostics | 1 | 0 or 1 | The one-line-a-second log. It also adds five small hooks. |

`SurfaceMask` is a sum of these bits, in hex or decimal: `0x4` terrain and player walls, `0x20` rock props, `0x100` iron lattices, `0x8` other solid props such as trees. Other bits are refused. Bits 0 and 1 are other copies of the terrain, and asking for them gives a different surface per ray.

## Gliding

`Glide = 1` is off by default because three things about it are unproven: whether a climb can replace the glider sequence, whether the glider state ends cleanly, and whether the game gives move input while gliding. It also changes how the game requests every climb, lattices too. Turn it on to test it, and keep the log.

## What it does

The game already climbs, but only on flat iron lattices. Its wall probe wants four rays with one exact normal. Rock never gives that.

- The game's probe always runs first, so a lattice stays exactly as it was. No mask or tolerance in the game is changed.
- When the probe fails, the mod casts four rays of its own at the surface, averages what they hit, and asks whether it is steep and flat enough.
- The game moves a climber along world up, which walks straight off any face that leans. The mod turns the movement into the surface, keeps the speed, and holds the game's 0.55 m distance.
- The game has no exit where a wall turns into a slope, and it mantles onto steep rock. The mod refuses the second and finds the first.
- A half-pressed stick fails the game's start test however straight you run at a wall. The mod tests only the heading.
- A surface lost for a moment is forgiven, so a seam in the rock does not drop you.

Evidence for every game fact is in [docs/re-notes.md](../../docs/re-notes.md), "Free climbing".

## Server

The mod is built for the dedicated server build `d7baa768-server`, and the code path is the same. **The server exe was not available when this was written, so nothing here has been checked against it.** If any signature does not match exactly once there, the mod logs which one and installs nothing. Before a server test:

```text
set ENSHROUDED_SERVER_EXE=<path to enshrouded_server.exe>
cargo test -p ember-climb --test real_exe -- --ignored
```

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
