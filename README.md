# enshrouded-headroom

A Rust ASI plugin for Enshrouded on Windows x64 with the Vulkan renderer. Ultimate ASI Loader v10.0.0 loads it.

## What the mod does

- **Texture streaming budget — implemented.** Sizes the top tier of the game's texture budget table for the GPU. In `auto` mode, it uses the largest Vulkan `DEVICE_LOCAL` memory heap minus the configured reserve, clamped to 2–8 GiB. A result of 2 GiB or less leaves the original budget unchanged. Integrated GPUs keep the original budget in every mode.
- **Async compute — implemented.** Uses the game's own async-compute override path. It leaves the game in charge when `AsyncCompute=game` or a command-line argument mentions `async-compute`.
- **NVIDIA Reflex — planned.** The settings parser accepts a Reflex choice, but this version does not apply it.
- **FPS-target governor — in progress.** The planned governor uses spare GPU time to raise object draw distance first, then grass distance, then terrain detail distance. It is intended to undo raises when frames are late and never go below the player's menu settings. The default target is 75 fps. The settings are parsed, but this version has no running governor or draw-distance changes.

The plugin also attempts to set the game's process priority to Above Normal once, after the first Vulkan device is created. `Priority=normal` leaves the current priority unchanged.

## Status

**Work in progress. Not tested in-game yet.** Implemented features still need in-game validation. There are no measured performance or stability results for this plugin yet.

The current build list recognises `f68140a3` and `f68140a3-steam` by executable SHA-256; see [the build table](src/build_id.rs). Recognition is not an in-game test result. After an unrecognised game update, code patches, including async compute, stay off by default. Texture-table checks and Vulkan API wrappers still run. Each feature logs its result.

See [the design](docs/design.md), [the implementation plan](docs/plan.md) and [the contributor instructions](AGENTS.md) for the remaining work. The settings reference below follows the current default INI and parser; planned settings in the design may differ.

## Install

