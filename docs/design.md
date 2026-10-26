# Enshrouded mod — design spec v2 (working name: `enshrouded-headroom`)

Status: APPROVED by the author on 2026-10-25 (version 2, with the 23 findings of the adversarial spec review folded in). Next step: the implementation plan.
Evidence and addresses: [RE-NOTES.md](RE-NOTES.md). Measurements: [performance findings](perf-findings.md).

## 1. Goal

A public mod for Enshrouded. It holds a target frame rate and uses spare performance for more graphics: first object draw distance, then grass distance, then terrain detail distance. It also corrects two fixed limits in the game, the texture budget and async compute, and adds NVIDIA Reflex on Vulkan, which the game's own menu option does not deliver.

Success criteria:

1. On a PC with headroom, draw distance and detail go up. Fps stays at or above the target, apart from short dips while the governor tests a step (at most `LowerHoldSeconds` plus the time the measurement window needs to see the dip).
2. On a PC without headroom, the game looks and runs as it does without the mod.
3. The mod never sets a value below the player's menu setting, and it never overrides an explicit player choice (the menu, or a command-line flag).
4. If the mod cannot find or apply something (for example after a game update, or if the driver refuses a change), it disables that feature, logs the reason, and the game runs normally.

## 2. Scope

In scope:
- Game build `f68140a3`, in two exes that have the same game code (checked section by section, see RE-NOTES). Both are known builds:
  - the old install's exe, sha256 `b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a`, label `f68140a3`;
  - the Steam exe (Steam buildid 26082781), sha256 `d64966e23b1a458ee6f147bcbceac0b577fa7e39a212f371287d8cc119817882`, label `f68140a3-steam`. This is the build on the author's machine now. Only the PE headers and Steam's DRM stub (the `.bind` section) differ from the old exe.
- Windows x64 with the Vulkan renderer (the game default).
- Discrete GPUs from NVIDIA, AMD and Intel. On integrated GPUs, the texture budget is left unchanged (section 6.1). Reflex is NVIDIA only.

Out of scope for version 1:
- The `--enable-dxgi` swapchain path.
- An in-game menu or overlay.
- Any setting below the player's menu choice.

Test-environment note: the test machine is the Steam install of the game (`C:\Program Files (x86)\Steam\steamapps\common\Enshrouded`). Its exe starts in Steam's DRM stub, the `.bind` section, and the folder has no `winmm.dll`. After the loader install step in plan Task 17, the folder holds one loader (Ultimate ASI Loader) and nothing else that could change what sits in the import slots. (The old install, which is being deleted, had an unexplained `winmm.dll`, 577,024 bytes, "ACME Corporation".) Ultimate ASI Loader handles `.bind`: it ignores calls from that section as triggers (from loader source v10.0.0, unconfirmed in game). The first in-game run uses the loader's `DebugLog=1`.

## 3. Constraints

- Language: Rust, latest stable toolchain. 1.99.1 was the newest on 2026-10-25; this machine has 1.98.0.
- Loader: Ultimate ASI Loader v10.0.0 (MIT), bundled with its licence text. This is a choice the author made on 2026-10-25.
- No game files ship. Game locations are found at run time by byte signature.
- Source is public on GitHub under MIT, byte signatures included. The author chose to publish them on 2026-10-25, as an exception to the project rule that third-party-derived material stays private.
- Disclosure: the code is joint work: human-designed, AI-written, human read and corrected. The author writes the disclosure text for the README and the Nexus page in the author's own words.
- The author reads all code before any public release.

## 4. Package layout

```
<game folder>\
  dinput8.dll                       Ultimate ASI Loader v10.0.0 x64 (bundled)
  ultimate-asi-loader-LICENSE.txt   MIT licence of the loader
  plugins\
    enshrouded-headroom.asi         this mod
    enshrouded-headroom.ini         settings (written with defaults if missing)
    enshrouded-headroom.log         run-time log
```

