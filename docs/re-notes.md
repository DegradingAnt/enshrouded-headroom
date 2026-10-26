# Enshrouded reverse-engineering notes

Target: `enshrouded.exe`, sha256 `b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a`, build `f68140a3`, image base `0x140000000`. The Steam exe of the same build has the same code at the same addresses (last section, "The Steam exe"); every virtual address in these notes holds for both.
Tools: REA 6.3.0 + Ghidra 12.1.4, and capstone/pefile scratch probes (`xref.py`, `callers.py`, `func.py`, `stores.py` in the session scratchpad).
Evidence labels: **VERIFIED** = read from bytes, decompile, or game log. **HYPOTHESIS** = inferred from names or neighbours.

## Shipped in the launcher ([performance findings](perf-findings.md), 2026-10-23)

- Texture-streaming budget. `FUN_1400f82c0` picks the budget from table `0x1413b86d0`: 5 rows of (VRAM threshold, budget). The top row is ≥10 GiB → 2 GiB. **VERIFIED** (decompile and log).
- `--enable-async-compute`. The parse at `0x14084f711` sets an optional bool (value at `+0x2a3`, has-value at `+0x2a4`). The renderer create call at `0x14084912f` uses it in place of the auto value from `0x1404b14d0`. **VERIFIED** (bytes and log).
- The game disables third-party Vulkan layers itself, unless `--keep-vulkan-layers` is passed. **VERIFIED** (strings `0x1da8360`–`0x1da84f8`).

## Render-settings struct (menu options)

Reflection field descriptors at `0x1417b5780` (stride 0x30: name, nameLen, type, offset, …). **VERIFIED**:
`fog` +0x2c, `ssao` +0x30, `distantObjects` +0x34, `decoObjects` +0x38, `detailScattering` +0x3c, `voxelMeshQuality` +0x40.
Enum types: `keen::pc::RenderDistantObjectsSetting` and similar; member tables at `0x141b39e50` (distant), `0x141b3a950` (deco), `0x141b3b390` (detail), `0x141b3c1a0` (voxel). The member names are not yet decoded.

## Distance-parameter candidates

| Field | Owner (descriptor neighbours) | Writer found | Status |
|---|---|---|---|
| `pixelSizeCullingBias` (+0xffc) | model-instance culling parameters (`forceLodIndex` +0xff0, `cpuInstanceCount`, `batchCount`) | `0x140b87fd5` writes constants 1.0, 10.0, 1.0 in three branches | The writer is **VERIFIED**. That it controls object draw distance is a **HYPOTHESIS**. |
| `fullyMeshedRange` (+0xec4) | BrickCullingShaderParameters | real writer `FUN_1400503c0` (0x140050994) from `mgr+0xc444` | **CORRECTED** (agent knob-terrain): a per-frame MEASUREMENT, not a setting. `0x1401f43c8` is the WATER pass `linearDepthParameters.y`; the offset match was a coincidence. The live terrain knob is dist0 at `FUN_1401e2e80` params[0] (defaults 32/128/256 at `0x1400fa623`, VERIFIED by me). |
| `maxDetailScatteringDistance` (+0xed0) | same descriptor block | `0x1401f4448` writes an INTEGER (`[r14] >> 5`) at +0xed0 | The name/offset match is **UNVERIFIED**: either the struct base differs or the name belongs elsewhere. |
| `maxScatterDistance` (+0xe0), `distanceCullingThreshold` (+0xd4), `sizeCullingPixelSize` (+0xd8) | detail-scatter (grass) parameters | not searched yet | — |
| `explicitLodDistances` | per-model asset data | — | Per model, not a global knob. |
| `cullingDistanceMin/Max` | VFX/particle data | — | Not a draw-distance knob. |

## Knob findings, verified by me against the bytes (2026-10-25)

