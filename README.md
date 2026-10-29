# enshrouded-headroom

A Windows x64 ASI plugin for Enshrouded's Vulkan renderer, loaded by Ultimate ASI Loader v10.0.0.

**0.1.0 release preparation.** The governor, NVIDIA Reflex and AMD Anti-Lag runtime paths are implemented. In-game verification and final-package validation remain release gates; no performance or stability result is claimed. See the [release checklist and Nexus draft](docs/release-checklist.md).

## What the mod does

**Texture streaming budget** defaults to `TextureBudget=auto` with `TextureReserveGiB=11`. It uses the largest Vulkan device-local heap minus the reserve, clamped to 2–8 GiB. A result of 2 GiB or less leaves the original budget unchanged. Integrated GPUs keep the original budget in every mode. The full original table must match before a write; an existing change by another tool is left alone.

**Async compute** defaults to `on`. It uses the game's checked override path on an allowed build. `game`, or any launch argument containing `async-compute`, leaves the game in charge. The argument check ignores ASCII case and quotes and excludes the executable path.

**Object distance** defaults to enabled, with maximum step 4. The scene hook captures the player's menu baseline. The governor starts at step zero and can raise the step when frame-time, world-signal and memory conditions permit; it never requests a step below the baseline. The visible distance direction still needs in-game confirmation.

**Grass and detail-scatter distance** defaults to enabled, with configured maximum step 4. The checked redirect is implemented, but the effective ceiling remains the game's 90 m while capacity headroom is unproven. A configured maximum does not bypass that ceiling.

**Terrain detail distance** defaults to enabled, with maximum step 4. Its checked hook starts at the menu baseline and applies governor-controlled steps when conditions permit. The visible result still needs in-game confirmation.

**Startup scales** for High decoration distance, distant objects and terrain rings default to `1.0`, leaving the original immediates unchanged. Non-unit values apply at startup through build, signature and original-value checks. The log reports applied, skipped or incomplete writes. These settings require a restart.

**FPS-target governor** defaults to a 75 fps target, limited by display refresh. Its background loop runs every 50 ms and spends available headroom on object, grass and terrain steps in that order. It undoes raises when frames are late and holds current steps when the world signal is absent. Live VRAM is queried every two seconds; unknown or insufficient memory blocks costly raises. The grass capacity ceiling still applies. `TargetFps=0` stops governor control; explicit debug statistics may keep a log-only worker.

