# Troubleshooting

Keep `enshrouded-headroom.log` from the run that had the problem. The plugin normally writes it beside its `.asi`. If that folder cannot be used, check `%LOCALAPPDATA%\enshrouded-headroom\`. The log can be replaced by a later start, so copy it before another run.

When reporting a problem, attach the full log, the `game build:` line, GPU and driver, INI changes, and steps to reproduce. Remove private paths from a copy if necessary. Save the INI as UTF-8. Close the game before changing plugin files or settings; changes take effect on the next start.

The tables quote the message templates from the source. Braces such as `{why}` and `{}` stand for values printed at runtime; hexadecimal addresses are diagnostic details, not values to edit. The timestamp and level prefix has the template `[{:>8.3}s {:<5}] {message}`. INFO often describes an intentional skip. WARN or ERROR needs the surrounding lines to identify the affected feature.

## Start-up, build check and log location

| Message template | Meaning and action |
|---|---|
| `enshrouded-headroom {} loaded` | The plugin reached logging. Record the printed version. This alone does not show which features succeeded. |
| `start-up ran twice; the second run is ignored` | Initialization was requested again. The second request does no work. Keep the log and check for duplicate plugin installations or loader configuration. |
| `settings: {}` | This is the INI actually selected. Edit this file if settings seem ignored. |
| `settings: defaults (no usable folder)` | No settings file could be used or created. Defaults apply. Read the following folder warnings. |
| `Enabled = 0 in {INI_FILE_NAME}: the mod does nothing` | The INI disabled feature start-up. Set Enabled to 1 only if you want the plugin enabled. |
| `game build: {label}; code patches {patches}` | The executable hash is known. The final word says whether code patches are allowed, not whether each feature installed. |
| `game build: unknown sha256 {}; code patches {patches}` | The executable differs from the known builds. Attach this line when requesting compatibility support. |
| `game build: unknown, not hashed ({why}); code patches {patches}` | The executable could not be identified. Resolve the printed file/path error before assuming it is a supported build. |
| `cannot find the path of the game exe` | Windows did not provide a usable executable path. Include the loader setup and full log in the report. |
| `cannot read {}: {error}` | Hashing could not read the executable. Check access and game-file integrity; retain the Windows error. |
| `AllowUnknownBuild = 1: code patches are on for a build this plugin has not been checked against; each patch still checks the game's bytes before it writes` | The compatibility override is active. Use the default AllowUnknownBuild=0 if you did not deliberately choose this risk. A byte check is not an in-game compatibility test. |
| `code patches stay off on this build; to try them anyway, set AllowUnknownBuild = 1 under [General] in {INI_FILE_NAME}` | Unknown-build protection is working. Prefer an updated plugin for the new build; the printed override is optional. |
| `cannot write to {}: {err}; logging to {} instead` | The preferred log folder failed. Read the log at the printed fallback location and check folder permissions. |
| `no preferred log folder; logging to {} instead` | No preferred folder was available. Use the printed fallback location. |