- **Grass / detail scatter.** The per-level constants are chosen by a switch on `[renderView+0x3f8]` in `FUN_1400767a0` (agent knob-scatter). I read the floats myself:
  level 1: density 0.3, distanceCullingThreshold 19, maxScatterDistance 70 m;
  level 2: 0.6 / 16 / 80 m;
  level 3: 1.0 / 14 / 90 m. Level 0 is all zero, i.e. off.
  The constants sit in shared rdata (`0x1413b65d0`..`0x1413b65fc`, `0x1413b3c90`, `0x1413b4080`), and other code may also use them, so patch the loads or the context, not the rdata.
  They are copied into `DetailScatteringShaderData` (+0xcc, +0xd4, +0xe0) by `FUN_140074b50` every pass, so a change takes effect at run time (HYPOTHESIS until tested in game). The fade band is a fixed 50 m (`invMaxDistanceFadeLength` = 0.02).
  VERIFIED by me (disassembly 0x1400768b0..0x140076990): `0x1400768d5 mov ecx,[r13+0x3f8]` is the level; level 3 loads at `0x140076925` (`movss xmm6,[rip+..]` = 1.0), `0x14007692d` (xmm7 = 14), `0x140076935` (xmm8 = 90). Levels 2/1 at `0x140076940`/`0x14007695b`, level 0 = xorps.
  Encodings: `0x140076925 f3 0f 10 35 <disp32>` (disp at +4), `0x14007692d f3 0f 10 3d <disp32>` (+4), `0x140076935 f3 44 0f 10 05 <disp32>` (+5). The three loads with the disp wildcarded match **8 times** in the file, so the signature must also take in the preceding `cmp ecx,1 / jne rel32` and the following `jmp rel8`, and must pass the exactly-once test.
  UNIQUE signature (tested, 1 match at file offset 0x75d1c = VA 0x14007691c): `83 f9 01 0f 85 ?? ?? ?? ?? f3 0f 10 35 ?? ?? ?? ?? f3 0f 10 3d ?? ?? ?? ?? f3 44 0f 10 05 ?? ?? ?? ?? eb ??`. The level-3 disps are at match +13, +21 and +30.
  **Knob method (generic):** allocate a page within ±2 GB of the image, copy the original floats into it, and rewrite the disp32 of those `movss [rip+disp]` loads to point at the page. The governor then changes the floats live with aligned 4-byte stores. The shared `.rdata` constants are never touched.
- **Object culling.** The real writer of `GpuCulling2ShaderParameters` is `FUN_1400ad0f0`. The instruction at `0x1400aded2` (file offset `0xad2d2`) is `c7 85 cc 25 00 00 00 00 00 40` = `pixelSizeCullingBias = 2.0`, constant and unconditional; I read it myself. The earlier `0x140b87fd5` lead was a false positive (a stack temp). Whether a lower or higher value draws more objects is still open (agent knob-culling).

## Menu settings → scene settings: `FUN_1400f72b0(scene, renderSettings, ...)` (VERIFIED, REA decompile ev_5e12d612)

- models (+0x50): 0 → scene+0x33c = 3.0, +0x340 = 1.5, +0x344 = 1.5; 1 → 2.0 / 1.0 / 1.2; 2 → 1.0 / 1.0 / 1.0; +0x338 = 1.0 always.
  scene+0x33c → view+0x218 → the pixelSize multiplier in GpuCulling2 (agent knob-culling, F4/F5), so a LOWER value culls less. Formula VERIFIED (REA ev_9b134185, `FUN_1400aa5a0`): pixelSize = projection ratio × 1/(max(w,h)/2) × multiplier. The direction agrees with the game's own mapping (lowest level 3.0, highest 1.0). It still needs an in-game look.
  The author's current log line 221: `"models": "Quality"`. That Quality = level 2 is a HYPOTHESIS until the enum is decoded.