**Optional measured quality trials** use `[Governor] TrialFloorFps=65`, `TargetFps=75` and `TerrainFirst=1`. Restart after changing them. The governor tests one step at a time after settling and a fresh frame-time window. Stable CPU-limited 67 FPS or capped 75 FPS can retain a tested step. Sustained cost beyond the larger of 0.5 ms or 3% of the retained p95 reference rolls it back, even above 65 FPS. The shared reference prevents repeated small steps from gradually spending the band. Below-floor performance causes backoff; 65 FPS is a stop boundary, while 75 FPS remains the recovery goal. The floor uses rolling p95, not an instantaneous FPS guarantee. No GPU timing is measured. Terrain is added first and removed last during normal cuts; failed trials and safety limits can still undo it. Both new settings default off. See the [precise policy](docs/design.md#opt-in-measured-quality-trials-w37).

**Frame pacing reports** append `1% low N fps, hitches N` to the governor's 30-second info status. At `LogLevel=debug`, a `frames:` line every two seconds reports p50, p99, maximum frame time and hitches. Both use frame intervals ending in the last two seconds, bounded by the existing 4096-slot ring. To keep these debug reports with `TargetFps=0`, set `[Debug] FrameStats=1` and `[General] LogLevel=debug`; the same worker runs without GPU queries, governor planning or knob writes. FrameStats defaults to 0. Empty or one-frame windows report no data. Percentiles use nearest rank; 1% low is the reciprocal of the mean slowest ceil(1%) frame times. Hitches exceed both twice p50 and 25 ms. These are observed present intervals, not GPU timings.


**NVIDIA Reflex** defaults to `auto` and requires NVIDIA plus the driver-listed extension and required capabilities. `auto` and `on` enable without boost; `boost` requests boost; `off` disables it. `game` follows saved off/on/boost values and uses auto while unknown. Runtime pacing uses present markers, driver sleep and a 10 ms semaphore-wait timeout; failures disable pacing. It has no submit hook. Available timings are logged every five seconds with debug output and a running governor; latency improvement remains unverified in game.

**AMD Anti-Lag** defaults to `auto`. Its runtime is implemented but **experimental, not verified on AMD hardware**. Both `auto` and `on` require AMD, a driver-listed `VK_AMD_anti_lag` extension and the reported feature. Active Reflex takes precedence. It paces only associated native queues and swapchains; the approximate INPUT timing and stop conditions are described below.

**HDR paper white** defaults to `game`, retaining 250 nits. A finite 40–400 nit override is a checked startup code patch. **HDR peak correction** defaults to `FixPeak=off`, with `DisplayPeakNits=game`. Opting in uses the shared scene hook and verified effective paper white; at least one distance knob must enable that hook. A failed paper-white read-back leaves peak correction off. HDR and SDR comparisons remain release gates.

**Process priority** defaults to `above_normal`, requested once after the first Vulkan device is created. `normal` leaves the current priority unchanged; a failed Windows request is logged.

Experimental shadow overrides can raise the existing Ultra constants. All four settings default to `game`; their visual effect and shader limits still need in-game verification.

## Build under test and release gates

The Steam verification target is **`f68140a3-steam`**. Executable SHA-256 recognition is implemented for that build and `f68140a3`; see [the shared build table](crates/emberkit/src/build_id.rs). Recognition is not proof of a successful game boot or feature validation.

After an unrecognised game update, code patches and hooks stay off until the build is known. Checked texture data and Vulkan API wrappers still run. `AllowUnknownBuild=1` permits attempts at your own risk; signatures and original-byte checks still apply.

The runtime features and startup scales are implemented. Release still requires:

- Final workspace checks, settings-table/version checks and package validation from the exact intended source.
- The author's Steam boot, distance-direction, Reflex, vsync/VRR, HDR and 30-minute soak checks, with recorded results.
- Capacity evidence before any grass ceiling above 90 m, and AMD hardware testing or retention of the explicit unverified limitation.

The perf mod targets the client only. A positively identified dedicated server logs a skip and installs no rendering features or hooks; see [workspace and process targets](#workspace-and-process-targets).

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

Missing or invalid keys use defaults. Numeric out-of-range values are clamped, except `PaperWhiteNits`, `DisplayPeakNits` and `[Shadows]` overrides, which use `game`. If `LowerAbove` is not greater than `RaiseBelow`, both reset to defaults. Names and choices are case-insensitive; boolean keys accept `0`, `1`, `false` and `true`. Numeric decimal commas are accepted. Unknown keys warn. Lines beginning with `;` or `#` are comments. GiB and MiB are binary units.

The table is generated from the crate's `DEFAULT_INI`, with reviewed ranges and descriptions. Run `cargo run --example generate_readme_settings` after changing defaults; `cargo run --example generate_readme_settings -- --check` verifies it without editing. The table contains the 38 shipped default keys; the schema also describes four omitted Debug keys.

<!-- BEGIN GENERATED SETTINGS -->
| Section | Key | Default | Range or choices | Meaning |
|---|---|---|---|---|
| `General` | `Enabled` | `1` | 0, 1 (also false, true) | Enable feature startup while keeping settings and logging available when disabled. |
| `General` | `TextureBudget` | `auto` | auto, off, 2–8 GiB | Choose automatic or fixed texture budget sizing; integrated GPUs keep the original budget. |
| `General` | `TextureReserveGiB` | `11` | 4–16 GiB | Subtract this amount from the largest device-local heap in auto mode. |
| `General` | `AsyncCompute` | `on` | on, game | Attempt the checked async-compute patch unless a launch argument mentions async-compute. |
| `General` | `Priority` | `above_normal` | above_normal, normal | Request Above Normal priority once after device creation, or leave priority unchanged. |
| `General` | `Reflex` | `auto` | game, auto, off, on, boost | Select a supported NVIDIA Reflex mode; game follows saved off/on/boost values. |
| `General` | `AntiLag` | `auto` | auto, off, on | Select experimental AMD Anti-Lag; enabled modes require AMD and driver support, and Reflex takes priority. |
| `General` | `AllowUnknownBuild` | `0` | 0, 1 (also false, true) | Permit checked code-patch attempts on unknown builds only at your own risk. |
| `General` | `LogLevel` | `info` | error, warn, info, debug | Set the logging threshold. |
| `Governor` | `TargetFps` | `75` | 0 (off), 30–360 fps | Set the target, limited by display refresh; 0 disables governor control, while FrameStats=1 plus debug logging permits log-only sampling. |
| `Governor` | `TrialFloorFps` | `0` | 0 (legacy), 30–360 fps; below an enabled TargetFps | Opt into one-step quality trials above a rolling p95 FPS floor; retain stable CPU/cap-limited performance and undo meaningful cost. |
| `Governor` | `TerrainFirst` | `0` | 0, 1 (also false, true) | Raise terrain first and retain it until the final normal cut; failed trials and safety vetoes still win. |
| `Governor` | `WindowSeconds` | `2` | 0.5–10 seconds | Choose the rolling frame-time measurement window. |
| `Governor` | `RaiseBelow` | `0.90` | 0.5–0.99 | Set legacy p95 headroom below this target-time fraction outside quantised vsync mode. |
| `Governor` | `RaiseHoldSeconds` | `1.0` | 0.1–10 seconds | Hold a raise condition, or accept a stable measured trial, for this long. |
| `Governor` | `SettleSeconds` | `1.0` | 0.1–10 seconds | Wait this long after a distance change before normal raise/lower decisions. |
| `Governor` | `LowerAbove` | `0.97` | 0.8–1.0; greater than RaiseBelow | Set the legacy p95 overload fraction for undoing the latest distance raise. |
| `Governor` | `LowerHoldSeconds` | `0.3` | 0.1–10 seconds | Require the normal overload condition to hold before undoing a raise. |
| `Governor` | `HitchFactor` | `2.0` | 1.2–5 | Set the post-raise hitch multiplier, with a display-period margin in quantised mode. |
| `Governor` | `BlockSeconds` | `20` | 1–300 seconds | Set the undo block and probation duration; measured trials also pause globally after any undo. |
| `Governor` | `MinVramFreeMiB` | `1024` | 0–8192 MiB | Block costly raises below this free VRAM; unknown VRAM also blocks them. |
| `Knobs` | `EnableObjectDistance` | `1` | 0, 1 (also false, true) | Allow governor-controlled object-distance steps above the menu baseline. |
| `Knobs` | `MaxObjectDistance` | `4` | 0–4 | Cap the object-distance step, with zero preserving the menu baseline. |
| `Knobs` | `EnableScatterDistance` | `1` | 0, 1 (also false, true) | Enable the checked grass-distance redirect, currently held at the game’s 90 m ceiling. |
| `Knobs` | `MaxScatterDistance` | `4` | 0–4 | Cap the scatter step, further restricted by proven capacity headroom. |
| `Knobs` | `EnableTerrainDistance` | `1` | 0, 1 (also false, true) | Enable the terrain-distance hook and its governor-controlled steps. |
| `Knobs` | `MaxTerrainDistance` | `4` | 0–4 | Cap the terrain-distance step, with zero preserving the menu baseline. |
| `Knobs` | `DecoDistanceScale` | `1.0` | 0.5–1.0 | Scale the verified High decoration threshold at startup; 1.0 keeps the original. |
| `Knobs` | `DistantObjectsScale` | `1.0` | 0.5–1.0 | Scale the verified High distant-object threshold at startup; 1.0 keeps the original. |
| `Knobs` | `TerrainRingScale` | `1.0` | 1.0–1.6 | Scale the verified High terrain rings at startup; 1.0 keeps the originals. |
| `Hdr` | `PaperWhiteNits` | `game` | game, 40–400 nits | Keep the game’s 250-nit paper white or apply a checked startup override. |
| `Hdr` | `FixPeak` | `off` | on, off | Opt into HDR peak correction through the shared scene hook when a distance knob enables it. |
| `Hdr` | `DisplayPeakNits` | `game` | game, whole number 80–10000 nits | Choose the game peak or panel target used only when FixPeak is on. |
| `Shadows` | `PointLightRays` | `game` | game, 65–256 | Experimental Ultra ray limit; game preserves vanilla. |
| `Shadows` | `PointLightStaticDistance` | `game` | game, 101–400 metres | Experimental Ultra static-light distance; game preserves vanilla. |
| `Shadows` | `PointLightTile` | `game` | game, 224, 256 | Experimental Ultra atlas tile; increases VRAM use. |
| `Shadows` | `SunResolution` | `game` | game, 4096 | Experimental Ultra sun resolution; increases VRAM use. |
| `Debug` | `FrameStats` | `0` | 0, 1 (also false, true) | Permit frame reports with the governor off; requires LogLevel=debug. |
<!-- END GENERATED SETTINGS -->

## Logs, diagnostics and known limits

See the [troubleshooting guide](docs/troubleshooting.md) for log messages and next steps. The log is written beside the ASI, then in `%LOCALAPPDATA%\enshrouded-headroom` if needed; it is replaced each run. Save it before restarting when reporting a problem. If neither location is writable, file logging remains off. A missing log alone does not prove whether the loader ran.

At `LogLevel=info`, check `settings:`, `game build:`, `texture budget:`, `async compute:`, `wrapper chain engaged`, HDR, distance-hook and `startup scales:` outcomes, `Anti-Lag:`, and `priority:`. The governor logs `started; inactive`, step changes and a 30-second status line. `Enabled=0` keeps settings and logging but skips feature startup. See the [bug-report form](.github/ISSUE_TEMPLATE/bug_report.yml) for the required evidence.

The scatter probe is a developer diagnostic, off by default. `[Debug] ScatterProbe=1` also requires `LogLevel=debug`; sampling is limited to once every 500 ms. The running governor reports configured scatter counters every two seconds and retains the largest scene peak A it observes across scene resets. All four hidden Debug keys remain outside the shipped default table. The grass ceiling remains 90 m until measured capacity allows more. Debug forced-step tests are for the author's verification, not a supported way to bypass the ceiling.

Anti-Lag approximates INPUT after the previous present, with a seed before the first present.
It does not hook the game's actual input processing. Three consecutive driver updates over
100 ms, device loss or a concurrent present stream disable it for the session. A returned slow
call can be detected; a driver call that never returns cannot be cancelled. The next device or
swapchain lifecycle operation reports a stop once. On a non-AMD GPU, including the author's
RTX 5080, `AntiLag=on` logs "not listed, off" and adds no AMD extension. See
[design section 6.6](docs/design.md#66-amd-anti-lag-2-experimental) for the safety limits.

The texture feature requires a unique match for the game's original table. If another tool changed its top budget value, the plugin leaves it alone. Changes affect process memory only; the plugin does not rewrite game files on disk.

AMD Anti-Lag is unverified on AMD hardware. In-game governor and latency validation, distance-direction tests, HDR/SDR comparisons and the soak test remain release gates. Source tests and a correctly hashed zip do not establish game results or performance gains.

## Using with Extended Graphics and Physics Tweaker

Headroom leaves Tweaker's fog, GI, resource and shader settings under Tweaker's control. A startup notice detects only its two installation files, not active patches. Combined runtime and performance checks remain pending; see [Tweaker compatibility](docs/tweaker-compatibility.md) for ownership, workload limits and the test matrix.

## Uninstall

Close the game. Remove only `plugins\enshrouded-headroom.asi`, its INI and its log. Remove this mod's fallback INI/log from `%LOCALAPPDATA%\enshrouded-headroom` if unwanted; keep a backup of your settings. Remove `dinput8.dll` and its licence only if no other ASI plugin uses that loader. Keep other mods and their `plugins` directory. Remove this package's notices only if they are not shared. Game-memory changes end when the process exits.

## Build and package from source

Use Windows x64, the pinned [Rust toolchain](rust-toolchain.toml), and Visual Studio C++ build tools with the Windows SDK. From the repository root:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
$env:RUSTDOCFLAGS='-D warnings'
cargo doc --no-deps --workspace
cargo test --workspace
cargo run --example generate_readme_settings -- --check
pwsh -File scripts/package.ps1
```

The packager runs `cargo asi`, verifies the pinned loader archive, requires exactly six zip entries and compares the archived ASI with `target/release/enshrouded-headroom.asi` before publishing the local zip. It prints both hashes and the zip hash. Building or packaging does not install the mod or authorize release. A preparation archive still needs the game and hardware verification listed above.

`pwsh -File scripts/test-package.ps1 -LoaderZip dist/cache/Ultimate-ASI-Loader_x64-v10.0.0.zip` tests staged-ASI corruption refusal in an isolated fixture without rebuilding. The fixture must use a verified loader cache. Leave ignored game/desktop tests ignored; see [AGENTS.md](AGENTS.md) for Windows and Linux cross-check commands.

## Licence and credits

This mod uses the [MIT licence](LICENSE). Ultimate ASI Loader v10.0.0 by ThirteenAG is bundled with its MIT licence. [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) contains the loader, MinHook, HDE and resolved Rust dependency notices.

## About AI assistance

TODO: the author writes this section.

### Workspace and process targets

`enshrouded-headroom` targets the Enshrouded **client only**. A positively identified dedicated server writes one skip line and installs no rendering features or hooks. Identification uses a case-insensitive executable filename and the shared known-build table. The server hash in that table is maintainer-supplied evidence. Unknown executable names remain `Unknown`; development hosts retain the existing build and byte-check guards.

`crates/emberkit` holds reusable native infrastructure as an MIT-licensed Rust library. New mods are separate crates depending on it; each declares its process targets. It has no ASI entry export. The root remains the default workspace member, so `cargo asi` keeps the existing artifact name and exports. Use `--workspace` for lint, docs and tests (`cargo fmt --all --check` for formatting). `cargo deny` traverses both workspace members from the workspace root.

`enshrouded_headroom::config::schema()` returns all 42 read settings without file I/O. Entries describe types, accepted ranges or words, default spellings, and restart requirements. All settings currently require a restart. The four developer settings under `Debug` are deliberately absent from the 38-key default INI. The three force-step defaults are absent values, not a literal `none` spelling. No settings UI is included.

### Shadow quality above Ultra (experimental)

Defaults unchanged; experimental; not verified in game. The four Shadows settings raise checked Ultra constants at startup and reserve requested extra texture VRAM. Invalid overrides use game. They do not add a menu quality level. Shader assumptions above 64 rays or 192-pixel tiles remain unverified. Compare High, Ultra and Ultra with PointLightRays=128 at the Polaris Falls bridge at night after restarting; record pop distance, VRAM and frame time.

Opt-in shadow texture reservation reduces the native texture-budget tier selected for the GPU. Below 10 GiB it subtracts from the selected stock tier without enabling a higher texture policy. With no shadow reservation, the original top-row-only behavior is unchanged.