If neither folder has a log, check the loader and plugin locations and their write permissions. A missing log does not by itself prove whether the loader ran. The [README](../README.md#logs-and-troubleshooting) covers installation checks.

## Settings

The parser identifies keys by section and name. A bad value falls back or clamps as the message states. Use the shipped default INI to check spelling, choices and ranges.

| Message template | Meaning and action |
|---|---|
| `cannot read {} ({error})` | An existing INI could not be read; another candidate folder is tried. Check the path and permissions, then confirm the settings line. |
| `cannot create {} ({error})` | Creating a default INI failed. Another folder is tried. Use a writable settings location and inspect the eventual settings line. |
| `no folder for the settings file (no module folder, no %LOCALAPPDATA%); using the defaults` | Neither settings location was available. Defaults apply. Include the loader environment in a report if this repeats. |
| `the settings file cannot be read ({error}), using all defaults` | INI syntax parsing failed. Back up the file, compare it with the default INI and correct the reported syntax. |
| `[{section}] {key}: "{text}" is not {expected}, using the default {default}` | The value has the wrong syntax or choice. Use the printed expected form. This also covers non-finite numbers, invalid HDR values and fractional whole-number settings. |
| `[{section}] {key}: {value} is outside {lo}..={hi}, using {kept}` | A numeric value was clamped. Set a value within the printed limits. |
| `[{section}] {key}: {value} is outside {range}, using {kept}` | A whole-number value was clamped. The range may also permit 0, for example TargetFps. Use the printed accepted value or another value in range. |
| `[Debug] {key}: {value} is outside 0..=4, using {kept}` | A forced distance step was clamped. Remove debug overrides for ordinary use. |
| `[Governor] LowerAbove ({}) must be above RaiseBelow ({}), using the defaults {} and {}` | The two thresholds are equal or reversed. Both return to their defaults. Restore a strictly larger LowerAbove. |
| `[default]: keys before the first section header, or a section called default, are ignored` | Move each setting under its named section header. |
| `[{}]: unknown section, ignored` | Correct the section name. |
| `[{known}] {}: unknown key, ignored` | Correct the key name or remove an obsolete setting. |

Long invalid values are shortened with an ellipsis in warnings. The INI itself is not rewritten by parsing. A generic warning about an expected choice uses the current list of choices; follow that list rather than guessing an alternative.

## Texture budget

These lines describe table discovery and the first Vulkan device's budget decision. Device-local memory is a heap size, not a live free-VRAM measurement. A reported write does not establish a performance improvement.

| Message template | Meaning and action |
|---|---|
| `texture budget: off: TextureBudget = off in the settings file` | The feature is intentionally disabled. Keep it off or choose the documented setting. |
| `texture budget: table at {:#x} (top {} GiB)` | The original table was found. Wait for the device-created budget result later in the log. |
| `texture budget: off: {error}` | Table discovery or verification failed. Match the reason below. |
| `the texture budget table was not found in .rdata (or .rdata could not be read)` | The expected table could not be found/read. Check the build and other plugins, then send the log; do not patch an address manually. |
| `{n} copies of the texture budget table in .rdata, expected exactly one` | The table is ambiguous, so it is not selected. Report the build and count. |
| `the top value is not the original {} GiB: changed by another tool, such as the old launcher or another plugin` | An existing override is preserved. Check for the old launcher or another budget plugin if this was unexpected. |
| `texture budget: not applied: the settings were not handed to the Vulkan wrapper` | Startup state is incomplete. Report the full log and plugin version. |
| `texture budget: not applied: the GPU's memory heaps could not be read` | GPU memory information was unavailable. Include the GPU and driver version in a report. |
| `texture budget: unchanged (the budget is already {} GiB)` | The chosen budget equals the game's original top entry. No adjustment is needed. |
| `texture budget: {} -> {} (device-local {}{reserve})` | The checked budget write succeeded. The optional auto-mode suffix is `, reserve {reserve_gib} GiB`. Compare with the setting you chose. |
| `texture budget: left as it is, the table already holds {now} instead of the original {}` | The table changed after discovery and is preserved. Check for another writer; avoid stacking budget tools. |
| `texture budget: {} written, but the read-back did not match` | A write occurred, but its final value is uncertain. Close the game, keep the log and disable the feature while reporting the conflict. |
| `texture budget: not applied ({error})` | The checked write failed. Read the shared memory-write reasons below. |
| `texture budget: unchanged: integrated GPU (device-local {})` | Auto mode leaves an integrated GPU's budget alone. No repair is required. |
| `texture budget: unchanged: device-local {} less the {reserve_gib} GiB reserve is not above 2 GiB` | The auto calculation gives no increase over the original budget. Keep the conservative result or review the documented reserve setting. |

## Async compute

| Message template | Meaning and action |
|---|---|
| `async compute: on` | The checked startup patch was applied. The log does not verify frame-time improvement. |
| `async compute: off: {reason}` | A startup gate skipped the patch. Reasons are `unknown build`, `ini: game` or `command line sets it`. Check the build, AsyncCompute setting, or game launch options respectively. |
| `async compute: {why}` | A patch attempt returned the reason below. |
| `off: {name}: {error}` | The built-in pattern could not be parsed. Report the plugin version and log. |
| `off: {}` | The source adds a shared game-code scan reason, described under signature checks below. |
| `off: {name}: the patch site is past the end of memory` | The calculated site is invalid. The patch is refused; report the build. |
| `off: the code at {site:#x} is not the original (byte {at:#x} differs; read {found:02x?}); another tool or a different build changed it, so it is left alone` | Expected bytes differ. Check compatibility and other async-compute tools; the mismatching code was not overwritten. |
| `patch written at {site:#x}, but the read-back did not match; the code there is not what was written` | The write happened but could not be verified. Close the game, preserve the log and disable AsyncCompute pending investigation. |
| `off: cannot patch the code at {site:#x}: {other}` | A checked memory write failed. Use the shared write reasons below; retain the Windows error if present. |

## Object, grass and terrain distance

“Queued” means a hook was prepared. The final `hooks:` line reports whether the shared batch enabled. A successful installation does not prove the desired visible distance in game.

| Message template | Meaning and action |
|---|---|
| `object distance: off: code patches are off on this build` | Build protection skipped the scene hook. Check the build line. |
| `object distance: off: EnableObjectDistance, EnableScatterDistance and EnableTerrainDistance are all 0, so the scene-settings hook is not installed` | No distance feature requested the hook. This is intentional with all three switches off. |
| `object distance: on (scene-settings hook queued)` | Object-distance processing is queued. Check the later batch-enable result. |
| `object distance: off (EnableObjectDistance = 0); the scene-settings hook is queued for the other knobs and only publishes` | Object-distance changes are off, but other distance controls still need the hook's observations. No repair is needed. |
| `object distance: off: {why}` | The hook could not be prepared. Use the shared scan and MinHook reasons below. |
| `grass distance: off: code patches are off on this build` | Build protection skipped the grass redirect. Check the build line. |
| `grass distance: off: EnableScatterDistance = 0` | The grass control is disabled in the INI. |
| `grass distance: redirect installed at 90 m (the game's value); the knob stays at 90 m in this version, because no scatter capacity headroom is proven` | The redirect is installed but remains at the game's distance. This is a capacity limit, not a failed raise. |
| `grass distance: off: {why}` | The redirect failed or was refused. Match its detailed reason below. |
| `terrain distance: off: code patches are off on this build` | Build protection skipped the terrain hook. Check the build line. |
| `terrain distance: off: EnableTerrainDistance = 0` | The terrain control is disabled in the INI. |
| `terrain distance: on (terrain update hook queued)` | The terrain hook is prepared. Check the later batch-enable result. |
| `terrain distance: off: {why}` | The hook failed verification or creation. Match the shared scan, terrain or MinHook reason. |
| `hooks: {n} enabled` | The queued batch enabled successfully. The number counts hooks, not proven in-game effects. |
| `hooks: {why}` | Batch enable failed. Keep the full reason; some earlier hooks may already be live. Restart with the affected features disabled while reporting it. |

Detailed distance and hook reasons:

| Reason template | Meaning and action |
|---|---|
| `{name}: {error}` | A signature/parser error is attached to the named hook/load. Include the complete reason and build. |
| `{name}: the hook site is past the end of memory` | The hook address could not be computed safely. Report it; do not supply a manual address. |
| `the terrain body window moved during the scan` | The two verification scans disagreed. Report the build and other code-changing tools. |
| `terrain body address overflow` | The body address is invalid. Installation is refused; report it. |
| `the terrain body check did not match the verified game code` | The function body differs from the verified layout. Keep terrain disabled and send the build/log. |
| `terrain hook address overflow` | The hook address is invalid. Installation is refused; report it. |
| `the two grass distance loads read different values (changed by another tool or a different build)` | The two loads no longer agree. Check other distance mods and the build. |
| `the grass distance value is not the game's 90 m (changed by another tool or a different build)` | A baseline check refused an existing override. Avoid stacking grass-distance writers. |
| `a grass distance load reads outside the game's image (changed by another tool or a different build)` | A load points outside the expected image. The redirect is refused; report the conflict/build. |
| `no free memory within 2 GiB of the game code for the distance value` | A reachable distance page could not be allocated. Restart and report persistent failures with the other injected tools in use. |
| `{name}: the disp32 is not inside the signature` | The compiled patch description is invalid. Report the plugin version. |
| `{name}: the disp32 is past the end of memory` | The load field address is invalid. The redirect is refused; report it. |
| `{name}: the load at {at:#x} ends past the end of memory` | The instruction end cannot be computed safely. Report the build/log. |
| `{name}: the distance page at {page:#x} is out of reach of the load at {at:#x}` | The replacement float cannot be reached by that instruction. No unsafe displacement is used; report it. |
| `the distance page at {page:#x} could not be written: {error}` | Initializing the replacement page failed. Read the shared write reason. |
| `{}: the load at {:#x} was not redirected: {error}` | Redirecting the first load failed. Read the write reason and keep the log. |
| `{}: the load at {:#x} was not redirected: {error}; {brick_now}` | Redirecting the second load failed. The suffix says whether the first was restored. Report the complete line. |
| `the brick load was put back` | The earlier redirect was rolled back successfully. The grass change is off. |
| `the brick load could not be put back ({undo_error}), so it reads the mod's page, which holds 90.0` | Rollback failed; the earlier load still reads the mod's stock-distance page. Close the game and report both errors. |
| `MinHook cannot hook {target:#x}: {status:?}` | Creating a hook failed. Keep the status and check for overlapping hook tools. |
| `MinHook cannot queue the hook at {target:#x}: {status:?}` | Queueing failed. The hook was not enabled by that request; report the status. |
| `MinHook could not enable all {queued} queued hook(s) ({status:?}); hooks queued before the failing one are live` | The batch partly enabled before failing. Restart with the affected feature switches off and include the full log. |
| `{what}: panic: {message}` | A protected startup operation panicked. Report the panic and its preceding operation; do not repeatedly retry it in the same session. |

## Scatter counter probe

The probe is off by default. The debug request requires both ScatterProbe=1 and LogLevel=debug. The ordinary consumer needs a measured grass ceiling above the stock distance.

| Message template | Meaning and action |
|---|---|
| `scatter probe: off: code patches are off on this build` | Build protection skipped the probe. Check the build line. |
| `scatter probe: off: no consumer (the grass ceiling r_max is not measured yet, and no [Debug] ScatterProbe = 1 with LogLevel = debug)` | No permitted consumer requested it. This is the normal default while the ceiling is unmeasured. |
| `scatter probe: on (grass pass hook queued; it reads the GPU counters at most every 500 ms)` | The probe hook was queued with time-limited sampling. Check the final hook-enable line. This is not a capacity measurement result. |
| `scatter probe: off: {why}` | Preparing the hook failed. Use the same signature and MinHook reasons as the distance hooks. |

## HDR paper white and peak

HDR is optional. The game-value/default settings preserve the original paper white and leave peak correction off. A startup “on” line describes configuration and installation, not a verified picture on the display.

| Message template | Meaning and action |
|---|---|
| `hdr paper white: game value (setting)` | PaperWhiteNits=game keeps the game's value. No repair is needed. |
| `hdr paper white: skipped: code patches are disabled for this build` | Build protection skipped the immediate patch. Check the build line. |
| `hdr paper white: {nits} nits (was 250)` | The checked paper-white write succeeded. Confirm the configured value before judging the image. |
| `hdr paper white: {why}` | The patch was skipped or its result is uncertain. Match the following reason. |
| `skipped: value is not finite or outside 40..=400 nits` | A direct patch request was invalid. Restore a documented PaperWhiteNits value; report if a valid INI still reaches this result. |
| `skipped: {error}` | Parsing the built-in signature failed. Report the plugin version. |
| `skipped: {}` | Signature scanning failed. Use the shared game-code scan reasons below. |
| `skipped: patch address overflows` | The patch address is invalid. No patch is attempted; report it. |
| `skipped: immediate changed by another tool; left unchanged` | Another value is already present. The plugin preserves it. Check overlapping HDR tools. |
| `patch written, but read-back failed; effective value was not published` | A write occurred but its result is uncertain. Close the game and report it; peak correction is withheld. |
| `skipped: cannot patch the immediate: {other}` | The checked write failed. Read the shared memory-write reason. |
| `hdr peak: off (setting)` | FixPeak is off, the default. |
| `hdr peak: off: code patches are off on this build` | Build protection disabled peak correction. |
| `hdr peak: off: paper-white patch read-back failed; effective value is unknown` | Peak correction cannot safely use the paper-white result. Close the game and report the earlier read-back failure. |
| `hdr peak: off: all distance knobs are off; scene-settings hook is not installed` | The current peak feature needs that shared hook. Keep peak correction off if you want all distance hooks disabled. |
| `hdr peak: on (target {target} nits, paper white {})` | Peak correction is configured with the printed explicit target or game target and verified paper white. It still needs valid HDR scene calls; compare the in-game result separately. |

## Vulkan hooks and priority

| Message template | Meaning and action |
|---|---|
| `vk wrapper installed` | The import hook was installed. It does not yet prove that the game followed the chain. |
| `wrapper chain engaged (device created): GPU "{}", {}, largest device-local heap {}` | The chain saw its first successful device and queried GPU information. Check the GPU is the one you intended to use. |
| `wrapper chain engaged (device created); the GPU could not be queried` | Device creation was observed, but GPU details are missing. Include the GPU and driver in a report. |
| `vk wrapper: off: {why}` | Installation failed or was refused. Match the reason below. |
| `the exe has no KERNEL32 GetProcAddress import` | The expected import is absent. The plugin does not guess another hook site; report the build and loader setup. |
| `already installed once: the real GetProcAddress is kept as {held:#x}` | A second installation disagreed with the saved original function. Check duplicate copies and report it. |
| `not hooked: the import slot {slot:p} was not changed (see the iat warning above)` | The slot swap failed. Read the preceding IAT diagnostic. |
| `the import slot {slot:p} changed during install ({old:#x}, then {was:#x}); not hooked` | Another writer changed the slot during installation. Check other injected tools and keep the full log. |
| `vk: putting {was:#x} back into the import slot {slot:p} failed; it may hold {wrapper:#x}` | Restoring the import slot failed. Its final value is uncertain. Close the game and report the conflict. |
| `vk: the real {what} is not known; the call fails instead of calling null` | An original Vulkan function was unavailable. The plugin returns failure. Include driver, loader and other Vulkan layers in the report. |
| `vk: {name} for device {device:#x} handed back unwrapped: the game device is {game:#x}` | That function belongs to another device or an incompatible dispatch chain, so it is preserved unwrapped. If timing/features are absent, report both device values and other overlays/layers. |
| `priority: unchanged (setting)` | Priority=normal keeps the process priority unchanged. |
| `priority: AboveNormal` | Windows accepted AboveNormal priority. This is not a measured performance result. |
| `priority: could not set AboveNormal (GetLastError={error})` | Windows rejected the request. Keep normal priority or report the error; do not run the game as administrator just to force this setting. |

## Signature checks and memory writes

These details may appear directly or as a feature's reason. A refusal generally preserves the mismatching bytes. Read-back and rollback failures explicitly say that a write already occurred.

| Message or reason template | Meaning and action |
|---|---|
| `the game code was not found (another tool or a different build may have changed it)` | The required unique code pattern was absent or unreadable. Check the build and overlapping mods. |
| `the game code matched {count} times, not once (another tool or a different build may have changed it)` | The pattern is ambiguous. The plugin refuses to choose; report the build and count. |
| `bad pattern: {why}` | A built-in pattern is invalid. Report the plugin version and reason. |
| `scan: no readable PE headers at {:#x}` | The mapped executable headers could not be read. Report the build/log. |
| `scan: section {name} lies outside the module` | Section bounds are invalid. Scanning is refused; report it. |
| `scan: section {name} is empty` | The requested section has no bytes to search. Read the later feature result and report an unexpected executable layout. |
| `scan: section {name} is not fully readable` | The section could not be read safely. The scan is refused; report other code-changing tools and the build. |
| `scan: section {name} not in the image` | The expected section is absent. Check the build and file integrity. |
| `bytes at {at:#x} are not the expected ones (read {found:02x?})` | Pre-write verification failed. The mismatching range is preserved; check overlapping writers. |
| `VirtualProtect failed (error {code})` | Windows rejected a page-protection change. The attempted write is refused. Keep the code and surrounding messages for the report. |
| `read-back after the write did not match` | The write's result is uncertain. Close the game, disable the affected feature and report it. |
| `range is not committed, readable memory` | A range could not be verified readable. No direct read/write is attempted there. Report the build/log. |
| `expected and new must be non-empty and equal in length` | An internal patch request is invalid. Report the plugin version. |
| `patch: could not restore protection {old:#x} at {:#x} (error {code})` | The patch remains, but restoring the original page protection failed. Close the game and report the full log. |
| `patch: FlushInstructionCache failed for {addr:#x}` | Windows could not confirm an instruction-cache flush. Keep the warning and report persistent occurrences. |
| `patch: VirtualAlloc gave {:#x} instead of {addr:#x}, and VirtualFree of it failed (error {})` | An unexpected allocation could not be released. Restart the process and report the Windows error. |
| `iat: slot {addr:#x} is not aligned` | An import slot is invalid. It is not written; report it. |
| `iat: cannot read slot {addr:#x}: {e}` | Reading the import slot failed. Use the appended memory reason. |
| `iat: slot {addr:#x} holds 0, so it is not a bound import: not written` | The import is unbound/null. The plugin refuses to replace it. Report the loader/build setup. |
| `iat: cannot swap slot {addr:#x}: {e}` | The import replacement failed. Read the appended write reason. |
| `iat: slot {addr:#x} did not read back as written; the old value was put back` | The replacement failed verification, and rollback succeeded. The wrapper was not installed by that attempt. |
| `iat: slot {addr:#x} did not read back as written, and putting the old value back failed ({e}); the slot may hold a wrong value` | Replacement and rollback both failed. Close the game and report the full log before retrying. |

## Panics and disabled features

| Message template | Meaning and action |
|---|---|
| `{f:?} disabled: {why}` | The named feature is disabled for the rest of the session. Keep its reason and preceding messages. A restart is required to try it again; report recurring failures. |
| `panic: {message}` | This reason follows a caught feature panic. Preserve the separate panic-location line and avoid repeating the triggering setup. |
| `panic in thread '{thread}' at {file}:{line}:{column}: {message}` | A panic reached the installed panic logger. Include the location, message and full preceding log in a bug report. |
| `panic in thread '{thread}' at <unknown location>: {message}` | The panic had no source location. Report the thread/message and preceding log. |

The guard can catch Rust unwinding; these messages do not promise recovery from native memory faults or a driver failure.

## Display and frame timing

The current display and frame-timer modules emit no dedicated INFO/WARN/ERROR message. There is no display-success or frame-statistics line to look for in this version. The wrapper-chain line confirms only that device creation was observed. Include monitor refresh rate and display mode in a pacing report; do not infer an effective fps target from a missing display line.

## How this reference is checked

The coverage check inventories production `log::info!`, `log::warn!`, `log::error!` and level-selected `log::log!` calls in every Rust file under `src/`. It excludes comments and test-only items, decodes Rust string continuations/escapes, and compares each full format string with an exact inline-code span in this guide.

Some calls forward a preformatted value with `{}`, `{line}`, `{warning}`, `{advice}`, `{notice}`, `{message}` or `{text}`. Those are formatting wrappers, not literal player messages. Their source helpers and error displays are also traced into the feature/reason tables above. Debug-only tracing and test-generated messages are outside this INFO/WARN/ERROR reference.