- decoObjects (+0x38) → scene+0x294 (byte 0..3). distantObjects (+0x34) → scene+0x295 (byte 0..3). detailScattering (+0x3c) → scene+0x290 (dword 0..3; probably the source of renderView+0x3f8, HYPOTHESIS).
- fog (+0x2c) → scene+0x328/+0x32c (0.45 / 0.4 / 0.3 / 0.2) and +0x334. tessellation (+0x44) → +0x2ec..+0x304. textureAnisotropy (+0x4c) → +0x420 (1/2/4/8/16).
- **Knob design that follows:** a minhook post-hook on `FUN_1400f72b0` calls the original, keeps the `scene` pointer, and then overwrites only the raised fields. A value below 1.0 at +0x33c goes past Quality; the game's own values stay as the floor. Call frequency: callers.py finds ONE call site, `0x1408576af`, with `lea rcx,[rbp-0x10]`. The scene-settings struct is a STACK LOCAL of the caller, rebuilt on every call, so a direct write between calls cannot hold. The caller then calls the scene renderer `0x1400f4ae0` (agent knob-culling F5), so this probably runs once per rendered frame (HYPOTHESIS-strong). Conclusion: the post-hook is the right method, and a governor change applies on the next frame.
- **Hook site (VERIFIED, REA/Ghidra ev_7ac38aaf, exe sha256 b28b448a…; the Steam exe has the same code):**
  - The function is a leaf. It makes no calls.
  - The first two instructions are `MOV EAX,[R8]` (3 bytes, at 0x1400f72b0) and the RIP-relative `MOVSS XMM1,[rip+disp32]` (8 bytes, at 0x1400f72b3). Together they are 11 bytes and end at 0x1400f72bb. MinHook copies them into its trampoline and relocates the RIP-relative one.
  - The lowest branch target in the body is 0x1400f72e7 (entry+0x37). So no branch jumps into the bytes that MinHook replaces.
  - The function writes only 3.0, 2.0 or 1.0 to +0x33c (models 0, 1 and 2).
  - For a models value of 3 or more, the `JNZ` at 0x1400f7694 goes to the `RET` at 0x1400f7710. +0x33c and +0x340 are then not written.
  - So the Task 12 hook publishes and scales +0x33c only when it holds exactly 3.0, 2.0 or 1.0. It leaves any other value alone and counts it.


## Menu enums and the two apply paths (agent knob-menu, VERIFIED)

- Enums: distantObjects / decoObjects / detailScattering = Disabled 0, Low 1, Medium 2, High 3. voxelMeshQuality = Low 0, High 1. models = Performance 0, Balance 1, Quality 2. The author's log shows High and Quality, which are the top levels.
- The live settings struct is options+0x9f8 (getter `0x140e28400`).
- **Path A, per frame:** `0x140855ad0` → `0x1408572e0` → `FUN_14082f270` (defaults) → `FUN_1400f72b0` (menu → stack scene struct, at the call site `0x1408576af`).
- **Path B, scene-view config:** `0x140840f20` presets a 0x84-byte config, then `0x1400f7ac0(config, settings)` runs at `0x140841063`. `0x1400f1cb0` compares it with the stored copy and frees and reallocates scene-view memory when it differs.
- voxelMeshQuality → config+0x60/+0x64/+0x68: Low 16/64/96, High 32/128/160; the preset is 32/128/256. A value of 2 is not handled, so the preset stays (an untested option). The High immediates are at `0x1400f7f2e` / `0x1400f7f35` / `0x1400f7f3c`. VERIFIED by me from the bytes: `c7 43 60/64/68 <imm32>` = 32.0 / 128.0 / 160.0 floats; the Low branch at `0x1400f7f42` = 16 / 64 / 96.
- Deco/distant chain: scene+0x294/+0x295 → block+0x230/+0x231 (`0x14012f5f4`) → +0x689/+0x68a (`0x1400d470b`) → `ShaderModelChunkInstanceCullingShaderParameters.distanceCulling` (+0xd90). The 0xde0 block is memcpy'd to scene+0x1e20. That copy is a possible place for a live post-hook (unverified).
- distantObjects also adds sub-view indices: Medium {2,3,4,5}, High adds {6,7,8,9} (`0x1400d48c6`).
- Writing an enum value above the top level does NOT help: `FUN_1400f72b0` writes only 0..3, and detail ≥4 reaches the fatal "invalid code path" at `0x140079536`.


## Signature table for the build (scratchpad `sigs.py`, every one tested UNIQUE in build f68140a3)