If the plugin folder is not writable, the `.log` goes to `%LOCALAPPDATA%\enshrouded-headroom\`, and the log says so. The `.ini` is looked for in the plugin folder first, then in `%LOCALAPPDATA%\enshrouded-headroom\`, and the first one that exists and can be read is used. If there is none, the default `.ini` is written to the first of the two folders where that works, so a player can also put their own `.ini` in the second folder. The log has one `settings:` line that names the file in use, or says that the defaults are in use.

How the loader runs the mod (loader source v10.0.0): it loads `*.asi` from `plugins\` and calls the export `InitializeASI()`. It does so on the game exe's first qualifying kernel32 import call, before WinMain, before the game parses its command line and before Vulkan starts. Before it calls `InitializeASI`, the loader puts back the original kernel32 import slots (`startup.cpp:152-161`). To confirm this on the author's build, the first test uses `DebugLog=1`.

The `_perf\Launch-Enshrouded.ps1` launcher from 2026-10-23 is replaced by this mod. Do not use both. If both are used, the mod finds that the texture table was already changed, logs "changed by another tool", and leaves it. It also sees `--enable-async-compute` on the command line and leaves async compute alone.

## 5. Components

| Module | One job |
|---|---|
| `entry` | Exports `InitializeASI`. Runs the start-up sequence. Every extern function catches unwinds; a panic is logged and that feature turns itself off. |
| `config` | Reads the `.ini`, writes defaults if it is missing, and clamps every value. |
| `log` | Writes the log: start-up results, every patch, every governor step. |
| `build_id` | Hashes the game exe and compares it with the list of known builds (section 9). |
| `scan` | Finds byte signatures (pattern + mask) in the game module. A signature counts only if it matches exactly once. |
| `patch` | Checks the expected original bytes, then `VirtualProtect`, write, read back, `FlushInstructionCache`. Keeps the original bytes. |
| `vk_wrap` | Wraps `GetProcAddress` → `vkGetInstanceProcAddr` → `vkGetDeviceProcAddr`, and from that chain `vkCreateDevice`, `vkCreateSwapchainKHR`, `vkQueuePresentKHR`. There is no submit wrapper (6.5). |
| `texture_budget` | Raises the top row of the VRAM-tier texture table (6.1). |
| `async_compute` | Reproduces the game's own `--enable-async-compute` override (6.2). |
| `frame_timer` | Records present-to-present times (6.3). |
| `reflex` | NVIDIA Reflex through `VK_NV_low_latency2` (6.5). |
| `antilag` | Pure AMD Anti-Lag 2 planning and sequencing (6.6); wiring is separate. |
| `vram_budget` | Reads the live VRAM budget and usage through `VK_EXT_memory_budget` when available (6.4). |
| `governor` | Pure control logic: frame times and VRAM headroom in, knob steps out. It never reads game memory (6.4). |
| `knobs` | The game-side hooks for 7.1–7.3 and the start-up patches for 7.4 and 7.5. |

**Threads and shared state.** Start-up runs on the loader's call. The hooks run on game threads. They read and write only mod-owned atomics: knob factors in, and observations out (menu baselines, frame stamps). They never block, log or allocate. The governor runs on its own thread every 50 ms. It reads only those atomics and the frame-time ring, and it never touches game memory. That avoids stale or freed game pointers (review findings 7 and 11).

## 6. Features

### 6.1 Texture budget

Fact: `FUN_1400f82c0` picks the texture budget from a 5-row table of (VRAM threshold, budget). The top row is ≥10 GiB → 2 GiB. The table is at `0x1413b86d0` and was verified from the decompile and bytes.

Rule: `budget = clamp(VRAM − reserve, 2 GiB, 8 GiB)`, with a default `reserve` of 11 GiB.
- Why 11 GiB: on the author's 16 GB card with a 6 GiB budget, the card had 15,334 of 16,303 MiB in use, which is about 1 GiB spare ([performance findings](perf-findings.md), 2026-10-23). The 6.6 GiB that the game uses outside the texture pool comes from the same file (8.6 GiB peak minus the 2 GiB pool).
- Examples: 16 GB (15.6 GiB device-local) → 4.6 GiB. 12 GB → 2 GiB, no change. 24 GB → 8 GiB.
- `.ini`: `TextureBudget = auto | off | <GiB>`.

Method:
- In `InitializeASI`, find the 80-byte table by its full contents (all 5 rows must match) and check that the top value is still `0x80000000`. If the value differs, log "changed by another tool" and stop.
- `wrap_create_device` is installed on every GPU. On the game's first `vkCreateDevice` call it reads the memory heaps of that physical device:
  - It uses the largest heap flagged `DEVICE_LOCAL`.
  - If the device type is `INTEGRATED_GPU`, it changes nothing.
  - It writes the new value and reads it back.
- The game finishes creating the device at 4.7 s (`Created Vulkan device!`, game log line 931) and reads the table at 9.4 s (`Setting texture streaming budget`, line 1236), so the write is in time (re-checked 2026-10-26).
- The old `_perf` launcher patches the same top value (to 6 GiB). With it still active, `prepare` sees a changed top value and logs "changed by another tool", so the launcher's texture patch is retired at install (Task 17).
- Later `vkCreateDevice` calls pass through untouched.

Proof that it worked: the game's own log line `Setting texture streaming budget to …`.

### 6.2 Async compute

Fact (bytes at `0x1408490cb`–`0x1408490e8`):

```
0x1408490d2: 88 4d 3e                mov  [rbp+0x3e], cl          ; auto value
0x1408490d5: 44 38 b6 89 52 03 00    cmp  [rsi+0x35289], r14b     ; has command-line override?
0x1408490dc: 74 0a                   je   0x1408490e8
0x1408490de: 0f b6 86 88 52 03 00    movzx eax, [rsi+0x35288]     ; override value
0x1408490e5: 88 45 3e                mov  [rbp+0x3e], al
0x1408490e8: 88 8d 98 0a 00 00       mov  [rbp+0xa98], cl         ; AUTO value, also feeds the texture tier flags
```

`[rbp+0xa98]` keeps the automatic value, and `FUN_1400f82c0` uses it to pick tier flags. So the patch must not change `cl` (review finding 4).

Method: patch the 16 bytes at `0x1408490d5`–`0x1408490e4` to `b0 01` (`mov al, 1`) followed by 14 bytes of NOPs. The following `mov [rbp+0x3e], al` then stores 1. This is exactly what `--enable-async-compute` produces, and the automatic byte stays untouched.

The patch is skipped if:
- any command-line argument contains `async-compute` after quotes are removed and ASCII case is ignored (this covers the game's `--enable-async-compute` and `--disable-async-compute`), because the player chose;
- the `.ini` has `AsyncCompute = game`;
- the build is unknown (section 9).

`.ini`: `AsyncCompute = on | game`, default `on` (the author, 2026-10-25).
Proof: the game log line `[render] async compute is enabled`.

### 6.3 Frame timer and the Vulkan wrapper chain

Fact: the game calls `LoadLibraryW("vulkan-1.dll")` and then `GetProcAddress(…, "vkGetInstanceProcAddr")` through the exe's import slot. It gets `vkGetDeviceProcAddr` from that, and `vkQueuePresentKHR` at device level. Hooking the exported `vkQueuePresentKHR` would be bypassed. The statically linked NVIDIA NGX code makes its own `GetProcAddress("vkGetInstanceProcAddr")` call through the same slot.

Method:
1. In `InitializeASI`, save the exe's `GetProcAddress` import slot, which the loader has already put back to the real function, and replace it with `wrap_gpa`. `wrap_gpa` always calls through the saved pointer.
2. `wrap_gpa` guards:
   - If `lpProcName` is an ordinal (value ≤ 0xFFFF), it passes the call through.
   - It compares names only when `hModule` is `vulkan-1.dll`, checked with `GetModuleHandleW`.
   - For `"vkGetInstanceProcAddr"` it returns `wrap_gipa`. NGX gets the same wrapper, which is harmless.
3. `wrap_gipa` returns `wrap_create_device` for `"vkCreateDevice"` and `wrap_gdpa` for `"vkGetDeviceProcAddr"`. `wrap_gdpa` returns `wrap_present` for `"vkQueuePresentKHR"` and `wrap_create_swapchain` for `"vkCreateSwapchainKHR"`. Every other name passes through.
4. `wrap_present` stores a `QueryPerformanceCounter` stamp in a lock-free ring (the main swapchain only), runs the Reflex steps if Reflex is active (6.5), and calls the real present.

No mid-function fallback in version 1. If the wrapper chain is not used (the log shows no `wrap_gipa` hand-out by the first present), the frame timer, the governor, Reflex and the texture budget are off for that session. The static patches still apply.

### 6.4 Governor

The author's decision (2026-10-25): "more aggressive". All values are `.ini` keys:

| Key | Default | Meaning |
|---|---|---|
| `TargetFps` | 75 | Target. `0` turns the governor off. |
| `WindowSeconds` | 2 | Rolling window for the 95th-percentile frame time (p95). |
| `RaiseBelow` | 0.90 | Raise when p95 < 90 % of the effective target frame time … |
| `RaiseHoldSeconds` | 1.0 | … for this long. |
| `SettleSeconds` | 1.0 | Wait after any change before the next decision. |
| `LowerAbove` | 0.97 | Undo the last step when p95 > 97 % of the target frame time … |
| `LowerHoldSeconds` | 0.3 | … for this long. |
| `HitchFactor` | 2.0 | Base multiplier for the post-raise hitch threshold described below. |
| `BlockSeconds` | 20 | Base block after an undo, and the probation duration. |
| `MinVramFreeMiB` | 1024 | Do not raise while free VRAM (budget − usage) is below this. |

Rules:
1. Each knob has steps 0..N. Step 0 is the player's menu value. The governor never goes below it.
2. **Only while the world is rendering.** The governor is active only when the 7.1 hook ran within the last 250 ms and presents came steadily, with no gap over 250 ms in the last 2 s. In menus, loading screens or pause it holds its current steps and decides nothing.
3. **Effective target.** The target frame time is `max(1000/TargetFps, display period)`. The display period is the refresh period of the swapchain's monitor (`EnumDisplaySettingsW` for the window's monitor). If the target fps is higher than the refresh rate, the governor holds the refresh rate instead.
4. **Fixed-refresh vsync (FIFO without VRR).** The display period must be at least half the effective target frame time. If at least 90 % of the non-long frame intervals in the window sit within ±5 % of one to four display periods, frame times are quantised. In that mode:
   - a non-long frame is on time at or below 1.05 × the effective target frame time;
   - it raises only while at least 99 % of non-long frames are on time and the window contains no long frames;
   - more than 2 % late non-long frames count as overload. The normal `LowerHoldSeconds` and `SettleSeconds` rules still apply.
   - This replaces the p95 thresholds, which would see only 8.3 or 16.7 ms (review finding 5).
5. **Raising.** It raises the next knob in priority order (section 7) that is below its maximum and not blocked, by one step. It only raises if `vram_budget` reports at least `MinVramFreeMiB` free, and does not raise a VRAM-costly knob when the budget is unknown.
6. **Lowering.** It undoes the most recent raise first (last in, first out). An undo blocks that knob for `BlockSeconds`, doubling on repeated undos up to 8 × `BlockSeconds`. A later raise that holds for `BlockSeconds`, or a menu reset, resets that knob's undo count.
7. **Hitches and long frames.** During `SettleSeconds + RaiseHoldSeconds` after a raise, a frame over the hitch threshold undoes that raise at once if it is still the most recent raise. The p95-mode threshold is `HitchFactor` × the effective target. In quantised mode it is at least one display period beyond the on-time multiple, with the 5 % margin. Frames over 100 ms and at most 250 ms stay in the window but are excluded from p95 and the quantised test. They prevent a raise. At least two such frames, or more than 2 % of the window, count as overload under the normal settle and lower-hold rules. Frames over 250 ms are stalls and never enter the window. A second stall within 10 s while the world is active undoes the latest raise.
8. **Menu changes.** The hooks publish the baseline they saw on each call. If a baseline changes, the player changed a menu setting: the governor drops that knob's added steps and starts again from the new baseline.
9. **Bounds and probation.** The steps are bounded by [0, max]. After an undo, raises are on probation until `BlockSeconds` pass without another undo. During probation, a raise also needs `BlockSeconds` since the last raise. Outside probation, raises follow the normal settle and hold rules.

VRAM budget: `vram_budget` queries `VkPhysicalDeviceMemoryBudgetPropertiesEXT` for the game's physical device every 2 s, when the driver lists `VK_EXT_memory_budget` (it does on the author's machine: game log line 502). The query holds the first device's live lease. The core command requires both the observed instance request and physical-device API version to support Vulkan 1.1. Otherwise, only an explicitly enabled `VK_KHR_get_physical_device_properties2` permits the KHR command. Unknown facts, a missing selected command or a retired device return unknown VRAM and block costly raises; the query never falls back to an unproven alias.

The governor is a pure state machine with unit tests (section 10).

The runtime reads the present ring every 50 ms using reserved frame and statistics buffers.
It refreshes the display period every 5 s and checks memory availability every 2 s.
A successful scene-hook batch is the source of the installed fact. If that hook stops advancing
for 10 s while frames arrive, the runtime logs once and holds every current step until it recovers.
A panic stops the governor before it publishes a partial knob batch. Every 30 s the info log
reports recent frame p50/p95, the effective target, all three steps and active/inactive/backoff state.
Fixed debug steps obey the same probe, grass capacity and memory gates as automatic raises.

### 6.5 NVIDIA Reflex (experimental)

Startup publishes the parsed Reflex mode before Vulkan interception. The real present wrapper
runs the Reflex sequence for an eligible live swapchain. Driver failures set the Reflex disabled
bit silently; no logging is scheduled from the present path. Fake-driver tests cover this wiring.
In-game behavior and latency improvement remain unverified.

Fact: the exe contains none of `vkLatencySleepNV`, `vkSetLatencySleepModeNV`, `vkSetLatencyMarkerNV`, `VK_NV_low_latency`, `low_latency2`, `NvAPI` or `nvapi64` (whole-file ASCII and UTF-16 search, 2026-10-25). The menu has a Reflex option (`nvidiaReflex`), but the Vulkan path cannot call Reflex, so the option is very likely inert.

Research, 2026-10-25 (`research-2026-10-25-reflex.md` in the author's notes, with a refuter check):
- The game's DLLs contain no low-latency API either.
- On Vulkan the Reflex row is not shown in the menu (the author, 2026-10-25), and no game code reads either Reflex settings field.
- The game chains its own `VkPresentId2KHR` on every present. With vsync on, it runs its own present-wait limiter (`vkWaitForPresent2KHR`, at most one queued present, 500 ms timeout).
- The mod never modifies `VkPresentInfoKHR` or its chain.

On the author's machine (game log, 2026-10-25):
- line 595: the driver offers `VK_NV_low_latency2`;
- line 892: `timelineSemaphore=VK_TRUE` in the feature listing;
- line 905: the game enables `VK_KHR_present_id2`.

Method (NVIDIA only, and only if the driver lists `VK_NV_low_latency2` revision 2 or later):
1. **Device.** Before creation the mod queries the driver extension list and timeline/present-ID feature bits. The saved menu is still unknown. Every configured mode except `off` may prepare Reflex. The mod adds `VK_NV_low_latency2`, timeline support, and present-ID support without duplicates. It preserves an enabled `VK_KHR_present_id2` path. Otherwise it enables `VK_KHR_present_id` and its queried feature. It copies each changed feature node and every prefix node it must relink; it never writes into the game's input chain. A Vulkan 1.2 feature structure takes precedence over a standalone timeline structure. Device groups with more than one device are refused.
   - Unchanged `vkCreateInstance` calls provide the requested API version and enabled `VK_KHR_get_physical_device_properties2` fact after successful creation. The observed instance generation retains those facts with its resolver. Reflex first reads physical properties through the Vulkan 1.0 command. It uses core Features2 only when both requested and physical API versions are at least 1.1; otherwise it uses Features2KHR only if that instance explicitly enabled the extension. Unknown facts or a missing selected function leave the device untouched, without trying the other alias. Pointer presence alone is not permission to use a route.
   - Wiring still conservatively requires and enables the listed `VK_KHR_timeline_semaphore` extension, including on Vulkan 1.2+, and uses its KHR wait entry point.
   - Typed copies currently support Features2, Vulkan 1.1/1.2/1.3 features, timeline, present-ID v1/v2, device group, synchronization2 and dynamic rendering nodes. An unknown device-chain layout, a cycle, or more than 64 nodes leaves the original device untouched. This is a compatibility limit, not proof that every game/driver chain is supported.
   - If the modified create call fails, the mod calls the driver exactly once with the game's **original create-info pointer**, original chain and extension list, then leaves Reflex disabled for the session (`driver refused`).
2. **Swapchain.** The mod prepends an owned `VkSwapchainLatencyCreateInfoNV` with `latencyModeEnable = TRUE`, borrowing the original tail unchanged. An already-present latency node is left alone. A rejected modified call retries the exact original pointer once and disables Reflex. A successful new swapchain gets an owned timeline semaphore and a sleep-mode command from the current decision. Recreation re-arms the new swapchain. If the saved menu becomes known later and changes that decision, the mod updates the sleep mode once.
3. **Frame IDs and attribution.** IDs start at 1 and never wrap. They are independent of the game's `VkPresentId2KHR` IDs. The mod leaves `VkPresentInfoKHR` and its chain untouched. There is **no submit hook** and no `VkLatencySubmissionPresentIdNV` chaining in v1. Markers around present and implicit driver attribution are the known limit; they do not identify individual application submissions.
4. **Present.** The exact order is `PRESENT_START`, the real present, `PRESENT_END`, `vkLatencySleepNV`, and `vkWaitSemaphores` with a **10 ms timeout**. After a successful wait, increment the frame ID, then emit `SIMULATION_START` and `RENDERSUBMIT_START`. The first timeout, lost device or out-of-date result disables Reflex. Any other sleep/wait failure also fails closed. Later presents do not sleep or wait. The original result is cached before post-work, so even a caught post-work panic cannot present again.
   - The normal present path has no allocation, blocking lock or log. Atomic non-waiting admission rejects concurrent or reentrant present streams, disables Reflex, and lets the real present run. The one game present call site is evidence for the expected serialized stream, not a general thread-safety guarantee. Already-entered driver work must return before it can observe another thread's disable.
   - Pacing occurs on the thread that presents, before its next frame. The effect on input sampling is unverified because the game's threading is not known.
5. **Lifetime and teardown.** A reader count is incremented before the active runtime pointer is loaded. Destruction detaches that pointer, then drains existing readers before freeing it. Both atomics use sequential consistency, so a reader cannot see the retired pointer while the writer sees the pre-reader zero. The device table and semaphore remain live for every present and report call. Wrapped device/swapchain destruction still retires resources when Reflex or ordinary Vulkan wrapping is disabled. Required GetProcAddress/GIPA/GDPA lookup routes also remain wrapped after disable. The game must obey Vulkan external synchronization and obtain those destroy functions through the installed chain.
   - A timed-out latency sleep can still signal its semaphore later. Retirement must drain that pending timeline value before destroying the semaphore. This can wait on a stuck driver during teardown; it is outside the normal present path. The 10 ms present timeout does not prove completion of native signaling.
   - `vk::with_game_device` provides the separate governor with a read-only lease on the first device and its enumerated physical-device parent. Only actual outputs from `vkEnumeratePhysicalDevices` or core/KHR physical-device group enumeration establish parentage. SUCCESS and INCOMPLETE outputs count; failed and count-only calls do not. The matching instance generation and real GIPA remain together. This applies to every GPU and Reflex mode, subject to observed parentage and compatible native lifetime entry points.
   - Device and instance teardown reject new leases and wait for old ones. Instance retirement removes its physical-device observations, so reused handle values cannot revive an old device context. After the first device dies the API stays unavailable. Callbacks must not destroy leased handles, retain them after returning, or launch asynchronous native work with them.
   - Unknown or ambiguous parentage fails closed. The registry retains at most 64 live instances and 256 physical-device handles per instance. Untracked handles do not enable Reflex or GPU-info queries. A conflicting first-wins lifetime function leaves the optional feature that requires it unavailable instead of replacing dispatch already issued to another instance. Destruction for an unadmitted device uses its matching original function when addresses differ. Device-query admission proves only the device destructor; swapchain retirement requires Reflex admission. The existing optional GPU-info query resolves through its proven parent; unknown parentage skips that query while preserving native creation, process priority and texture-budget policy.
6. **Measurement.** `reflex::latency_report() -> Option<String>` calls `vkGetLatencyTimingsNV` under a reader lease and formats averages for valid simulation-start-to-GPU-end and present-call timestamps. Missing timestamps produce no fabricated sample. The API has no display timestamp, so it does not claim a present-to-display measurement. The governor queries it every 5 s when debug output is enabled and logs only available reports. Formatting an emitted report may allocate; no measurement or formatting runs on the present path.

`.ini`: `Reflex = game | auto | off | on | boost`, default `auto`.
- Every enabled mode requires an NVIDIA GPU whose driver lists the extension and required capabilities.
- `auto` and `on` enable Reflex without boost; `boost` enables it with boost; `off` disables it.
- `game` follows the saved value: 0 is off, 1 is on and 2 is boost. Unknown values use the `auto` answer.
- The scene-settings post-hook reads the verified `RDX-8` u32 and `RDX-11` vsync byte only when the checked relationship `R8 == RDX + 0x54` holds. Values outside Reflex 0..2 or vsync 0..1 publish unknown. A single Relaxed atomic publishes the coherent pair, with no hook logging.
- A bad setting warns and falls back to `auto`.

W19's pure `plan_device` and `decide_mode` contracts are unchanged. W23 adds runtime wiring.
Validation uses fake native function tables and the real wrappers, including immutable chain copies,
exact-pointer retries, timeout disable, swapchain recreation and the native menu post-hook.
This is **not verified in game; the maintainer runs the boot test**. Latency improvement still needs
an in-game A/B measurement at the same spot and frame rate.

Primary API sources: [Khronos low-latency2](https://docs.vulkan.org/refpages/latest/refpages/source/VK_NV_low_latency2.html),
[latency sleep](https://docs.vulkan.org/refpages/latest/refpages/source/vkLatencySleepNV.html),
[semaphore lifetime](https://docs.vulkan.org/refpages/latest/refpages/source/vkDestroySemaphore.html), and
[latency reports](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetLatencyTimingsNV.html).
Layouts use ash 0.38.0 (Vulkan 1.3.281) and the existing present-ID2 declarations pinned to
[Vulkan-Headers v1.4.366, commit 73814d006638aa00bd553215b054f0f02e457e22](https://github.com/KhronosGroup/Vulkan-Headers/blob/73814d006638aa00bd553215b054f0f02e457e22/include/vulkan/vulkan_core.h).
### 6.6 AMD Anti-Lag 2 (experimental)

Anti-Lag is not verified on AMD hardware. W19 adds only pure planning, mode decisions, AMD ABI
types and an atomic call sequencer. It makes no driver calls. Task 16b owns the runtime wiring.

`.ini`: `AntiLag = auto | off | on`, default `auto`.
- `auto` enables Anti-Lag for AMD GPUs (vendor ID `0x1002`) only when the driver lists
  `VK_AMD_anti_lag` and reports the `antiLag` feature.
- `on` still requires the extension and feature. `off` disables it.
- A bad setting warns and falls back to `auto`.
- Reflex and Anti-Lag are never enabled together. The GPU vendor decides the automatic path;
  an active Reflex mode blocks Anti-Lag even with an explicit `on` setting.
- The device planner skips groups with more than one device. It avoids duplicate extension and
  feature-chain entries. Future wiring must query capabilities before applying it.

For one serialized present stream, the counter starts at 1. The first present has no PRESENT marker.
After it returns, the sequencer plans INPUT(2), then PRESENT(2) before the second present, then
INPUT(3), and so on. The INPUT stage is an approximation, called after the previous present returns.
Future wiring must execute every returned call in order. The sequencer itself sends nothing.

A lost device disables the sequencer at once. Three consecutive driver updates taking more than
100 ms disable it for the session; an update at or below 100 ms resets the streak. The caller
measures these durations and logs the single disable transition. Counter exhaustion also stops
planning rather than wrapping a frame ID. The data helper sets ON with no frame-rate cap and
clears both extension chains. Its presentation-info pointer must remain valid through the call.

## 7. Knobs

Each knob states:
- what it changes;
- where the value lives;
- how the baseline is read;
- the steps and the maximum;
- when a change applies;
- the evidence.

"Floor" is the game's own value for the player's menu setting. Hooks publish baselines to atomics and read knob factors from atomics; they never wait.

### 7.1 Object draw distance (`ObjectDistance`): priority 1

- **What it changes:** two pixel-size multipliers that decide when small or far objects are culled.
  - `scene+0x33c` feeds the main GPU culling pass.
  - `scene+0x340` feeds the model-chunk pass.
  - For Models = Performance / Balance / Quality, the game writes 3.0 / 2.0 / 1.0 to `+0x33c` and 1.5 / 1.0 / 1.0 to `+0x340` (REA ev_5e12d612).
- **Where it lives:** a minhook post-hook on `FUN_1400f72b0` (signature in RE-NOTES). After the original returns, the hook:
  - publishes the baseline values and the detail-scattering level (`scene+0x290`) to atomics;
  - multiplies both fields by the current knob factor;
  - records the time it ran, which the governor uses as the world-rendering signal (6.4 rule 2).
- **Baseline:** what the original just wrote. It is rebuilt on every call (a stack struct, one call site `0x1408576af`), so a menu change is the new floor at once.
- **Steps:** factor 1.0 → 0.85 → 0.72 → 0.61 → 0.52. The clamp is `[0.5, 1.0]`.
- **Direction:**
  - `FUN_1400aa5a0` returns projection ratio × 1/(max(w,h)/2) × multiplier (REA ev_9b134185). A larger value culls more, and the game's presets fall from 3.0 to 1.0 as quality rises. So a factor below 1.0 should draw farther.
  - **The first in-game test confirms this. If it is wrong, this knob ships disabled.**
  - The separate constant `pixelSizeCullingBias = 2.0` (`0x1400aded2`) is not touched.
- **Limits:** `meshCount` is fixed at 0x20000. Overflow would drop or flicker objects, which is why the clamp stops at 0.5. Check in game at the maximum step.

### 7.2 Grass and detail-scatter distance (`ScatterDistance`): priority 2

- **What it changes:** the High-level grass distance, which the game checks in two places. Both use 70 / 80 / 90 m for Low / Medium / High (bytes verified).
  - per instance: `FUN_1400767a0`;
  - per terrain brick: `FUN_1400794f0`.
- **Where it lives:** the two High-level `movss` loads.
  - The per-instance distance disp32 is at match +30 of its signature.
  - The brick disp32 is at match +9 of its signature (both in RE-NOTES).
  - At start-up the mod allocates one page within ±2 GB of the game image, writes one float (90.0) into it, and points both disp32 fields at that float.
  - The shared `.rdata` constant 90.0 has 27 other users and is never written.
- **Capacity (research 2026-10-25, which replaces the doubling plan):**
  - The buffers are fixed per scene view: `ScatteredObjectData[0x14000]` and two `DetailScatteringInstanceData[0x14000]`. One GPU counter (A) fills the 0x14000 budget for both scatter passes each frame. The game reads that counter back to the CPU: last value at entry+0x14, peak at +0x1c, a warn level of 0xf000 (75 %) at +0x20, and the cap of 0x14000 at +0x24.
  - The exe has no CPU-side clamp. The only bound is the shader constant `scatteredObjectCapacity` (`0x140074d52`). The 0x14000 at `0x1400745e0` is counter METADATA, not a buffer size, so patching it would cause an overflow. **Capacity is never changed.**
  - **Scatter counter probe (7.2a):** a pre-hook on `FUN_1400767a0` publishes only the pass pointer and a call counter. A sampler thread copies the GPU counter blocks every 500 ms while calls continue. It self-checks A.warn = 0xf000 and A.cap = 0x14000 before publishing a complete sample. A layout mismatch or ten consecutive misses leaves the probe off and the knob at 90 m.
  - **Live headroom rule:** the governor raises this knob from r to r_next only while `last × (r_next / r)² < 0xf000`, and steps down at once when `last ≥ 0xf000`. The readback is at least one frame late; the 25 % gap between warn and cap is the margin.
  - **Hard ceiling:** r_max = 90 × sqrt(0xf000 / peak₉₀), where peak₉₀ is measured once at vanilla 90 m in the densest grass (the author's measurement session, `[Debug] ScatterProbe = 1`). Until it is measured, the ceiling is 90 m and the knob is inactive.
  - Not covered: the brick gate's own `maxCullElements` (`BrickCullingShaderParameters` +0xec8). The measurement session also watches for missing bricks at the raised distance.
- **Baseline:** the knob acts only when the published detail-scattering level is High (3). At a lower level, the player's choice is the floor and the knob stays at step 0.
- **Steps:** 90 → 100 → 110 → 120 → 127 m, cut at the measured ceiling r_max. The live headroom rule can hold the knob below that.
- **When a change applies:** the governor writes the float in the mod's page, an aligned 4-byte store, and both passes read it on their next run.

### 7.2a Scatter counter probe (the author, 2026-10-25: "Add it to v1")

- **Hook:** a minhook pre-hook on `FUN_1400767a0(rcx, pass, f32 in XMM2)`. Its signature is the 14-byte prologue `48 89 54 24 10 55 53 57 41 55 41 56 41 57`, unique in build f68140a3 (checked 2026-10-26). Like the terrain signature it is fragile across updates; a mismatch leaves the probe off.
- **Pointer chain** (research write-up; a HYPOTHESIS until the runtime self-check passes):
  - obj = [pass+0x18];
  - pool = [[pass+0x8]+0x20];
  - entry(h) = [pool+0x48] + (h & 0xffff) × 0x40, for h = [obj+0x8] (counter A), [obj+0xc] (B) and [obj+0x10] (C).
  - The sampler copies 7 blocks with `ReadProcessMemory(GetCurrentProcess(), ...)`: `pass` +0x8..+0x20, owner +0x20..+0x28, pool +0x48..+0x50, obj +0x8..+0x14, and each entry +0x14..+0x3c. A copy is accepted only if the API succeeds and reports the exact requested byte count. Checked field reads parse only these local buffers; Rust never dereferences a game address.
- **When it is installed:** only with `[Debug] ScatterProbe = 1` and `LogLevel = debug`, or with `EnableScatterDistance = 1` once the measured ceiling r_max allows a raise (`ceiling_step(SCATTER_STEPS, r_max) > 0`). Until r_max is measured, a default install has no probe hook or sampler thread. An off log names both failed halves of this rule.
- **Reason for the sampler:** the in-game pulse remained with the earlier 500 ms hook sampler and its 7 `VirtualQuery` checks. Contention on the process address-space lock is a HYPOTHESIS. All sampling work now runs off the game thread. Not verified in game; the maintainer boot-tests it with the probe on.
- **Rules:**
  - The guarded hook stores the latest pass pointer with `Release`, increments a call counter with `Relaxed`, and calls the original exactly once with unchanged arguments. It does not read the clock or game memory, allocate, lock or log.
  - Only after the wanted hook installs, start one detached thread named `scatter-probe`. It sleeps 500 ms without holding a lock. If the call counter is unchanged, it reads nothing. Otherwise it loads the pass pointer with `Acquire` and attempts one sample. Its body runs inside `guard(Feature::ScatterDistance, ...)`; a panic stops it and latches `FAILED`.
  - A failed or short copy, zero handle or ownership mismatch skips that sample. Nine consecutive misses are tolerated; the tenth while calls continue sets `FAILED` and clears `PROBE_OK`. A good complete sample resets the miss count to zero. An unchanged call counter neither reads memory nor counts another miss.
  - A correctly copied counter A must have warn = 0xf000 and cap = 0x14000. A mismatch fails at once. No partial sample is published. Failure stays latched for the session; the governor logs it once.
- **Debug output:** with `[Debug] ScatterProbe = 1`, `LogLevel = debug` and the governor running (`TargetFps > 0`), each 2 s counter line logs A, B and C (last and peak), followed by `session_peak_A`. The governor observes `PEAK_A` every 50 ms and retains the largest value seen during that run across scene-view resets. A peak that resets between observations may be missed.
- **Measurement session (the author, once per build):** grass quality High, mod distance left at vanilla 90 m, stand in the densest grass for 60 s, then send the log. The controller computes r_max and records it in the RE notes. The release uses r_max as the knob's ceiling.

### 7.3 Terrain detail distance (`TerrainDistance`): priority 3

- **What it changes:** `dist0`, the base distance of the voxel-terrain LOD octree. A node refines while `dist × 0.969 ≤ cell × R`, where cell = 32/2^level and R = dist0 × min(2·scale, 1) (agent knob-terrain). With dist0 = 32, full detail reaches about 33 m and each coarser band doubles.
- **Where it lives:** `FUN_1401e2e80(mgr, camPos, params)` copies 40 bytes (0x28) from `params` into `mgr+0xc478` on every call (verified at `0x1401e2eed`–`0x1401e2f0c`). `camPos` points at three i64 values. The defaults are 32 / 128 / 256 (verified at `0x1400fa623`–`0x1400fa631`). A minhook pre-hook multiplies `params[0]` by the knob factor before calling the original. It reads and writes only the first 32 bytes of the params buffer; the conditional change to `params[1]` is described under Steps below.
- **Not used:** `fullyMeshedRange` (`mgr+0xc444`). It is a per-frame measurement, rolled over at `0x1401e2ee4`.
- **Baseline:** the incoming `params[0]`, published to an atomic.
- **Steps:** factor 1.0 → 1.25 → 1.5 → 1.75 → 2.0. If dist1 < dist0 × factor, dist1 is raised by the same factor, so that the order holds. dist2 is unchanged.
- **When a change applies:** at the next update. The engine's own adaptive meshing factor (`mgr+0xc438`, 0.1–1.0) still pulls ranges back if meshing is overloaded.
- **To check in the build:** that the menu path does not re-copy over the value between the hook and its use, and the meshing and VRAM cost per step.

### 7.4 Deco and distant-object distance thresholds (start-up only)

- **What it changes:** `distanceCulling[0/1].threshold` in the model-chunk culling parameters.
  - Disabled+Low / Medium / High: deco 4.0 / 3.0 / 1.5, distant 6.0 / 4.0 / 2.5 (bytes verified).
  - A lower value should draw farther. That is inferred and must be confirmed in game; the default is off.
- **Where it lives:** code immediates. Deco High is at `0x1400d77cc`, which is aligned. Distant High is at `0x1400d781f`, which is not aligned.
- **Method:** written once, in `InitializeASI`, before the game runs that code. Never written live, because that would be cross-modifying code.
- **`.ini`:** `DecoDistanceScale` and `DistantObjectsScale`, default 1.0 (off), range [0.5, 1.0].
- **Later option:** a live post-hook on the culling-parameter builder (the block is copied to `scene+0x1e20`). Not in version 1.

### 7.5 Terrain LOD ring ranges (start-up only, optional)

- **What it changes:** the voxel-mesh config. Voxel Mesh Quality High sets 32 / 128 / 160 through immediates at `0x1400f7f2e` / `0x1400f7f35` / `0x1400f7f3c` (bytes verified). This config path reallocates scene-view memory whenever it changes (`0x1400f1cb0`), so it is start-up only.
- **`.ini`:** `TerrainRingScale`, default 1.0 (off), range [1.0, 1.6]. Kept separate from 7.3 until the build shows how the two relate.

### Priority order

The governor raises 7.1, then 7.2, then 7.3. The order is re-checked after the in-game cost test. 7.4 and 7.5 are start-up settings.

## 8. Settings file

`[General]`:
- `Enabled = 1 | 0`
- `TextureBudget = auto | off | <GiB>` (reserve key `TextureReserveGiB = 11`)
- `AsyncCompute = on | game`
- `Priority = above_normal | normal` (default `above_normal`; `normal` leaves the current priority unchanged)
- `Reflex = game | auto | off | on | boost` (default `auto`)
- `AntiLag = auto | off | on` (default `auto`)
- `AllowUnknownBuild = 0 | 1`
- `LogLevel = error | warn | info | debug`

`[Hdr]`:

- `PaperWhiteNits = game | <number>` (default `game`; finite 40..=400 nits). Invalid or out-of-range values warn and use `game`.
- `FixPeak = on | off` (default `off` until the author's in-game A/B test).
- `DisplayPeakNits = game | <whole number>` (default `game`; 80..=10000 nits). Used only with `FixPeak = on`. Invalid or out-of-range values warn and use `game`. Decimal commas with zero fraction are accepted.

`[Governor]`: the keys in 6.4.

`[Knobs]`:
- `EnableObjectDistance = 1`, `MaxObjectDistance = 4`
- `EnableScatterDistance = 1`, `MaxScatterDistance = 4`
- `EnableTerrainDistance = 1`, `MaxTerrainDistance = 4`
- `DecoDistanceScale = 1.0`, `DistantObjectsScale = 1.0`, `TerrainRingScale = 1.0`

`[Debug]` (debug only, needs `LogLevel = debug`; not in the default file; at any other log level the mod ignores these keys):
- `ScatterProbe = 0 | 1` (default 0): enable the publish-only hook and its 500 ms sampler thread, and log the scatter counters (7.2a). Until r_max is measured, this key with `LogLevel = debug` is the only reason to install them.
- `ForceStepObjectDistance`, `ForceStepScatterDistance`, `ForceStepTerrainDistance = 0..=4` (default: absent, the knob is not held): hold that knob at a fixed step for testing (pre-flight ruling for Task 17). A whole number outside 0..=4 is clamped with a warning. Other text warns and counts as absent.

Unknown keys and invalid values are logged. Invalid choices use their defaults.
Out-of-range numeric values are clamped, except `PaperWhiteNits` and `DisplayPeakNits`, which fall back to `game`.

### HDR paper white

The HDR composite pass multiplies scene and UI by a hard-coded 250.0f before PQ encoding.
`PaperWhiteNits = game` leaves it unchanged. A finite number from 40 through 400 replaces
only that four-byte immediate, once at start-up, when `code_patches_allowed()` permits it.
The mod requires one `HDR_PAPER_WHITE` match in `.text` and checks `00 00 7a 43` before
writing the little-endian f32 bits. A changed immediate is left alone and logged as changed
by another tool. Each outcome produces one `hdr paper white:` line.

The effective-value accessor starts at 250 and publishes the chosen number only after a
successful checked write. Skips and failed writes do not publish the configured number.
It does not measure another tool's replacement value. A typed read-back failure also
marks the value unverified, because the immediate may have changed despite the numeric
getter staying at its last verified value. Dependent peak correction stays off in that case.

The default remains `game` until the in-game A/B test (Task 17). The research proposes
trying 80 nits first. The scale mismatch and visible HDR improvement remain hypotheses
until that test. Shader replacement and HDR metadata are later tasks.
See `docs/re-notes.md`, "HDR paper white", for the verified instruction and context.

### HDR tonemapper peak

With `FixPeak = on`, the Task 12 post-hook corrects the u16 at scene+0x41e only when the
colour-space byte at scene+0x408 is 1 or 2. It reads the game's fresh peak on each call,
then uses `DisplayPeakNits` when set. It writes `round(target * 80 / paperWhite)`, clamped
to 1..=10000, with an unaligned store. Invalid paper white skips the write. Paper white
comes from the same effective-value accessor as W13, including its 250-nit fallback.
Windows' full-frame luminance can be below the panel's true peak, so an explicit panel
target can replace the game's figure.

Start-up configures and logs this feature after paper white and before queued hook enable.
A failed paper-white read-back keeps it off and produces one start-up reason. It requires
code hooks to be allowed and at least one distance knob to be enabled. With
all distance knobs off, the existing Task 12 gate omits the hook; peak correction stays
off and logs why. A failed scene-hook installation is reported by `object distance:`.
The correction runs before object publication or scaling inside Task 12's inner guard.
Its normal hook path allocates nothing, takes no lock and logs nothing. A panic there
leaves the original scene untouched, disables the shared scene feature and does not run
the original again. If the game skips the scene-settings call, it retains its own peak.

The default remains off until the in-game A/B test. The shader-side chain and visible
roll-off remain hypotheses, with the limits recorded in `docs/re-notes.md`,
"HDR tonemapper peak (2026-10-26; plan route B2)".

## 9. Failure handling

- **Features are independent.** A missing signature, a wrong original byte, or a failed read-back turns that feature off and logs why. The others continue.
- **Signatures** must match exactly once.
- **Known builds only.** The mod hashes the exe at start-up.
  - On a known build, all features apply.
  - On an unknown build, only API-level features apply: the frame timer, the governor's timing, Reflex, and the texture budget, which needs a full 80-byte table match. Code patches and code hooks stay off, unless `AllowUnknownBuild = 1` (review finding 10). The log says which applied.
  - A game update therefore turns the knobs off until the mod is updated, instead of risking a crash.
- **Driver refusals.** Changed `vkCreateDevice` and `vkCreateSwapchainKHR` calls are retried with the game's original arguments if they fail. The feature then stays off.
- **No waits without timeouts** on game threads. The Reflex present wait has a 10 ms timeout (6.5). Pending semaphore retirement can wait longer during teardown, as documented in 6.5.
- **Panics.** Every extern function catches unwinds and falls back to the original.
- **Memory access.** Hooks read only the arguments the game passes in, at that moment. The governor never reads game memory. Access violations cannot be caught by `catch_unwind`, so the design avoids the stale pointers that would cause them (review finding 11).
- **Original bytes** are kept for every patch. They are not restored at process exit.

## 10. Testing

Unit tests (`cargo test`, no game):
- `scan`: masks, the exactly-once rule, no match, two matches.
- `patch`: refuses when the original bytes differ.
- `governor` traces:
  - steady headroom: one raise per settle;
  - sudden load: lowered within `LowerHoldSeconds` after the window registers it;
  - a hitch after a raise: that raise is undone and blocked;
  - oscillation: blocking stops it;
  - menu change: the baseline is reset;
  - loading and menus: no decisions;
  - FIFO quantised at 60/120 Hz: quantised mode, no flapping;
  - target above refresh: the refresh rate is held;
  - VRAM below `MinVramFreeMiB`: no raise.
- `governor` bounds and probation: steps stay within [0, max]. After an undo, raises during probation need `BlockSeconds` since the last raise. Steady headroom outside probation can raise after each settle and hold.
- `config`: defaults, clamping, unknown keys, missing file, read-only folder.
- Texture budget formula: 8, 10, 12, 16 and 24 GB, and integrated GPUs.

In-game checks on the author's machine:
1. **Loader log.** `DebugLog=1` shows the trigger API and that the plugin loaded.
2. **Game log.** It shows the new texture budget and `async compute is enabled`.
3. **Mod log.** It shows the build as known, the wrapper chain engaged, and the governor steps.
4. **Direction test, per knob.** At a fixed spot, set each knob to its maximum by `.ini` and look. The distance must visibly go up. A knob that fails ships disabled.
5. **Raise test.** the author's rig was GPU-bound at max settings (96 % GPU, 68 fps). So the raise test lowers the menu baseline (for example Models = Balance) or uses a light scene, to create headroom. Check with PresentMon that fps holds the target apart from brief probe dips.
6. **Reflex.** Logged latency on versus off, at the same spot and the same fps.
7. **Vsync.** Run with vsync on and no VRR: the governor holds the refresh multiple without flapping.
8. **Soak.** 30 minutes of play with no crash or hang. Include an alt-tab and a resolution change, which re-create the swapchain.

## 11. Build

- **Output:** a `cdylib`, written straight to `.asi` with `cargo rustc --release --lib -- -C link-arg=/OUT:<dir>\enshrouded-headroom.asi`. That command produced a loadable `.asi` in the probe.
- **Profile** (tested by the Rust-stack agent in a probe host):
  - `panic = "unwind"`, with `catch_unwind` at every extern boundary. A panic hook logs, and a kill switch makes hooks tail-call the original. With `panic = "abort"`, `catch_unwind` does nothing.
  - `lto = "fat"`, `codegen-units = 1`, `opt-level = 3`, `debug = "line-tables-only"`.
  - Static CRT (`+crt-static`): no VCRUNTIME, MSVCP or UCRT import. The Task 1 artefact imports `KERNEL32.dll`/`kernel32.dll`, `ntdll.dll` and `api-ms-win-core-synch-l1-2-0.dll` (Rust std's `WaitOnAddress`, a Windows 8+ api-set). Release checks therefore test that the CRT DLLs are absent; they do not use a whitelist.
- **Toolchain:** update to the latest stable before the first build.
- **Crates:** latest on crates.io on 2026-10-25, vetted, probe built and loaded:

  | Crate | Version | Why |
  |---|---|---|
  | `windows-sys` | 0.61.2 | Win32 bindings; 1 transitive dependency. Note: `BOOL` is now `windows_sys::core::BOOL`. |
  | `minhook` | 0.9.0 | Inline hooks. Upstream MinHook freezes other threads while installing. Stress test: 300 cycles with 6 busy threads, exit 0 (`retour` and `detour` crashed 5 of 5). `initialize()` can panic, so it runs inside `catch_unwind`. Transitive (from `cargo tree`): `tracing` 0.1.44 with `tracing-core`, `pin-project-lite`, `once_cell` and the build-time proc-macro set (`tracing-attributes`, `proc-macro2`, `quote`, `syn`, `unicode-ident`); `log` (already direct); and the build-time C driver `cc` 1.10.0 with `find-msvc-tools` and `shlex`. |
  | `ash` | 0.38.0, `default-features = false` | Vulkan types only; no dependencies. Missing newer structs are defined by hand (6.5). |
  | `configparser` | 3.3.0 | Ini parsing; no dependencies. Dual MIT / LGPL-3.0: we elect MIT. |
  | `log` + `fern` | 0.4.34 + 0.7.1 | File logging; 1 extra dependency. |
  | (hand-written) | — | Signature scanner, about 30 lines, bounded by `VirtualQuery`. |

  Rejected:
  - `retour`: no stable release since 2023, and it does not freeze threads.
  - `ilhook`: not thread-safe, by its own docs.
  - `detour` 0.10.0: 25 days old; on a watch list.
  - `rust-ini`: non-reproducible builds.
  - `tracing-appender`: 23 dependencies.

## 12. Release

1. The author reads all code.
2. Public GitHub repo at `C:\Modding\Enshrouded`, MIT licence. The README covers what the mod does, install, uninstall, the `.ini` reference, the tested build, and the disclosure in the author's words.
3. The Nexus zip has the section 4 layout and the loader's MIT licence.
4. The mod page states the tested game build and that a game update turns the knobs off until the mod is updated (section 9).

## 13. Decisions (the author, 2026-10-25)

1. Async compute default: `on` for everyone.
2. Byte signatures: published in the public repo (an exception to the private rule).
3. Reflex default: `auto`, meaning on for supported NVIDIA devices. `game` is retained and follows the saved 0/1/2 value; unknown values use the auto answer. The in-game option is not shown on Vulkan.
4. Licence: MIT.
5. Name: `enshrouded-headroom` for now; rename before the Nexus release.
6. Spec v2: approved.
