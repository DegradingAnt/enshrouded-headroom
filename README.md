# enshrouded-headroom

A Windows x64 ASI plugin for Enshrouded's Vulkan renderer, loaded by Ultimate ASI Loader v10.0.0.

**0.1.0 release preparation, not a ready-to-release build.** The governor, NVIDIA Reflex and AMD Anti-Lag runtime integrations are pending. The author still needs to complete the in-game checks; there are no claimed performance or stability results. See the [release checklist and Nexus draft](docs/release-checklist.md).

## What the mod does

**Texture streaming budget** defaults to `TextureBudget=auto` with `TextureReserveGiB=11`. It uses the largest Vulkan device-local heap minus the reserve, clamped to 2–8 GiB. A result of 2 GiB or less leaves the original budget unchanged. Integrated GPUs keep the original budget in every mode. The full original table must match before a write; an existing change by another tool is left alone.

**Async compute** defaults to `on`. It uses the game's checked override path on an allowed build. `game`, or any launch argument containing `async-compute`, leaves the game in charge. The argument check ignores ASCII case and quotes and excludes the executable path.

**Object distance** defaults to enabled, with maximum step 4. The scene hook captures the player's baseline and supports object-distance factors, but the current startup leaves the step at zero: automatic changes await the governor.

**Grass and detail-scatter distance** defaults to enabled, with configured maximum step 4. The checked redirect is implemented, but the effective ceiling remains the game's 90 m while capacity headroom is unproven. A configured maximum does not bypass that ceiling.

**Terrain detail distance** defaults to enabled, with maximum step 4. The checked terrain hook is implemented and starts at the menu baseline; automatic raises await the governor. The separate decoration, distant-object and terrain-ring startup scale settings default to `1.0` and are parsed but not applied on this base.

**FPS-target governor** defaults to a planned target of 75 fps. Its decision logic is implemented; its runtime thread is not integrated here. It is intended to spend spare frame time on objects, grass, then terrain, undoing raises when frames are late without going below the player's menu settings. `TargetFps=0` will disable that controller.

**NVIDIA Reflex** defaults to `auto`. The pure mode/device-planning layer exists, but this base has no active runtime Reflex calls. Pending integration must support `game`, `auto`, `off`, `on` and `boost`; `game` follows saved off/on/boost values and uses auto while unknown. Do not expect a latency change from this preparation build.

**AMD Anti-Lag** defaults to `auto`. The experimental pure layer exists; its runtime integration is pending and it is **not verified on AMD hardware**. Its planned automatic mode requires AMD plus the extension and feature; active Reflex takes precedence.

**HDR paper white** defaults to `game`, retaining 250 nits. A finite 40–400 nit override is a checked startup code patch. **HDR peak correction** defaults to `FixPeak=off`, with `DisplayPeakNits=game`. Opting in uses the shared scene hook and verified effective paper white; at least one distance knob must enable that hook. A failed paper-white read-back leaves peak correction off. SDR is unchanged. Both visible results await the author's HDR comparison.

**Process priority** defaults to `above_normal`, requested once after the first Vulkan device is created. `normal` leaves the current priority unchanged; a failed Windows request is logged.

Shadows above Ultra are not included in this base, and there is no shipped shadow setting.

## Build under test and pending integration

The Steam verification target is **`f68140a3-steam`**. Executable SHA-256 recognition is implemented for that build and `f68140a3`; see [the build table](src/build_id.rs). Recognition is not proof of a successful game boot or feature validation.

After an unrecognised game update, code patches and hooks stay off until the build is known. Checked texture data and Vulkan API wrappers still run. `AllowUnknownBuild=1` permits attempts at your own risk; signatures and original-byte checks still apply.

Before release, update these preparation notes against the integrated source and rerun validation:

- W23 / [#27](https://github.com/DegradingAnt/enshrouded-headroom/issues/27): Reflex runtime, lifecycle limitations and actual enable/disable log messages.
- W24 / [#30](https://github.com/DegradingAnt/enshrouded-headroom/issues/30): governor thread, VRAM query, reports and actual knob control.
- W25 / [#31](https://github.com/DegradingAnt/enshrouded-headroom/issues/31): Anti-Lag runtime, precedence and explicit AMD verification status.
- Resolve the currently unapplied startup scales, the unmeasured grass ceiling and any still-pending validation work before describing finished v1 behavior.
- Regenerate the settings table if defaults grow, confirm startup/version agreement, repackage, and record the author's game results.

## Install

These are instructions for a future approved package. Close the game first.

1. In Steam, open Enshrouded's **Properties → Installed Files → Browse**. Use the folder containing the game executable, normally `Steam\steamapps\common\Enshrouded` under your Steam library.
2. Extract the package there, preserving the `plugins` directory below. Use the x64 loader as `dinput8.dll`; do not overwrite another mod's shared loader without checking compatibility.
3. Save the .ini as UTF-8. Restart the game after editing it, then inspect the mod log for the selected settings and feature outcomes.

```text
<game folder>\
  dinput8.dll
  ultimate-asi-loader-LICENSE.txt
  LICENSE
  THIRD_PARTY_NOTICES.md
  plugins\
    enshrouded-headroom.asi
    enshrouded-headroom.ini
    enshrouded-headroom.log         created at runtime, not in the zip
```

The package's INI comes from [the default INI](enshrouded-headroom.default.ini). There is no in-game menu or overlay. The `--enable-dxgi` swapchain path is outside this mod's intended Vulkan scope.

## Using with ShroudForge

This guidance comes from the maintainer's read-only review of [ShroudForge](https://github.com/bonsaibauer/shroudforge) 1.7.5, commit `46b59da`, and its release archive. ShroudForge installs its own `dinput8.dll`, `dbghelp.dll` and `winmm.dll` in the game folder. Its `dinput8.dll` has the same filename as Ultimate ASI Loader, which Headroom needs.

By default, ShroudForge checks for automatic updates every 60 minutes and installs them after the game exits, without a confirmation click. Its updater and uninstaller replace or delete `dinput8.dll` by name. If Ultimate ASI Loader is replaced or removed, Headroom silently stops loading.

- Keep Ultimate ASI Loader as `dinput8.dll`.
- Install ShroudForge without its `dinput8.dll`.
- Turn ShroudForge's automatic updates off, or verify that `dinput8.dll` is still Ultimate ASI Loader after every ShroudForge update.

A symptom is no fresh `plugins\enshrouded-headroom.log` after a game start. Check the loader if this happens after a ShroudForge update or uninstall.

**Running Headroom and ShroudForge together is untested.** Frame-time measurements with ShroudForge mods running are not comparable to Headroom-only runs: ShroudForge runs its own 16 ms thread that touches game state.

## Update

Close the game and back up your INI and any diagnostic log you need. Replace the ASI and package notices with the approved new versions. Keep your existing INI; compare the new default file for added settings instead of overwriting your choices. Replace a shared loader only after checking the other plugins that use it. Restart and check the build/settings lines in the log.

## Settings

Save the .ini as UTF-8. The plugin tries the first readable `enshrouded-headroom.ini` beside its ASI (normally `plugins`), then `%LOCALAPPDATA%\enshrouded-headroom`. If neither is readable, it creates defaults only where no INI exists and never overwrites an existing file. If neither location is usable, built-in defaults apply. The `settings:` log line names the selected file.

Missing or invalid keys use defaults. Numeric out-of-range values are clamped, except `PaperWhiteNits` and `DisplayPeakNits`, which use `game`. If `LowerAbove` is not greater than `RaiseBelow`, both reset to defaults. Names and choices are case-insensitive; boolean keys accept `0`, `1`, `false` and `true`. Numeric decimal commas are accepted. Unknown keys warn. Lines beginning with `;` or `#` are comments. GiB and MiB are binary units.

The table is generated from the crate's `DEFAULT_INI`, with reviewed ranges and descriptions. Run `cargo run --example generate_readme_settings` after changing defaults; `cargo run --example generate_readme_settings -- --check` verifies it without editing. Governor descriptions remain planned until runtime integration lands.

<!-- BEGIN GENERATED SETTINGS -->
| Section | Key | Default | Range or choices | Meaning |
|---|---|---|---|---|
| `General` | `Enabled` | `1` | 0, 1 (also false, true) | Enable feature startup while keeping settings and logging available when disabled. |
| `General` | `TextureBudget` | `auto` | auto, off, 2–8 GiB | Choose automatic or fixed texture budget sizing; integrated GPUs keep the original budget. |
| `General` | `TextureReserveGiB` | `11` | 4–16 GiB | Subtract this amount from the largest device-local heap in auto mode. |
| `General` | `AsyncCompute` | `on` | on, game | Attempt the checked async-compute patch unless a launch argument mentions async-compute. |
| `General` | `Priority` | `above_normal` | above_normal, normal | Request Above Normal priority once after device creation, or leave priority unchanged. |
| `General` | `Reflex` | `auto` | game, auto, off, on, boost | Select the NVIDIA Reflex mode; runtime integration is pending. |
| `General` | `AntiLag` | `auto` | auto, off, on | Select the experimental Anti-Lag mode; runtime integration and AMD verification are pending. |
| `General` | `AllowUnknownBuild` | `0` | 0, 1 (also false, true) | Permit checked code-patch attempts on unknown builds only at your own risk. |
| `General` | `LogLevel` | `info` | error, warn, info, debug | Set the logging threshold. |
| `Governor` | `TargetFps` | `75` | 0 (off), 30–360 fps | Set the planned governor target, capped by display refresh when runtime integration lands. |
| `Governor` | `WindowSeconds` | `2` | 0.5–10 seconds | Choose the planned frame-time measurement window. |
| `Governor` | `RaiseBelow` | `0.90` | 0.5–0.99 | Allow a planned raise when the 95th-percentile frame time is below this target-time fraction. |
| `Governor` | `RaiseHoldSeconds` | `1.0` | 0.1–10 seconds | Require the planned raise condition to hold for this long. |
| `Governor` | `SettleSeconds` | `1.0` | 0.1–10 seconds | Wait this long after a planned distance change before deciding again. |
| `Governor` | `LowerAbove` | `0.97` | 0.8–1.0; greater than RaiseBelow | Undo a planned raise when the 95th-percentile frame time exceeds this target-time fraction. |
| `Governor` | `LowerHoldSeconds` | `0.3` | 0.1–10 seconds | Require the planned lower condition to hold for this long. |
| `Governor` | `HitchFactor` | `2.0` | 1.2–5 | Undo a recent planned raise if a frame exceeds this multiple of the target frame time. |
| `Governor` | `BlockSeconds` | `20` | 1–300 seconds | Delay retrying a planned raise that was undone. |
| `Governor` | `MinVramFreeMiB` | `1024` | 0–8192 MiB | Require this free VRAM before a planned distance raise. |
| `Knobs` | `EnableObjectDistance` | `1` | 0, 1 (also false, true) | Enable the object-distance hook path; automatic step changes await the governor. |
| `Knobs` | `MaxObjectDistance` | `4` | 0–4 | Limit the planned object-distance step, with zero preserving the menu baseline. |
| `Knobs` | `EnableScatterDistance` | `1` | 0, 1 (also false, true) | Enable the checked grass-distance redirect, currently held at the game’s 90 m ceiling. |
| `Knobs` | `MaxScatterDistance` | `4` | 0–4 | Limit the planned scatter step, further restricted by proven capacity headroom. |
| `Knobs` | `EnableTerrainDistance` | `1` | 0, 1 (also false, true) | Enable the terrain-distance hook path; automatic step changes await the governor. |
| `Knobs` | `MaxTerrainDistance` | `4` | 0–4 | Limit the planned terrain-distance step, with zero preserving the menu baseline. |
| `Knobs` | `DecoDistanceScale` | `1.0` | 0.5–1.0 | Select the parsed decoration threshold scale; startup application is not wired on this base. |
| `Knobs` | `DistantObjectsScale` | `1.0` | 0.5–1.0 | Select the parsed distant-object threshold scale; startup application is not wired on this base. |
| `Knobs` | `TerrainRingScale` | `1.0` | 1.0–1.6 | Select the parsed terrain-ring scale; startup application is not wired on this base. |
| `Hdr` | `PaperWhiteNits` | `game` | game, 40–400 nits | Keep the game’s 250-nit paper white or apply a checked startup override. |
| `Hdr` | `FixPeak` | `off` | on, off | Opt into HDR peak correction through the shared scene hook when a distance knob enables it. |
| `Hdr` | `DisplayPeakNits` | `game` | game, whole number 80–10000 nits | Choose the game peak or panel target used only when FixPeak is on. |
<!-- END GENERATED SETTINGS -->

## Logs, diagnostics and known limits

See the [troubleshooting guide](docs/troubleshooting.md) for log messages and next steps. The log is written beside the ASI, then in `%LOCALAPPDATA%\enshrouded-headroom` if needed; it is replaced each run. Save it before restarting when reporting a problem. If neither location is writable, file logging remains off. A missing log alone does not prove whether the loader ran.

At `LogLevel=info`, check `settings:`, `game build:`, `texture budget:`, `async compute:`, `wrapper chain engaged`, HDR and distance-hook outcomes, and `priority:`. `Enabled=0` keeps settings and logging but skips feature startup. See the [bug-report form](.github/ISSUE_TEMPLATE/bug_report.yml) for the required evidence.

The scatter probe is a developer diagnostic, off by default. `[Debug] ScatterProbe=1` also requires `LogLevel=debug`; sampling is limited to once every 500 ms. Hidden debug keys are not part of the shipped default table. The grass ceiling remains 90 m until measured capacity allows more. Debug forced-step tests are for the author's verification, not a supported way to bypass the ceiling.

Anti-Lag approximates INPUT after the previous present, with a seed before the first present.
It does not hook the game's actual input processing. Three consecutive driver updates over
100 ms, device loss or a concurrent present stream disable it for the session. A returned slow
call can be detected; a driver call that never returns cannot be cancelled. The next device or
swapchain lifecycle operation reports a stop once. On a non-AMD GPU, including the author's
RTX 5080, `AntiLag=on` logs "not listed, off" and adds no AMD extension. See
[design section 6.6](docs/design.md#66-amd-anti-lag-2-experimental) for the safety limits.

The texture feature requires a unique match for the game's original table. If another tool changed its top budget value, the plugin leaves it alone. Changes affect process memory only; the plugin does not rewrite game files on disk.

AMD Anti-Lag is unverified on AMD hardware. Runtime governor/Reflex/Anti-Lag integration, distance-direction tests, HDR comparisons and the game soak test are still release gates. Source tests and a correctly hashed zip do not establish runtime results or performance gains.

## Using with Extended Graphics and Physics Tweaker

Headroom leaves Tweaker's fog, GI, resource and shader settings under Tweaker's control. A startup notice detects only its two installation files, not active patches. Combined runtime and performance checks remain pending; see [Tweaker compatibility](docs/tweaker-compatibility.md) for ownership, workload limits and the test matrix.

## Uninstall

Close the game. Remove only `plugins\enshrouded-headroom.asi`, its INI and its log. Remove this mod's fallback INI/log from `%LOCALAPPDATA%\enshrouded-headroom` if unwanted; keep a backup of your settings. Remove `dinput8.dll` and its licence only if no other ASI plugin uses that loader. Keep other mods and their `plugins` directory. Remove this package's notices only if they are not shared. Game-memory changes end when the process exits.

## Build and package from source

Use Windows x64, the pinned [Rust toolchain](rust-toolchain.toml), and Visual Studio C++ build tools with the Windows SDK. From the repository root:

```powershell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
$env:RUSTDOCFLAGS='-D warnings'
cargo doc --no-deps
cargo test
pwsh -File scripts/package.ps1
```

The packager runs `cargo asi`, verifies the pinned loader archive, requires exactly six zip entries and compares the archived ASI with `target/release/enshrouded-headroom.asi` before publishing the local zip. It prints both hashes and the zip hash. Building or packaging does not install the mod or authorize release. A development preparation zip still contains the current incomplete implementation.

`pwsh -File scripts/test-package.ps1 -LoaderZip dist/cache/Ultimate-ASI-Loader_x64-v10.0.0.zip` tests staged-ASI corruption refusal in an isolated fixture without rebuilding. The fixture must use a verified loader cache. Leave ignored game/desktop tests ignored; see [AGENTS.md](AGENTS.md) for Windows and Linux cross-check commands.

## Licence and credits

This mod uses the [MIT licence](LICENSE). Ultimate ASI Loader v10.0.0 by ThirteenAG is bundled with its MIT licence. [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) contains the loader, MinHook, HDE and resolved Rust dependency notices.

## About AI assistance

TODO: the author writes this section.

### Workspace and process targets

`enshrouded-headroom` targets the Enshrouded **client only**. A positively identified dedicated server writes one skip line and installs no rendering features or hooks. Identification uses a case-insensitive executable filename and the shared known-build table. The server hash in that table is maintainer-supplied evidence. Unknown executable names remain `Unknown`; development hosts retain the existing build and byte-check guards.

`crates/emberkit` holds reusable native infrastructure as an MIT-licensed Rust library. New mods are separate crates depending on it; each declares its process targets. It has no ASI entry export. The root remains the default workspace member, so `cargo asi` keeps the existing artifact name and exports. Use `--workspace` for lint, docs and tests (`cargo fmt --all --check` for formatting). `cargo deny` traverses both workspace members from the workspace root.

`enshrouded_headroom::config::schema()` returns all 35 read settings without file I/O. Entries describe types, accepted ranges or words, default spellings, and restart requirements. All settings currently require a restart. The four developer settings under `Debug` are deliberately absent from the 31-key default INI. The three force-step defaults are absent values, not a literal `none` spelling. No settings UI is included.