| Site | Anchor | Signature (`??` = rel32 / rip disp32 wildcard) |
|---|---|---|
| scene settings fn (post-hook) | 0x1400f72b0 | `41 8b 00 f3 0f 10 0d ?? ?? ?? ?? 89 81 08 03 00 00 f3 41 0f 10 40 04 f3 0f 58 c1 f3 0f 11 81 0c 03 00 00 0f 57 c0 41 0f 2f 40 08 76 ?? f3 0f 10 05 ?? ?? ?? ?? eb ?? f3 0f 10 05 ?? ?? ?? ?? f3 41 0f 59 40 08 45 33 c9` |
| brick scatter High load | 0x140079502 | `83 f9 01 75 ?? f3 0f 10 05 ?? ?? ?? ?? 48 83 c4 28` (disp32 at +9) |
| per-instance scatter High loads | 0x14007691c | `83 f9 01 0f 85 ?? ?? ?? ?? f3 0f 10 35 ?? ?? ?? ?? f3 0f 10 3d ?? ?? ?? ?? f3 44 0f 10 05 ?? ?? ?? ?? eb ??` (distance disp32 at +30) |
| scatter capacity const | 0x140074d4c | `89 85 80 05 00 00 c7 85 84 05 00 00 00 40 01 00 41 8b 55 10` (imm at +12) |
| scatter capacity alloc | 0x1400745d8 | `49 8b 56 20 48 8d 4d 20 c7 44 24 30 00 40 01 00` (imm at +12) |
| deco High imm | 0x1400d77c3 | `83 f9 01 75 ?? c7 44 24 30 00 00 c0 3f` (imm at +9) |
| distant High imm | 0x1400d7816 | `83 f9 01 75 ?? c7 44 24 30 00 00 20 40` (imm at +9) |
| voxel High imms | 0x1400f7f1f | `8b 4f 40 85 c9 74 ?? 83 f9 01` (then `75 ??` and three `c7 43 60/64/68 imm32`; imms at +15, +22, +29) |
| async compute override | 0x1408490d2 | `88 4d 3e 44 38 b6 89 52 03 00 74 ??` |
| terrain update fn (pre-hook) | 0x1401e2e80 | `48 89 5c 24 10 4c 89 44 24 18 55 56 57 41 54 41 55 41 56 41 57 48 8d ac 24 50 e0 ff ff` (prologue only; unique in this build, but fragile across updates, so prefer pairing with a body check) |

Several signatures include the original constant, so a match also proves the expected original bytes are present.

**Extended patterns (controller, 2026-10-25, for plan Task 4).** Two rows above stop before their patch sites, so a match proved nothing about the bytes to be patched. Both were extended to cover every patch byte, and each was re-checked against `enshrouded.exe` (sha256 b28b448a…): exactly 1 match each.
- voxel High imms (0x1400f7f1f, 33 bytes; imms 32.0 / 128.0 / 160.0 at +15 / +22 / +29): `8b 4f 40 85 c9 74 ?? 83 f9 01 75 ?? c7 43 60 00 00 00 42 c7 43 64 00 00 00 43 c7 43 68 00 00 20 43`
- async compute override (0x1408490d2, 19 bytes; the 16-byte patch at +3 ends at +19): `88 4d 3e 44 38 b6 89 52 03 00 74 ?? 0f b6 86 88 52 03 00`. Bytes +3..+19 on disk equal Task 8's `ORIGINAL`.


## Scatter capacity audit: first pass (controller, 2026-10-25 22:2x; pre-research for plan Task 13)

Probe: scratchpad `capaudit.py`. It scanned 13 functions: the seeds FUN_140074b50 / FUN_1400744e0 / FUN_1400767a0 / 0x14007521d, plus their direct callees under 0x4000 bytes.
Hits, all in `FUN_1400744e0` (the allocation function):
- `0x1400745e0` `mov [rsp+0x30], 0x14000` and `0x1400745ff` `mov [rsp+0x30], 0x14000`: the capacity argument for TWO scatter sets.
- `0x1400745eb` / `0x14007460a` `mov [rsp+0x28], 0xf000`: a second size argument (61,440 = 0.75 × 0x14000), role unknown.
- `0x14007468b` `mov r8d, 0xa0000` (= 0x14000 × 8) and `0x1400746a8` / `0x1400746c4` `mov r8d, 0x280000` (= 0x14000 × 32): buffer BYTE sizes derived from the capacity.

**COVERAGE GAP:** the known shader constant `0x140074d52` (`scatteredObjectCapacity = 0x14000`) was NOT reported, although its function was a seed. The scan works per `.pdata` entry, and chunked functions are missed, so this audit is NOT exhaustive.
**Second pass, chunk-aware (`capaudit2.py`, 2026-10-25 22:35):** it groups chained `.pdata` chunks (UNW_FLAG_CHAININFO) with their primary entry. FUN_140074b50 = 3 chunks, FUN_1400744e0 = 1, FUN_1400767a0 = 7; 26 functions scanned (the seeds and their direct callees). It now FINDS the known positive `0x140074d52`, which proves the fix. Full list in that scope: 8 sites:
- `0x1400745e0`, `0x1400745ff`: capacity args 0x14000 (two scatter sets);
- `0x1400745eb`, `0x14007460a`: args 0xf000, role UNKNOWN;
- `0x14007468b`: 0xa0000 (= cap × 8);
- `0x1400746a8`, `0x1400746c4`: 0x280000 (= cap × 32);
- `0x140074d52`: shader constant.
Scope limit: GPU shader code is not in the exe and was not audited.
**Ruling for Task 13:** `cap_ok` stays false until the role of 0xf000 is known AND the shaders are confirmed to read the capacity only from the constant. Otherwise the grass knob tops out at 90 m.
**Implication for Task 13:** doubling capacity needs at least 7 sites changed together (2× 0x14000 args, 0xa0000, 2× 0x280000, the shader constant, and the 0xf000 args to be understood). Unless a chunk-aware audit maps every use, `cap_ok` must be false: the grass knob stays at 90 m.