This is the intended package layout from [design section 4](docs/design.md#4-package-layout), not a published release package. Close the game before adding or replacing files.

Place these files in the game folder, next to the game executable:

```text
<game folder>\
  dinput8.dll                       Ultimate ASI Loader v10.0.0 x64
  ultimate-asi-loader-LICENSE.txt   the loader's MIT licence
  LICENSE                           this mod's MIT licence
  plugins\
    enshrouded-headroom.asi         this mod
    enshrouded-headroom.ini         settings
    enshrouded-headroom.log         created at run time
```

Use the x64 loader as `dinput8.dll`. The mod's settings file comes from [enshrouded-headroom.default.ini](enshrouded-headroom.default.ini), renamed to `enshrouded-headroom.ini`. If it is missing, the plugin tries to write a default copy at start-up. Keep an existing settings file when updating. Do not overwrite another mod's loader without checking how it is shared.

Restart the game after changing the settings. Check the mod log to see which features applied. There is no in-game menu or overlay in this version. The `--enable-dxgi` swapchain path is outside the intended scope.

## Settings

Edit `enshrouded-headroom.ini` as UTF-8. The plugin looks for the first readable file in this order:

1. Next to `enshrouded-headroom.asi` (normally `plugins\`).
2. In `%LOCALAPPDATA%\enshrouded-headroom\`.

If neither folder has a readable file, it tries to create the default file in the first usable folder where no INI exists. It never overwrites an existing INI. If neither location works, it uses built-in defaults. The `settings:` log line names the chosen file or says that defaults are in use.

Missing keys use their defaults. Invalid values use their defaults, and numeric values outside their ranges are clamped. Unknown sections and keys are ignored with warnings. If `LowerAbove` is not greater than `RaiseBelow`, both return to their defaults. Section names, key names and word choices are case-insensitive. Boolean values accept `0`, `1`, `false` or `true`. Lines starting with `;` or `#` are comments.

The tables list all 27 keys from [the default INI](enshrouded-headroom.default.ini), with their defaults as written there. GiB and MiB are binary units.

### `[General]`

| Key | Default | Meaning |
|---|---|---|
| `Enabled` | `1` | `1` enables feature start-up. `0` still reads settings and opens the log, then skips executable hashing, features and hooks. |
| `TextureBudget` | `auto` | `auto` uses the GPU heap and reserve formula above. `off` leaves the budget alone and skips the table search. A number from 2 to 8 sets a fixed budget in GiB, without subtracting the reserve. Integrated GPUs remain unchanged. |
| `TextureReserveGiB` | `11` | Amount subtracted from the largest device-local heap in `auto` mode, in GiB. Range: 4–16. It has no effect with a fixed budget or `off`. This is a calculation input, not a live VRAM reservation. |
| `AsyncCompute` | `on` | `on` attempts the checked code patch on an allowed build. `game` leaves the game's choice unchanged. The command-line rule below can also skip the patch. |
| `Priority` | `above_normal` | Attempts Above Normal process priority once after the first Vulkan device is created. `normal` leaves the current priority unchanged. A failed Windows request is logged. |
| `Reflex` | `game` | Parsed choices: `game`, `off`, `on`, `boost`. They describe following the game, disabling Reflex, enabling it, or enabling it with boost. **Planned: none changes runtime behaviour in this version.** |
| `AllowUnknownBuild` | `0` | `0` blocks code patches on an unknown executable build. `1` permits attempts on an unknown build; signatures and original-byte checks still apply. Texture data checks and Vulkan API wrappers do not depend on this switch. |
| `LogLevel` | `info` | Logging threshold: `error`, `warn`, `info` or `debug`. `debug` includes more diagnostic detail. |

The texture feature requires a unique match for the game's original table. If another tool changed its top budget value, the plugin leaves it alone. Changes affect process memory only; the plugin does not rewrite game files on disk.

For async compute, an argument containing `async-compute` skips the patch, even if it is not a recognised game flag. This check ignores ASCII case and removes double quotes. It excludes the executable path. For example, both `--enable-async-compute` and `--disable-async-compute` leave the game in charge.

### `[Governor]`

**In progress: all keys in this section are parsed, but have no runtime effect yet.** The meanings below describe their intended use by the governor.

| Key | Default | Intended meaning |
|---|---|---|
| `TargetFps` | `75` | Target frame rate. `0` disables the planned governor; otherwise 30–360 fps. The planned controller also caps the target at the display refresh rate. |
| `WindowSeconds` | `2` | Frame-time measurement window in seconds. Range: 0.5–10. |
| `RaiseBelow` | `0.90` | Raise a distance when the 95th-percentile frame time stays below this fraction of the target frame time. Range: 0.5–0.99. |
| `RaiseHoldSeconds` | `1.0` | Seconds the raise condition must hold. Range: 0.1–10. |
| `SettleSeconds` | `1.0` | Seconds to wait after a change before the next decision. Range: 0.1–10. |
| `LowerAbove` | `0.97` | Undo the last raise when the 95th-percentile frame time stays above this fraction of the target frame time. Range: 0.8–1.0; must exceed `RaiseBelow`. |
| `LowerHoldSeconds` | `0.3` | Seconds the lower condition must hold. Range: 0.1–10. |
| `HitchFactor` | `2.0` | Undo a recent raise at once if a frame takes more than this multiple of the target frame time. Range: 1.2–5. |
| `BlockSeconds` | `20` | Seconds before retrying a raise that was undone. Range: 1–300. |
| `MinVramFreeMiB` | `1024` | Minimum free VRAM required before raising a distance, in MiB. Range: 0–8192. |

### `[Knobs]`

**In progress: all keys in this section are parsed, but have no runtime effect yet.** The meanings below describe planned distance controls and start-up settings.

| Key | Default | Intended meaning |
|---|---|---|
| `EnableObjectDistance` | `1` | `1` allows the governor to raise object draw distance; `0` disables this control. |
| `MaxObjectDistance` | `4` | Highest object-distance step allowed. Range: 0–4; 0 is the menu baseline. |
| `EnableScatterDistance` | `1` | `1` allows the governor to raise grass and detail-scatter distance; `0` disables this control. |
| `MaxScatterDistance` | `4` | Highest scatter-distance step allowed. Range: 0–4; 0 is the menu baseline. |
| `EnableTerrainDistance` | `1` | `1` allows the governor to raise terrain detail distance; `0` disables this control. |
| `MaxTerrainDistance` | `4` | Highest terrain-distance step allowed. Range: 0–4; 0 is the menu baseline. |
| `DecoDistanceScale` | `1.0` | Start-up scale for the decoration culling threshold. Range: 0.5–1.0; 1.0 keeps the game's value. A lower value is expected to draw farther, but that direction is unconfirmed in-game. |
| `DistantObjectsScale` | `1.0` | Start-up scale for the distant-object culling threshold. Range: 0.5–1.0; 1.0 keeps the game's value. A lower value is expected to draw farther, but that direction is unconfirmed in-game. |
| `TerrainRingScale` | `1.0` | Start-up scale for terrain detail rings. Range: 1.0–1.6; 1.0 keeps the game's value. Higher values are intended to widen the rings. |

## Logs and troubleshooting

The plugin writes `enshrouded-headroom.log` next to the `.asi`. If that location cannot be written, it tries `%LOCALAPPDATA%\enshrouded-headroom\`. If neither location works, file logging stays off. The log is replaced each run, so save a copy before restarting when investigating a problem.

At the default `info` level, check these entries:

- `settings:` names the settings file in use. This explains why editing a different copy has no effect.
- `game build:` identifies the executable and says whether code patches are on. An unknown build normally leaves async compute off. It does not disable every feature.
- `texture budget:` reports the table search and the budget decision. An integrated GPU, an unchanged 2 GiB result, or another tool's changed table can explain why the budget stays unchanged.
- `async compute:` explains whether the patch applied or was skipped. Check the INI, launch arguments and build line.
- `wrapper chain engaged (device created)` reports the first Vulkan device and GPU information when available.
- `priority:` reports the priority request or why it was left unchanged.

For more detail, set `LogLevel=debug` and restart. Setting `Enabled=0` keeps settings and logging available while skipping feature start-up. Reflex, governor and distance settings do not change the game in this version.

If there is no mod log, check both locations and their write permissions, then check that the x64 loader and `.asi` are in the intended folders. A missing log alone does not prove whether the loader ran. Keep the log when reporting a problem, along with the game build and relevant settings.

## Uninstall

Close the game. Remove `plugins\enshrouded-headroom.asi`, its `enshrouded-headroom.ini` and its `enshrouded-headroom.log`. Remove the same INI and log from `%LOCALAPPDATA%\enshrouded-headroom\` if they were created there and are no longer needed. Keep a copy of the INI if you want to reuse your settings.

Remove `dinput8.dll` and `ultimate-asi-loader-LICENSE.txt` only if no other ASI plugin uses that loader. Remove the mod's `LICENSE` only if it came from this package. Delete only this mod's files; keep the `plugins` folder and other mods. The plugin's game-memory changes end when the process exits.

## Build from source

The crate is Windows-only and targets `x86_64-pc-windows-msvc`. Install Rust and the Visual Studio C++ build tools with the Windows SDK. [rust-toolchain.toml](rust-toolchain.toml) pins the Rust toolchain and components. Run these commands from the repository root on Windows:

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo asi
```

`cargo asi` builds `target/release/enshrouded-headroom.asi`. It does not package the loader or install the mod.

On Linux, the Windows tests cannot run. Use the cross-target checks from [AGENTS.md](AGENTS.md):

```sh
rustup target add x86_64-pc-windows-msvc
cargo check --target x86_64-pc-windows-msvc --all-targets
cargo clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings
```

State that tests were not run when submitting a change checked only on Linux, so Windows CI can run them. Tests marked `#[ignore]` require the real game executable or a desktop. Leave them ignored unless that environment is available. Passing source tests does not validate in-game behaviour.

## Licence

This mod uses the MIT licence; see [LICENSE](LICENSE). Ultimate ASI Loader also uses the MIT licence. Its licence must accompany the bundled loader as `ultimate-asi-loader-LICENSE.txt`.

## Credits

Ultimate ASI Loader by ThirteenAG.

## About AI assistance

TODO: the author writes this section.