## Reflex menu option location (controller, 2026-10-25; pre-research for plan Task 16)

The machine graphics settings struct (reflection descriptors at `0x14177a370`.., VERIFIED by descriptor dump) has these fields:
windowMode +0x0, windowPosition +0x4, windowSize +0xc, forceWindowClientSizeResolution +0x14, forceFullscreenResolution +0x1c, renderScale +0x24, automaticRenderScale +0x28, vsync +0x29 (bool), hdrSettings +0x2a, **nvidiaReflex +0x2c** (`NvidiaReflexModeSettings`), nvidiaReflexDefaultOff +0x30, **render +0x34** (the render-settings struct), renderPreferences +0x88, useDefaultRenderSettings +0x94.
- The live render settings are at options+0x9f8 (getter `0x140e28400`, menu agent), so the machine struct is at options+0x9c4 and nvidiaReflex is at options+0x9f0.
- `FUN_1400f72b0(scene, renderSettings, …)` receives the render-settings pointer as its 2nd argument, so **nvidiaReflex = `*(u32*)(renderSettings − 0x8)`** and vsync = `*(u8*)(renderSettings − 0xb)`. The Task 12 post-hook can publish both. HYPOTHESIS until it is read at run time; `NvidiaReflexModeSettings` members (VERIFIED, member table 0x141779b80, stride 0x28): **Off = 0, On = 1, Boost = 2**. The author's log shows "On" (= 1). There is also an `nvidiaReflexDefaultOff` field at +0x30.

## Dead ends

- Ranking functions that read the settings offsets 0x2c–0x40 (`scan_fields.py`): 776 matches. Not selective.
- `0x1400f8140`: it switches on a 2-value enum and sets buffer sizes and thresholds. It is NOT `models`, which has 3 values (menu agent). It writes the same scene-view config as `0x1400f7ac0`; perhaps a console Quality/Performance mode (HYPOTHESIS). It is not the distance function.

## Decisions (the author, 2026-10-25, via questionnaire)

- Structure: approved ("use what is best and most efficient").
- Default target: **75 fps**.
- Build for THIS install (build f68140a3) now; test on the Steam build when the author has it.
- **Bundle** Ultimate ASI Loader v10.0.0 with its MIT licence. This is a choice the author made. The text for the project rules was given to the author to add.
- Working name only for now; rename before release.
- Public GitHub repo; project folder `C:\Modding\Enshrouded`.
- Disclosure: joint work per project rule (human-designed, AI-written, human read and corrected), in the author's own words. The author reviews all code before upload.
- ASI Loader facts (agent report): MIT; `InitializeASI()` export; loads from root/scripts/plugins/update; triggers on the exe's first kernel32 import call (before WinMain), so confirm with `DebugLog=1`. The loader DLL is signed by an untrusted cert `CN=FusionFix`, so expect AV/SmartScreen noise. Issue #215: "Disable fullscreen optimizations" makes Windows skip the local dinput8.dll. No compat-flag entry for enshrouded.exe on this machine (checked HKCU/HKLM Layers).
- Present hook (agent notes): the game resolves vkQueuePresentKHR via vkGetDeviceProcAddr and stores it at deviceObj+0x5a0 (`0x140fe3c84`).
- Rust stack (agent notes): rustup reports 1.99.1 latest stable (local 1.98.0); minhook 0.9.0 (freezes threads) is preferred over retour (no stable release since 2023); windows-sys 0.61.2; ash 0.38.0.

## Governor decision (the author, 2026-10-25): "More aggressive"

Defaults, all in the .ini: target 75 fps. Signal = p95 of present-to-present over a rolling 2 s window; samples >100 ms are ignored.
RAISE one knob one step when p95 < 90 % of target for 1 s; settle 1 s after any change.
LOWER (undo the last step) when p95 > 97 % of target for 0.3 s. A step that overshoots is blocked for 20 s.
Floor = the menu setting (step 0); never below it. A menu change re-reads the baseline and resets the added steps.

## Present-hook design (agent report, VERIFIED from bytes)

- The game does `LoadLibraryW(L"vulkan-1.dll")` at `0x140fcc2d6`, then `GetProcAddress("vkGetInstanceProcAddr")` through IAT slot `0x1413b2908` (`0x140fcc32f`).
- vkGetDeviceProcAddr comes via GIPA at `0x140fe4931` and is stored at devobj+0x78. vkQueuePresentKHR comes via GDPA (`0x140fe3c67`) and is stored at devobj+0x5a0. There is ONE present call site, at `0x140fd529a` (signature `48 8B 80 A0 05 00 00 FF D0`, unique, RVA 0xfd5293).
- Hooking the vulkan-1 exported vkQueuePresentKHR is BYPASSED: the game uses device-level pointers.
- CHOSEN: wrap the GetProcAddress IAT slot; return wrap_gipa for "vkGetInstanceProcAddr"; wrap_gipa returns wrap_gdpa for "vkGetDeviceProcAddr"; wrap_gdpa returns wrap_present for "vkQueuePresentKHR". No game-specific addresses. Fallback: detour the call-site signature.
- The game does NOT use VK_NV_low_latency2. Its "nvidiaReflex" option appears inert on Vulkan (HYPOTHESIS). No Streamline.
- `--enable-dxgi` would switch to a DXGI-interop swapchain; out of scope.

## Mod direction (the author, 2026-10-25)

Public Nexus mod. A Rust `.asi` plugin for Ultimate ASI Loader (latest is v10.0.0, 2026-10-21). An fps-target governor that only ADDS above the menu settings, never below them. Brainstorming: Section 1 structure has been proposed and is waiting for the author's answer.

## Research results, 2026-10-25 (workflow wf_abc7d5af-ccd, 2 researchers + 2 refuters; both PARTLY, conclusions survive)
Full write-ups: `research-2026-10-25-reflex.md` and `research-2026-10-25-scatter-capacity.md` in this folder.
- **Reflex (T1 + T3):**
  - No low-latency or Reflex API name appears in the exe or its five top-level DLLs.
  - The game log shows that the driver offers `VK_NV_low_latency2` and that the game never enables it. The game enables `VK_KHR_present_id2` and `VK_KHR_present_wait2`, but only on non-AMD GPUs and NVIDIA drivers >= 0x9a438000.
  - The game chains its own `VkPresentId2KHR` on its single present call (0x140fd529a). With vsync on, its limiter at 0x140fb0d35 keeps at most one present queued (`vkWaitForPresent2KHR`, 500 ms timeout).
  - The settings hold TWO fields of the same Reflex enum: `nvidiaReflex` (+0x9f0, default On) and `nvidiaReflexDefaultOff` (+0x9f4, default Off). No `.text` code reads either field.
- **Scatter capacity, CORRECTION to the audit above:**
  - The 0xf000 values are GPU-counter WARN levels (75 %), and the 0x14000 at 0x1400745e0/0x1400745ff are counter CAP metadata. Neither is a buffer size.
  - The buffers are fixed per scene view: `ScatteredObjectData[0x14000]` (0xa0000 B) and two `DetailScatteringInstanceData[0x14000]` (0x280000 B each).
  - Both `FUN_140074b50` calls share counter A, so there is ONE 0x14000 budget, not two. Counter B is the per-model instance-offset counter.
  - The exe has no CPU-side clamp. The cbuffer `scatteredObjectCapacity` (0x140074d52) is the only scattered-object bound. The brick gate has its own `maxCullElements` (`BrickCullingShaderParameters` +0xec8).
  - Patching the "capacity alloc" signature would CAUSE the overflow, so that patch was removed from plan Task 13.
  - Measurement path: the game reads the counters back on the CPU (last value at entry+0x14, peak at +0x1c, warn at +0x20, cap at +0x24).

## Decisions (the author, 2026-10-25, second questionnaire)
- The Reflex row is NOT shown in the in-game menu on Vulkan.
- Reflex default: "On for NVIDIA". The `.ini` value `game` becomes `auto` (on for NVIDIA GPUs whose driver lists `VK_NV_low_latency2`).
- The grass measurement tool goes into v1: a debug-only read of the game's scatter GPU-counter peak in dense grass, to compute a safe maximum distance.

## The Steam exe (build f68140a3, Steam buildid 26082781), 2026-10-26

The author moved from the old install (`C:\Games\Enshrouded`, being deleted) to Steam (`C:\Program Files (x86)\Steam\steamapps\common\Enshrouded`). The old exe is kept for reference at `reference\old-build\enshrouded.exe` (not in git).

| | sha256 | bytes | mod label |
|---|---|---|---|
| old install | `b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a` | 35338752 | `f68140a3` |
| Steam | `d64966e23b1a458ee6f147bcbceac0b577fa7e39a212f371287d8cc119817882` | 35338312 | `f68140a3-steam` |

**VERIFIED** (controller's PE compare, then the ignored test `build_id::tests::steam_exe_matches_the_old_build_except_the_stub`, which reads both files):
- `.text`, `.rdata`, `.data`, `.pdata`, `.rsrc` and `.reloc` have the same virtual address, the same raw size and byte-identical raw data. So every RVA, signature, patch site, the texture table (`0x1413b86d0`) and the IAT slot (`0x1413b2908`) are unchanged. The three ignored real-exe tests (signatures, texture table, async-compute site) pass on the Steam exe, with every signature at its anchor.
- Same `TimeDateStamp` (1792507660), image base, `SectionAlignment` (`0x1000`) and `SizeOfImage` (`0x2a1e000`). Neither file has a certificate.
- The entry point moved. Steam: `0x029e4310`, inside `.bind` (`0x029e4000`, size `0x39248`), which is Steam's DRM stub. Old: `0x0132dbb8`, inside `.text`, the game's own entry.
- `.bind` is the only section with different bytes. The old exe has a `.bind` section too (size `0x39400` raw), with other content, but its entry point is in `.text`.
- The section headers state the virtual sizes differently: the Steam headers give them exactly (`.text` `0x13b0730`), the old ones round them up to the page (`.text` `0x13b1000`). After rounding up to `SectionAlignment` they are equal for every section, so the loader maps the same pages.

**HYPOTHESIS** (inferred, not checked): the old exe is a copy with the stub bypassed (entry point put back into `.text`, a changed `.bind` left in the file). And `.text` on disk is plain in the Steam exe (it equals the old `.text`, which ran), so the stub does not encrypt the code.

**Mod**: `build_id::KNOWN` has both hashes. Both are known builds: code patches are on for either. The label only tells the log which file runs. The ignored real-exe tests read `ENSHROUDED_EXE` if it is set, else the Steam path above. A path that is set must be a readable file, or the test panics: it never skips. Only a missing file at the default Steam path skips (a PC without the game). The Steam-against-old test takes its Steam exe from `ENSHROUDED_STEAM_EXE`, else the Steam path, and needs `reference\old-build\enshrouded.exe` (a missing copy fails it). A skip prints a line, but the harness still reports `ok`, and the line shows only with `-- --ignored --nocapture`. So run them as `cargo test --lib -- --ignored --nocapture` and look for the real lines (`SKIPPED` means nothing was read).

**Task 17 risk (Steam only)**: SteamStub runs before the game's entry. Check in game:
1. Which caller fired the loader's trigger, and whether the stub had already handed over to `.text` by then. The loader fires on the exe's first kernel32 import call. `docs/design.md` (test-environment note) says the loader "ignores calls from that section [`.bind`] as triggers": from loader source v10.0.0, **unconfirmed in game**. If that holds, the trigger comes from the first call out of `.text`, after the stub. If it does not, `InitializeASI` may run while the stub is still working. Read what fired the trigger in `dinput8.log` (`DebugLog=1`), and compare it with the `.bind` range (`0x029e4000`..`0x02a1d248`). If the log names no caller, compare its time stamps with the mod log's first line. The `GetProcAddress` slot swap (`vk::install`) and the signature scans need the final image, so they must not run before the stub has handed over.
2. Whether the stub checks `.text` in memory. The async-compute patch (16 bytes at `0x1408490d5`) and the minhook detours are in `.text`. If the game exits or restarts through Steam after the code patches, find which feature does it (`AsyncCompute=off`, then `Enable<Knob>=0`); the texture budget is a write to `.rdata`, not `.text`.
