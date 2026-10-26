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

The author moved from a standalone install to the Steam version (default Steam library). The old exe is kept for reference at `reference\old-build\enshrouded.exe` (not in git).

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

## HDR paper white (controller, 2026-10-26; plan route B1, worksheet W13)

- **The problem.** In HDR mode the composite pass (`cs_main_composite_frame`) multiplies the whole frame, scene and UI, by a paper-white value before the PQ encode. The exe hard-codes that value as 250.0. The tonemapper's output is in 80-nit units. So the HDR picture is about 3.1 times too bright, and highlights clip at the panel's peak. The scale mismatch is read from the shader; its visible effect is a strong hypothesis until the in-game A/B test (plan Task 17).
- **The site (VERIFIED, bytes, Steam exe sha256 `d64966e2...`, 2026-10-26).** The instruction `c7 45 88 00 00 7a 43` (`mov dword [rbp-0x78], 0x437a0000`, which is 250.0f) occurs exactly once in the file. It is in `.text` at file offset `0x8f7cbc`, VA `0x1408f88bc`. The 4-byte immediate is at VA `0x1408f88bf`.
- **Context bytes:** `48 89 45 80 45 33 e4 8b c3 | c7 45 88 00 00 7a 43 | 49 8b 16 c7 45 8c 9a 99 19 3f`. The block starts at `[rbp-0x80]`. Paper white is at `+8`. The next store, `+12`, is 0.6f, the luminance clamp factor. This matches the shader's uniform layout (paper white at +8, clamp at +12).
- **Patch rule.** Replace the immediate with the f32 bits of the setting, little-endian. Expected old bytes: `00 00 7a 43`. The function runs every frame, so the new value applies from the first HDR frame. From the shader analysis, the SDR path does not use this value (a hypothesis until the in-game A/B test).

## Point-light and sun shadow quality levels (2026-10-26; plan Task 14b, lighting bug L3)

Evidence: REA/Ghidra and local capstone scans on the old-build exe (sha256 `b28b448a...`). A second, independent byte check re-derived each item. The old exe's `.text` and `.rdata` are byte-identical to the Steam exe's (see "The Steam exe"), so every address below holds in both. Items are VERIFIED (bytes) unless marked HYPOTHESIS.

- **Caveat (read first).** It is unproven that these settings cause the player reports (shadows resolve only near the player). On the CPU, no setting sets a point-light shadow distance. The GPU picks which lights get shadow tiles and rays. An in-game A/B test must decide it: High against Ultra at the Polaris Falls bridge at night, then Ultra with a raised ray budget and static distance. The shader limits are unknown: nobody has checked whether the shaders assume `maxLightsShootingRays` <= 64 or a tile <= 192 px.
- **Menu → scene-view config (VERIFIED, REA ev_8ccd39c8).** `0x1400f7ac0` (Path B, called at `0x140841063`) copies each menu level into the config as the same number.
  - `pointLightShadows` (settings+0x48) has levels 0..4 and goes to config+0x5c = 0..4. The chain starts at `0x1400f7edd`. The stores `MOV [RBX+0x5c],ESI/1/2/3/4` for levels 0, 1, 2, 3 and 4 are at `0x1400f7f1c`, `0x1400f7f13`, `0x1400f7f0a`, `0x1400f7f01` and `0x1400f7ef8`.
  - `shadows` (settings+0x18) has only levels 0..3. There is no level 4. It goes to config+0x4c = 0..3. The chain starts at `0x1400f7ea9`. The stores `MOV [RBX+0x4c],ESI/1/2/3` for levels 0, 1, 2 and 3 are at `0x1400f7eda`, `0x1400f7ed1`, `0x1400f7ec8` and `0x1400f7ebf`.
  - Any other value takes a jump (`0x1400f7f1f` for point lights, `0x1400f7edd` for shadows) and writes nothing. The field keeps its preset value.
  - Menu types: field record `0x141b41e90` is `pointLightShadows` (offset 0x48, type `0x141b3d6b0`, `RenderPointLightShadowsSetting`, 5 members). Field record `0x141b41c50` is `shadows` (offset 0x18, type `0x141b35c30`, `RenderShadowQualitySetting`, 4 members Low, Medium, High, Ultra = 0..3, members at `0x141b35980`).
- **Preset and fallback (VERIFIED, ev_cd0e7359).** The preset in `0x140840f20` runs before `0x1400f7ac0`. It sets config+0x4c = 1 at `0x140840fd0` (`c7 46 4c 01 00 00 00`) and config+0x5c = 1 at `0x140840fec` (`c7 46 5c 01 00 00 00`). `CALL 0x1400f7ac0` comes later, at `0x140841063`, and only when the settings pointer (RBP) is non-null. `0x140840f20` is the only resolved caller of `0x1400f7ac0` (ev_be4ebc63). So an out-of-range menu value, for example `pointLightShadows` = 5 in the settings file, leaves level 1 in place. It never indexes past a table. A second function, `FUN_1400f8140`, also writes both fields as 1: `c7 41 5c 01 00 00 00` at `0x1400f822e` and `c7 41 4c 01 00 00 00` at `0x1400f8235` (ev_bcb44d48). Whether it runs on PC is not known.
- **Enum names (VERIFIED, bytes; ev_3539f52e, ev_1561eaef, ev_9997a559, ev_dbe9edf0).**
  - Menu `RenderPointLightShadowsSetting` (type name `keen::pc::RenderPointLightShadowsSetting`, string at `0x141d1a5a0`; descriptor `0x141b3d6b0`/`0x141b3d6c0`, count 5; members at `0x141b3d5e0`): Lowest = 0, Low = 1, Medium = 2, High = 3, Ultra = 4. So "Ultra" in the menu is level 4.
  - Engine `keen::ManyLightsShadowsQuality` (descriptor `0x141bda600`, count 5; members at `0x141bda3c0` and `0x141bda438`): UltraPerformance = 0, Performance = 1, Balance = 2, High = 3, Ultra = 4.
  - Engine `keen::RasterShadowQuality` (descriptor `0x141bd7340`, count 4; members at `0x141bd6f20`): Low = 0, Medium = 1, High = 2, Ultra = 3.
  - HYPOTHESIS: config+0x5c holds a `ManyLightsShadowsQuality` and config+0x4c holds a `RasterShadowQuality`. The link rests on the matching member counts and on the consumers below.
- **Consumer chain (VERIFIED).**
  - `FUN_1400f1cb0` (`0x1400f1cb0`, ev_fbd3edd1) keeps a copy of the config at wrapper+0x10. So config+0x4c is at wrapper+0x5c and config+0x5c is at wrapper+0x6c. It compares every field. Any difference takes the free-and-reallocate path. The early return needs bit 0 of param_3.
  - Before `CALL 0x140103d40` (at `0x1400f27e0`), it repacks the config (`0x1400f2651`..`0x1400f2731`). config+0x4c goes to repacked+0x44 (carried at `0x1400f26ac`). config+0x5c goes to repacked+0x54 (carried at `0x1400f26c5`).
  - `FUN_140103d40` (ev_0df1914f) is the only consumer of the two fields on the reallocation path. At `0x140104248` (`MOV EAX,[R15+0x44]`) it reads the sun-shadow quality and calls `FUN_1401637c0` (call at `0x140104263`), which builds the cascade view at sceneView+0x990. At `0x1401043ce` (`MOV EAX,[R15+0x54]`) it reads the point-light quality and calls `FUN_1400c8780` (call at `0x1401043d6`), which builds the ManyLightsShadows view at sceneView+0xf28. The repacked struct is also copied to renderSceneView+0x14.., so the two values sit at RSV+0x58 and RSV+0x68.
  - `FUN_140103ba0` (features memory, ev_6b550b8d) reads neither field. It reads only param_3+0x58. `FUN_1400f4140` (flag remap, ev_950b9483) and `FUN_14013bfc0` (reads only +0x38, ev_db84990f) do not read them either.
  - The ManyLightsShadows view is created for feature 0x12 by `FUN_1400c8a00(renderer+0x16558, &2000)`: `mov [rbp+0x18],0x7d0` at `0x1401081c4`, the call at `0x1401081d2`, `mov [rdi+0xf28],rax` at `0x1401081d7` (ev_4209c1d7). The failure string at `0x1413b9270` reads "[render] could not create many lights shadows view ". Its light capacity at view+0x34 is `0x7d0` = 2000 for every quality level.
- **Point-light quality table lookup (VERIFIED, ev_22650670, ev_3f4dd3c4).** `FUN_1400c8780` (code `0x1400c879c`..`0x1400c87f5`) uses the quality value as a table index with no bounds check: `MOV EAX,[RDX]; IMUL R8,RAX,0x2c; LEA RAX,[0x1413b78f0]`.
  - It copies the 0x2c-byte entry to view+0x08..+0x33: 16 bytes to `[RCX+8]`, 16 to `[RCX+0x18]`, 8 to `[RCX+0x28]` and 4 to `[RCX+0x30]`.
  - It creates the atlas texture with width = `[view+0x10]` x `[view+8]` (tile x atlasSizeX), height = tile x `[view+0xc]` (atlasSizeY), 1 layer, 1 mip, and format byte `0x61` (`MOV word [RBP-0x1c],0x6101`). The create call, `CALL 0x1411ce210`, is at `0x1400c8854`.
  - It creates two hash buffers of atlasSizeX x atlasSizeY x 16 x 8 bytes each (`FUN_1411cdc10`, for view+0x48 and view+0x58). It also creates two small fixed buffers (0x30 and 0x18 bytes) at view+0x68 and view+0x78.
- **Table `ManyLightsShadowsQualitySettings` (VERIFIED, ev_893b7119; field layout ev_61694141).** The table is in `.rdata` at `0x1413b78f0`. The stride is `0x2c`. It has 5 entries (0xdc bytes), indexed by the `ManyLightsShadowsQuality` value. The reflection descriptor `0x14198cea0` is `keen::ManyLightsShadowsQualitySettings`, size 0x2c, 11 fields. The field array is at `0x14198b1c0`. Fields: `atlasSizeX` +0, `atlasSizeY` +4, `shadowAtlasTextureSize` +8 (the tile size in px), `shadowMapBias` +0xc, `shadowMapNormalBias` +0x10, `gaussianScale` +0x14, `exponentialShadowConstantX` +0x18, `exponentialShadowConstantY` +0x1c, `maxLightsShootingRays` +0x20, `stopLightMovementAtDistance` +0x24, `updateStaticLightsContinuously` +0x28. Entry n is at `0x1413b78f0` + n x 0x2c. The Ultra entry is at `0x1413b79a0` and the High entry at `0x1413b7974`.

  | Level | Name | Tiles X x Y | Tile px | shadowMapBias / shadowMapNormalBias / gaussianScale | maxLightsShootingRays | stopLightMovementAtDistance | updateStaticLightsContinuously |
  |---|---|---|---|---|---|---|---|
  | 0 | UltraPerformance | 16 x 8 | 96 | 0.3 / 0.0085 / 1.65 | 16 | 20 | 0 |
  | 1 | Performance | 24 x 16 | 128 | 0.25 / 0.00055 / 2.0 | 24 | 30 | 0 |
  | 2 | Balance | 32 x 16 | 160 | 0.25 / 0.00055 / 2.0 | 32 | 40 | 1 |
  | 3 | High | 32 x 32 | 160 | 0.25 / 0.00055 / 2.0 | 48 | 50 | 1 |
  | 4 | Ultra | 32 x 32 | 192 | 0.1 / 0.00035 / 2.0 | 64 | 100 | 1 |

  - The Ultra entry bytes are `20000000 20000000 c0000000 cdcccc3d 3480b739 00000040 8fc2f53d ec51383d 40000000 0000c842 01000000`.
  - Ultra against High: tile 192 against 160, rays 64 against 48, static distance 100 against 50, smaller biases. Both have 1024 tiles.
  - Ultra entry fields (derived by arithmetic from the layout and the Ultra entry bytes, not read as separate facts): `shadowAtlasTextureSize` at `0x1413b79a8` (old bytes `c0 00 00 00` = 192); `maxLightsShootingRays` at `0x1413b79c0` (old bytes `40 00 00 00` = 64); `stopLightMovementAtDistance` at `0x1413b79c4` (old bytes `00 00 c8 42` = 100.0).
- **A level above Ultra is not safe (bytes VERIFIED; the "not safe" conclusion is an inference, HYPOTHESIS, but a sound one).** A level 5 in config+0x5c indexes past the table, at `0x1413b79cc` (the table start + 0xdc). The bytes there are `00000000 1e000000 00000000 00000000 78873de9 11d1716d 965c53fd 20090100 00000000 00000000 00000000`. They read as atlasSizeX = 0, atlasSizeY = 30, tile = 0, maxLightsShootingRays = 0, stopLightMovementAtDistance = 0.0, updateStaticLightsContinuously = 0, and garbage bias floats. By the lookup above, the atlas texture is 0 x 0 and both hash buffers are 0 bytes (0 x 30 x 128). By the uniform code below, `distanceMovementScale` = 1.0 / 0.0. Sun shadows fail the same way: a `shadows` level of 4 or more matches no case in `FUN_140164400` (below), so it leaves 0 cascades and a 0 resolution.
- **Per-frame uniform use (VERIFIED, ev_da0240de).** `FUN_1400c91a0` builds `ManyLightsShadowsUniformBuffer` (base `RBP-0x50`, size 0x360) from the view. Its only caller is the ManyLightsShadows frame function `FUN_1400ca3c0` (ev_f9c555d3). The only caller of that function is `FUN_140129fc0`. That it runs every frame rests on this call chain (HYPOTHESIS-strength). The view values come from the table only at allocation time. This function reads them each time it runs.
  - `maxLightsShootingRays` (UB+0xac) = view+0x28 (`0x1400c92f8`).
  - `updateStaticLights` (UB+0x2c4) = view+0x30, a byte (`0x1400c92fe`).
  - `distanceMovementScale` (UB+0x2c0) = 1.0 / view+0x2c (`DIVSS` at `0x1400c9308`; the 1.0 is `0x3f800000` at `0x1413b3c90`, ev_7ff352b4). So `stopLightMovementAtDistance` = 0 divides by zero.
  - `maxLightCount` (UB+0x94) = view+0x34. `gaussianBlurScale` (UB+0x98) = view+0x1c.
  - The atlas block at UB+0x310 is `keen::ManyLightsShadowsAtlasParameters` (descriptor `0x1419947d0`, fields `0x1419927d0`; code `0x1400c941a`..`0x1400c9467`). atlasSize = (X, Y). atlasMaxEntries = X * Y. atlasHashMapSize = (X * Y) << 4. atlasElementSize = tile. atlasElementTexelCount = tile - 2. atlasElementBorder = 1. atlasShadowMapBias = view+0x14. atlasNormalBias = view+0x18. atlasExponentialShadowConstant = view+0x20 and view+0x24. The Chebyshev constants are fixed at 0.01 and 0.8.
  - The uniform field names are in the descriptor at `0x141995240` (ev_ac1fbbaf).
- **Texture formats (VERIFIED, ev_fd53b225, ev_3b2d660b).** PixelFormat `0x61` is `R16G16B16A16_sfloat`: 8 bytes per texel, used by the point-light atlas (entry `0x141740218`, name length 0x13). PixelFormat `0x7e` is `D32_sfloat`: 4 bytes per texel, used by the sun cascades (entry `0x1417406a0`, name length 0xa). The bytes per texel come from the format names, not from the exe.
- **VRAM per level (arithmetic on the layouts above, checked twice).**
  - Point-light atlas, RGBA16F: UltraPerformance 1536 x 768 = 9 MiB. Performance 3072 x 2048 = 48 MiB. Balance 5120 x 2560 = 100 MiB. High 5120 x 5120 = 200 MiB. Ultra 6144 x 6144 = 288 MiB.
  - A tile of 256 at 32 x 32 tiles would give 8192 x 8192 = 512 MiB. That is 224 MiB more than Ultra.
  - The two hash buffers are 128 KiB each at 1024 tiles (32 x 32 x 16 x 8 bytes).
  - The light buffers are sized from the capacity 2000 (view+0x34) or are small constants. They do not depend on the level (`FUN_1400c7dc0`, ev_80a98ed2).
  - Sun cascades, D32: Low 16 MiB. Medium and High 45 MiB. Ultra 96 MiB. A resolution of 4096 at Ultra would be 384 MiB, which is 288 MiB more. These figures leave out two small textures of resolution / 16 that exist when `param_2+4` of `FUN_1401637c0` is set.
- **Sun cascade layout per level (VERIFIED, ev_3703d77f).** `FUN_1401637c0` calls `FUN_140164400(view, RasterShadowQuality)` (`0x140164400`). At entry it sets `[view+0x5c]` (cascade count) = 0 and `[view+0x78]` (resolution) = 0. Then it sets the layout per level:

  | Level | Cascades | Resolution | Cascade end distances (m) | Cadence words | view+0xd0 |
  |---|---|---|---|---|---|
  | Low (0) | 4 | 1024 | 20 / 100 / 400 / 4000 | 1, 2, 0x104, 0x304 | 0.25 |
  | Medium (1) and High (2), identical | 5 | 1536 | 12 / 40 / 120 / 400 / 4000 | 1, 1, 2, 0x104, 0x304 | 1.0 |
  | Ultra (3) | 6 | 2048 | 4 / 12 / 50 / 150 / 400 / 4000 | 1, 1, 1, 2, 0x104, 0x304 | 3.0 |

  - The Ultra block is at `0x140164451`..`0x140164493`. The resolution `imm32` is at `0x14016445b`. The bytes from `0x140164451` (ev_bec2da7b) are `c7415c06000000 c7417800080000 c7412c04000000 c741300c000000 c7413432000000 c7413896000000 c7413c90010000 c74140a00f0000 c781d000000000004040`.
  - A level of 4 or more matches no case. The count and the resolution stay 0, and the distances and the cadence stay unchanged. This is not safe.
  - The cascade texture has width = height = view+0x78, layers = view+0x5c, and format `0x7e` (D32_sfloat) (ev_b67419d78d1b55bd).
- **Cascade distances and cadence (VERIFIED, ev_c9fa0808, ev_47aad6fe).** The cascade end distances are unsigned integers in meters. `FUN_140161f20` converts them to float and needs them strictly increasing. `FUN_1401675e0` caps the cascade count at 8 (`min(view+0x5c, 8)`). It reads the distances at view+0x2c and the cadence words at view+0x4c. Each per-cascade 16-bit cadence word has a low byte (the update interval in frames) and a high byte (the phase). A cascade re-renders when `frameId % interval == phase` and the interval is at most the frames since its last render. It also re-renders when the camera or the sun has moved past a threshold (label `LAB_140162c2c`). On Ultra, the cascade that ends at 150 m has cadence 2, so it updates every 2nd frame. The two cascades that end at 400 m and 4000 m use `0x104` and `0x304`, so they update every 4th frame (phase 1 and phase 3). A low byte of 0 passes the interval check and then does a modulo by zero.
- **The game rewrites the sun layout every frame (VERIFIED, ev_ede5a1d2).** `FUN_1401675e0` calls `FUN_140164400(view, [view+0x74])` again on each call, while the sun direction is non-zero. The skip test is `|x|, |y|, |z| <= 1e-5` on `[rdx+0x14c..0x154]`. `MOV EDX,[RCX+0x74]` is at `0x140167647` and `CALL 0x140164400` is at `0x140167662`. That rewrites the cascade count, the resolution field, the distances and the cadence from the code immediates. So a live write to view+0x2c..+0x56 is lost on the next frame. A patch to the code immediates takes effect on the next frame for the distances and the cadence. The resolution (view+0x78) is read when the texture is allocated, in `FUN_1401637c0`. A resolution patch made after the allocation leaves view+0x78 out of step with the texture. "Every frame" rests on the frame-id cadence and on the only caller being `FUN_140129fc0` (HYPOTHESIS-strength).
- **Signatures (VERIFIED, ev_32e62231, ev_bec2da7b).** Each is unique in `.text`, by a byte scan of the old-build exe. REA confirmed the bytes.
  - `MLS_QUALITY_TABLE_LEA` = `8B 02 48 8B F1 4C 6B C0 2C 48 8D 05 ?? ?? ?? ?? C7 45 E0 01 00 01 01`. One hit, at `0x1400c879c`. The bytes there are `8b02488bf14c6bc02c488d0544f12e01c745e001000101`. The disp32 at match+0xc is `0x012ef144`. The table address is (match+0x10) + disp32 = `0x1400c87ac` + `0x12ef144` = `0x1413b78f0`. The Ultra entry is the table + 4 x 0x2c = `0x1413b79a0` (table + 0xb0).
  - `SUN_SHADOW_ULTRA` = `C7 41 5C 06 00 00 00 C7 41 78 00 08 00 00 C7 41 2C 04 00 00 00 C7 41 30 0C 00 00 00`. One hit, at `0x140164451`. The patch offset is +10 (`0x14016445b`), the `imm32` of `mov [rcx+0x78]`. The old bytes are `00 08 00 00` (2048).
  - The 0xdc-byte table itself occurs once in `.rdata` (at `0x1413b78f0`) and in no other section.
- **Not verified (HYPOTHESIS or open).**
  - What the fields mean. `maxLightsShootingRays` reads as the number of shadowed lights updated per frame. `stopLightMovementAtDistance` reads as the distance beyond which lights count as static. Only the arithmetic is verified: the uniform holds `distanceMovementScale` = 1.0 / `stopLightMovementAtDistance`. The GPU picks which lights get atlas tiles and rays (the IndirectCounter and PickedIndirectArgument buffers). `0x1400ca3c0` only dispatches compute passes and binds buffers (ev_d55c351d). No CPU light-distance cull was found.
  - No GPU buffer is sized from `maxLightsShootingRays`. The buffers use the light capacity of 2000.
  - Candidate knobs that belong to other systems (decoded with a local PE parser of the reflection descriptors, not with REA): `maxPointLightCount` (`VolumetricFog3ViewVolumePopulateParameters` +0x790, `ClusterCullingShaderData` +0xa8), `pointLightBufferCapacity` (`VfxUpdateBuffer` +0x25c), and `pointLightInfluenceScale` and `enablePointLightShadows` (GI probe parameter structs). `LightCacheLightCulling` has no struct owner and is probably a pass name. The game also logs "[render] Disabling ManyLightsShadows, requires the WorldSdf feature!".
  - The sun-shadow view field `+0xd0` (0.25, 1.0 or 3.0 by level) goes into the per-frame shadow constants (code `0x140164be5` and `0x140164c17`). It may be filter softness.
  - Not checked: other readers of the stored quality copies (wrapper+0x5c, wrapper+0x6c, RSV+0x58, RSV+0x68); whether `FUN_1400f8140` runs on PC; whether the ASI start-up runs before the first scene-view allocation (check the log order); whether the pop-in is light culling or the GI cache radius, not shadows.

## HDR tonemapper peak (2026-10-26; plan route B2)

Route B2 sets the HDR tonemapper's peak field, so the roll-off ends at the panel's peak. Evidence: REA/Ghidra on the old-build exe (sha256 `b28b448a...`; the Steam exe has the same code). A second byte check re-derived each item. Items are VERIFIED (bytes) unless marked otherwise. Several "only writer" and "only reader" claims were checked only along the traced call path. The text names each such limit.

- **The field `scene+0x41e` (VERIFIED).** It is a u16 in nits. The scene struct is a stack local of `0x1408572e0` at `RBP-0x10`. It is rebuilt on every call (`LEA RCX,[RBP-0x10]` at `0x1408573b7` for the defaults function and at `0x1408576ab` for `FUN_1400f72b0`; ev_62eac43d). Two game writes come before the hooked call:
  - The defaults function `FUN_14082f270` (only caller `0x1408572e0`; ev_ee0d6cd4, ev_66a201c7) sets dword scene+0x41c = `0x006e0101` (`MOV dword [RBX+0x41c],0x6e0101` at `0x14082f545`). So the u16 at +0x41e is `0x6e` = 110. It also sets byte scene+0x408 = 0 (`MOV byte [RBX+0x408],DIL` at `0x14082f516`).
  - `0x1408572e0` then copies settings+0x240: `MOVZX EAX, word [RDI+0x240]` at `0x140857530` and `MOV word [RBP+0x40e],AX` at `0x140857537` (RBP+0x40e = scene+0x41e). RDI is the settings argument (`MOV RDI,RDX` at `0x140857301`). Both accesses are word-sized. This copy is the last game write before the hook. It is not the only one.
- **The hooked function does not write it (VERIFIED, ev_c08f32fe).** `FUN_1400f72b0` writes only these scene fields: +0x308, +0x30c, +0x310, +0x314 (qword, so 0x314..0x31b), +0x318, +0x31c, +0x320, +0x324, +0x328, +0x32c, +0x334 (word), +0x338..+0x344, +0x290, +0x294, +0x295, +0x2ec, +0x2f0..+0x304, and a dword at +0x420 (`0x1400f7647`..`0x1400f7677`). No store reaches +0x41e or +0x41f. The function makes no call (it is a leaf). Its only caller is `0x1408572e0` (ev_c8219d03).
- **Nothing overwrites it after the hook (VERIFIED, ev_62eac43d).** Between the hook's return (`0x1408576b4`) and `CALL 0x1400f4ae0` (`0x1408576f7`), the caller writes only scene+0x0 (byte, `0x1408576d2`), +0x41c (byte, `0x1408576d9`), +0x40c..+0x41b (16 bytes, `0x1408576e9`) and +0x325 (byte, `0x1408576f0`). The only other store is the outgoing argument at `[RSP+0x20]` (`0x1408576e4`), which is outside the scene. So a value written by a post-hook on `FUN_1400f72b0` survives to the reader.
- **When the hook does not run (VERIFIED, ev_62eac43d).** `FUN_1400f72b0` is called only when settings+0x50 and settings+0x58 are both non-null (`MOV RDX,[RDI+0x50]` at `0x140857650`; TEST/JZ at `0x14085769d`..`0x1408576a9`). Otherwise execution jumps to `0x1408576b4`, and +0x41e keeps the value copied at `0x140857537`. That frame uses the game's own peak.
- **The colour-space byte `scene+0x408` (VERIFIED).** HDR is true if and only if the byte is 1 or 2.
  - `FUN_140fe0ab0` is the game's HDR test: `MOVZX EDX,CL; SUB EDX,1; JZ true; CMP EDX,1; JZ true; XOR AL,AL` (ev_2cdc99bb).
  - The byte is written before the hook: `MOVZX EAX,byte [RDI+0x38]` at `0x140857502` and `MOV [RBP+0x3f8],AL` at `0x14085751b` (ev_62eac43d). No later store in `0x1408572e0` or in `FUN_1400f72b0` touches +0x408.
  - settings+0x38 is copied from `[[R14]+0x38]` in `FUN_140855ad0` (`0x1408562a0`..`0x1408562d5`; ev_25a16057). That is the same byte `FUN_140855ad0` tests at `0x1408567ba` to choose the peak.
  - `FUN_140129fc0` carries the byte to PP+0x50 (`0x14013b331`, `0x14013b338`; ev_2f3fb6f7). `FUN_140a4f430` tests it again (`0x140a4f697`, then the call at `0x140a4f75e`; ev_879450b4).
  - HYPOTHESIS: the byte is assumed unchanged in memory between the two reads.
- **settings+0x240 (VERIFIED, ev_25a16057).** `FUN_140855ad0` computes it on every call:
  - SDR: 110. `FUN_140fe0ab0(byte [[R14]+0x38])` is false (`MOV RCX,[R14]` at `0x1408567b7`; `MOVZX ECX,[RCX+0x38]` at `0x1408567ba`). `MOV EAX,0x6e` is at `0x1408567ed`.
  - HDR with no swapchain: 600. `FUN_1408fcc80([RSI+8])` returns null, and its result is the swapchain argument of the next call. `MOV EAX,0x258` is at `0x1408567e6`.
  - Otherwise: `FUN_140fdb640([RSI+0x20], swapchain) >> 16` (`SHR EAX,0x10` at `0x1408567e1`). That is max(80, swapchain+0x188), as shown below.
  - The result is stored by `MOV word [RBP+0x2a0],AX` at `0x1408567f9`. The settings struct lives at RBP+0x60 (`LEA RDX,[RBP+0x60]` at `0x140856807`), so this is settings+0x240. `CALL 0x1408572e0` follows at `0x14085680b`. A default of 110 is also written at `0x1408561c8`/`0x1408561cd`.
  - A local scan found no jumps into or out of `0x14085627c`..`0x14085680b`. The only branch that skips the range is the error path `0x140855d60` to `0x140856b7f`, which never returns.
  - `0x1408572e0` has one caller, `FUN_140855ad0` (ev_77d32104).
- **The value behind `FUN_140fdb640` (VERIFIED).**
  - `FUN_140fdb640` (ev_6d26a6c3) loads `RCX=[RCX+0x278]`, sets `R8=swapchain` and `RDX=&stack_out`, calls `[vtable+0x140]`, and returns the dword written. The vtable is `0x141da69e0` (`LEA RAX,[0x141da69e0]` at `0x140faaa57`, stored to `[RCX]` by `48 89 01`, inside `FUN_140faaa40`). The slot at `0x141da6b20` holds `0x140fdb5f0`. The bytes at `0x141da6b18` are `50adfd40 01000000 f0b5fd40 01000000` (ev_fbbdac4f). `0x141da6b20 - 0x141da69e0 = 0x140`. xrefs: `0x140fdb5f0` = `[0x141da6b20]` (ev_df08816e).
  - HYPOTHESIS: that the object at `[arg+0x278]` has this vtable is inferred. No store into `[arg+0x278]` was traced. One fact supports it: the caller `0x140fcc990` allocates `0x30e0` bytes (`0x140fcc9c6`) before it calls the constructor `0x140faaa40`, and that size covers the `+0x30d8` field used below.
  - `0x140fdb5f0`..`0x140fdb638` (ev_f9799d88): `cmp qword [rcx+0x30d8],0; je 0x140fdb625; test r8,r8; je 0x140fdb625; movzx eax,byte [r8+0x18a]; movzx ecx,word [r8+0x188]; mov [rdx],al; mov eax,0x50; cmp cx,ax; cmova ax,cx; mov [rdx+2],ax; mov rax,rdx; ret`. At `0x140fdb625`: `movzx eax,byte [r8+0x18a]; mov [rdx],al; mov rax,rdx; mov word [rdx+2],0x258; ret`.
  - So the out u16 at +2 is max(80, u16 swapchain+0x188) when `[device+0x30d8] != 0` and the swapchain is non-null. Otherwise it is 600. The out byte at +0 is swapchain+0x18a. Byte 1 of the out struct is never written, and the caller's `>> 16` discards it. The 600 path still reads `[r8+0x18a]` when r8 is null. The caller guards against null (`JZ` at `0x1408567d3`).
- **The DXGI source (VERIFIED; one field name is HYPOTHESIS-strong).** swapchain+0x188 (u16 nits) is written when a swapchain is created, in `FUN_140fc7860` (ev_17b1bd20). That it happens only at creation, not per frame, is HYPOTHESIS from the function's role.
  - `CALL 0x14100c260` at `0x140fc803c` has RCX = RBP, the result struct. A non-zero byte at `[RBP]` takes the error-log path. Otherwise `LEA RDX,[0x141dac2b8]` at `0x140fc80b8` and a log call print "[graphics] Queried monitor luminance through dxgi of %u nits" (string `0x141dac2b8`; ev_ec06bbd1, xrefs ev_1e029845). Then `MOVZX EAX,word [RBP+8]` at `0x140fc80ce` and `MOV word [RDI+0x188],AX` at `0x140fc80d2` (RDI = arg 2).
  - The swapchain-init default is 600: `66 c7 87 88 01 00 00 58 02` (`mov word [rdi+0x188],0x258`) at `0x140fcaa54`, in `FUN_140fca970` (`0x140fca970`..`0x140fcad80`; ev_ac8d7471).
  - That `FUN_140fc7860` is the Vulkan path is HYPOTHESIS. It calls through dispatch-table slots +0x580 and +0x590.
  - `FUN_14100c260` (ev_e7677e49) picks the DXGI output whose `desc.Monitor` equals `MonitorFromWindow(hwnd, 2)` (`CALL [0x1413b2b20]` at `0x14100c28c`; compare `CMP [RBP-0x38],RDI` at `0x14100c326`). It enumerates through vtable +0x38 (factory EnumAdapters, adapter EnumOutputs, output GetDesc into a 0x60-byte desc). On a match it queries the IID at `0x141daceb8` (bytes `e8468306 ecaa 844b add7137f513f77a1`, which is `{068346e8-aaec-4b84-add7-137f513f77a1}`, IDXGIOutput6; ev_8eccd2dc) and calls vtable +0xd8 (GetDesc1) into `[RBP-0x30]`.
  - `CVTTSS2SI EAX,[RBP+0x60]` at `0x14100c46b` reads desc+0x90 and truncates it to an integer. `MOV EBX,0x50 / CMP AX,BX / CMOVA BX,AX` floors it at 80. The word goes to result+8 (`0x14100c4e1`, `0x14100c50c`).
  - By the public `DXGI_OUTPUT_DESC1` layout, +0x8c is `MaxLuminance` and +0x90 is `MaxFullFrameLuminance`. HYPOTHESIS-strong: that field name comes from the Windows SDK layout, not from the exe. So the game's peak is the MaxFullFrameLuminance, floored at 80. This is the "800 nits" in the game log ("Queried monitor luminance through dxgi of 800 nits").
- **The other backend path looks inactive (bytes and log VERIFIED; the conclusion is HYPOTHESIS).** `FUN_14100a1f0` calls `FUN_14100c0f0`. That function reads the same desc+0x90 (`CVTTSS2SI EAX,[RSP+0xe0]` at `0x14100c1b0`), applies the same floor of 80, and `FUN_14100a1f0` writes swapchain+0x188 at `0x14100a631` (ev_223b8a69, ev_7bae527a). Its log "[graphics] Detected display peak luminance at %u nits" (string `0x141dacf78`; `lea rdx` at `0x14100a5de`) does not appear in the reference game log. The log shows only the "Queried monitor luminance through dxgi" line. So the path above is the active one. This is an inference. It holds unless the other path ran and failed before it logged.
- **The only reader and the chain to the shader (VERIFIED).**
  - The scene pointer reaches `FUN_140129fc0` unchanged. `0x1408572e0` passes `LEA R9,[RBP-0x10]` (`0x1408576bb`) to `FUN_1400f4ae0`. That function keeps it in R13 (`MOV R13,R9` at `0x1400f4b36`) and passes `MOV R9,R13` (`0x1400f4ca3`) to `CALL 0x140129fc0` (`0x1400f4cab`). `FUN_140129fc0` has only this caller (ev_1cf1c89f, ev_8e9dcba7).
  - In `FUN_140129fc0` (ev_2f3fb6f7): `MOV RDI,[RBP+0x2b6c8]` at `0x14013aea0` loads the R9 home (param_4, the scene). The listing has no store to that slot, and no jump enters `0x14013aea7`..`0x14013b412` from outside. `MOV dword [RBP+0xbfc],0x42dc0000` (110.0) at `0x14013b300` pre-sets PP+0x9c. Then `MOVZX EAX,word [RDI+0x41e]` at `0x14013b412`, `MOVD`/`CVTDQ2PS`, and `MOVSS [RBP+0xbfc]` at `0x14013b420` store the peak as a float in nits at PP+0x9c. The post-process block starts at RBP+0xb60 (0xbfc - 0xb60 = 0x9c). `LEA R8,[RBP+0xb60]` at `0x14013b3c1` comes before `CALL 0x140a4f430` at `0x14013b436`. This is the only access to +0x41e in the function.
  - LIMIT: "only reader" was checked only along the traced path (`0x1408572e0`, `0x1400f4ae0`, `0x140129fc0` up to `0x14013b412`, and one level of callees that receive the scene pointer). Deeper callees were not checked.
  - `FUN_140a4f430` is the post-upscale constants function (ev_879450b4). `R14 = R8` (`0x140a4f461`). It copies `[R14+0x9c]` to `[RBP+0x80]` (`0x140a4f6d8`, `0x140a4f6e1`). The tonemapper selector `[RBP-0x30]` = 6 (`MOV dword [RBP-0x30],6` at `0x140a4f544`; the name "GT" is HYPOTHESIS). The GT parameters are `[RBP+0x84]` = 80.0 (`0x42a00000`), +0x88 = 0.4 (`0x3ecccccd`), +0x8c = 0.98 (`0x3f7ae148`) and +0x90 = 1.16 (`0x3f947ae1`).
  - The correction factor `[RBP+0x94]` is set to 1.0 at `0x140a4f767` when `FUN_140fe0ab0(byte [R14+0x50])` is true (the call is at `0x140a4f75e`), that is, in HDR. Otherwise it is 80/peak: 1.0 (`0x1413b3c90`, bytes `0000803f`, ev_a4a84857) divided by `[R14+0x9c]` / 80.0 (`0x1413b65f8`, bytes `0000a042`, ev_ae535f9c), at `0x140a4f773`..`0x140a4f788`. The GT curve parameters at `[RBP+0x68]`..`[RBP+0x74]` are built from peak/80 (`0x140a4f790`..`0x140a4f813`).
  - Upload: the 0xd0-byte tonemapper block at `RBP-0x30` (`RAX=RBP-0x30` at `0x140a4f87b`) is copied to `RBP+0x120` (copy `0x140a4f8b1`..`0x140a4f932`). So the peak (block+0xb0 = RBP+0x80) lands at `RBP+0x1d0`. The UBO image starts at `RBP+0xa0` (`LEA R8,[RBP+0xa0]` at `0x140a4fc46`; `CALL 0x140a49fc0` at `0x140a4fcd9`). `FUN_140a49fc0` (ev_da7ce009) asks for 0x150 bytes and copies 0x150 bytes from that pointer (calling `FUN_140049aa0` an allocator is an inference). So the peak is at UBO offset `0x130` (0x1d0 - 0xa0), as a float in nits.
- **Shader side (NOTES: read from a shader disassembly, not from exe bytes; HYPOTHESIS until the in-game A/B test).** Post-upscale shader: the UBO is 0x150 bytes. The member chain @128, @128, @48 gives offset 304 = `0x130` (SSA id %900). The tonemapper switch is member 0 of the second block, and case 6 is GT. %82 = %900 / 80. The GT result is `min(x, %82)`, multiplied by %904 (offset `0x144`, the correction factor). The GT path scales by 80/10000 before the PQ encode, so 1 output unit = 80 nits. The tonemapper runs only when flag bit `0x40` is set. The layout matches the exe writes above (selector 6, size 0x150, flag `0x40`). Composite shader: UBO +8 is paperWhite. In HDR the composite multiplies the sRGB-decoded frame by paperWhite / 10000 and PQ-encodes it (PQ 1.0 = 10000 nits). So the displayed nits = the intermediate value x paperWhite.
- **Paper white is in another function and another UBO (VERIFIED, ev_c9585570).** The bytes at `0x1408f88b3` (length 32) are `48 89 45 80 | 45 33 e4 | 8b c3 | c7 45 88 00 00 7a 43 | 49 8b 16 | c7 45 8c 9a 99 19 3f | f3 48 0f 2a c8`. The instruction `c7 45 88 00 00 7a 43` (`mov dword [rbp-0x78], 0x437a0000` = 250.0) is at `0x1408f88bc`. Its `imm32` is at `0x1408f88bf`. The next store is `mov dword [rbp-0x74], 0x3f19999a` (0.6) at `0x1408f88c6`. All of it lies in `FUN_1408f8800` (`0x1408f8800`..`0x1408f8dea`). That is a different function and a different UBO from the peak upload (`FUN_140a4f430`, the post-upscale UBO). "Composite constants block" is a naming label (HYPOTHESIS). See also "HDR paper white".
- **The second scene builder is always SDR (VERIFIED, ev_bf9f6353, ev_21ba404d, ev_2d50e4fe).** `FUN_1400f4ae0` has two callers, `0x14023f140` and `0x1408572e0`. `FUN_14023f140` builds its own scene struct at `RBP-0x40` (`LEA R9,[RBP-0x40]` at `0x14023f7e4`; `CALL 0x1400f4ae0` at `0x14023f823`). It sets scene+0x41e = 110 (`MOV EAX,0x6e` at `0x14023f29d`; `MOV word [RBP+0x3de],AX` at `0x14023f4d0`). It sets scene+0x408 = 0 (`MOV byte [RBP+0x3c8],R13B` at `0x14023f495`; R13 is zero from `0x14023f180`). Its later copy loops write `RBP+0xf4..0x208` and `RBP+0x3f0` and up, so they do not touch these fields. It does not call `FUN_1400f72b0`. Because +0x408 = 0, `FUN_140fe0ab0` is false in `FUN_140a4f430`, so this view always renders SDR. A hook on `FUN_1400f72b0` neither reaches it nor needs to.
- **Formula for the peak field (derived; the shader steps are NOTES-level).** The game's tonemapper output is in 80-nit units and is capped at peak_field / 80. The composite multiplies by paperWhite (nits per unit) before the PQ encode. So the brightest displayed value is peak_field x paperWhite / 80 nits. To end the roll-off at the panel peak P (in nits), set `peak_field = P x 80 / paperWhite`. In code: `v = round(target_nits x 80 / paper_white)`, clamped to `1..=10000`, written as a u16 at `scene+0x41e`.
  - `target_nits` is the panel peak in nits (cd/m2). `paper_white` is the value actually in effect: the patched float if the paper-white patch is on, else 250.0 (the stock immediate at `0x1408f88bf`). Both features must read the same configured value, or the roll-off lands in the wrong place.
  - Never write 0. `FUN_140a4f430` divides by peak/80 in the SDR branch and builds the GT curve from peak/80. Skip the write if `paper_white` is not finite or is <= 0.
  - Examples: paper white 250 and a panel of 800 nits give v = 256. Paper white 200 gives v = 320. Paper white 80 gives v = 800, the same as the game's own value for an 800-nit report. So with paper white 80 the game's value already gives the right roll-off, and this route only replaces the DXGI full-frame figure.
  - Write it only when `scene+0x408` is 1 or 2. In SDR the field is 110, and it also drives the SDR correction factor 80/peak.
  - Where and when: a post-hook on `FUN_1400f72b0` (scene = RCX), after the original returns, on every call. The scene struct is rebuilt on every call. So read the game's fresh value each time and never accumulate.
- **Not verified (HYPOTHESIS or open).**
  - No other `.text` instruction uses displacement `0x41e` as a word. The other hits are byte stores to `[rbx+0x41e]` in unrelated code at `0x14126c32b`, `0x14126cdfb`, `0x1412700ab` and `0x14127a80b`. They belong to a different struct (HYPOTHESIS). Displacement `0x40e` is word-written only at `0x140857537`. A displacement scan cannot see memcpy-style or computed-pointer writes. So a hidden overwrite between the hook and `0x14013b412` is not fully excluded.
  - The tonemapper mode jump table at `0x140a503a8` (indexed by PP+0x51; the table bytes were read in REA, ev_edb89165). Index 0 goes to `0x140a4f980` (flags `0x40` or `0x44`, and UBO member 10 at `0x38` = PP+0x58 = scene+0x310). Index 1 gives flag `0x40`, 2 gives `0x20`, 3 gives 0, 4 gives `0x40` with member 4 = 1, 5 gives `0x400`, and 6 gives `0x800`. Only modes with flag `0x40` run the GT tonemapper. That normal gameplay uses index 0 is an assumption.
  - The peak is computed per call of `0x140855ad0` (callers `0x140856ba0` and `0x1408578a0`; ev_c8bd5c50) and copied per call of `0x1408572e0`. That each runs once per rendered frame is HYPOTHESIS-strong (see "Path A, per frame").
  - No ini or settings override feeds settings+0x240. Only the constants 110 and 600 and the swapchain value were found. This is a negative search, so it is HYPOTHESIS.
  - Open: does the HDR picture roll off at the panel peak with the paper-white patch and this one (in-game A/B test, plan Task 17)? Is the intermediate between the post-upscale and composite passes a float format? If it is UNORM, values above 1.0 clip before the composite and this route has no visible effect. Does the author's panel report a MaxLuminance above its MaxFullFrameLuminance? A log of both DXGI fields would answer.

## Reflex and vsync menu values seen from the scene-settings hook (2026-10-26; plan Task 16)

This section confirms the "Reflex menu option location" section with static evidence. Evidence: REA/Ghidra and capstone scans on the old-build exe (sha256 `b28b448a...`; the Steam exe has the same code). A second byte check re-derived each item. Items are VERIFIED (static bytes) unless marked otherwise. No run-time read has been done. The values stay unconfirmed until one log line from the hook shows `reflex=1` and `vsync=<menu value>` on the author's machine.

- **One caller (VERIFIED, ev_a1600500, ev_c8219d03).** `FUN_1400f72b0` has exactly one reference: the `CALL` at `0x1408576af` inside `FUN_1408572e0`. A raw scan of the file for an `E8`/`E9` rel32 to `0x1400f72b0`, an 8-byte absolute pointer and a 4-byte RVA finds only that `E8`. The function has no `.pdata` entry because it is a leaf. So there is no vtable or function-pointer use.
- **Arguments at the call (VERIFIED, ev_62eac43d).** In `FUN_1408572e0`, RDI = arg 2 (`MOV RDI,RDX` at `0x140857301`). The call has RCX = `LEA [RBP-0x10]` (the stack scene struct, `0x1408576ab`), RDX = `[RDI+0x50]` (`0x140857650`) and R8 = `[RDI+0x58]` (`0x1408576a2`). It is made only when both pointers are non-null (`0x14085769d`..`0x1408576a9`). Nothing writes RDI between `0x140857301` and the call.
- **The caller of the caller (VERIFIED, ev_77d32104, ev_25a16057).** `FUN_1408572e0` has one caller, `FUN_140855ad0`: `LEA RDX,[RBP+0x60]` at `0x140856807`, then `CALL 0x1408572e0` at `0x14085680b`. So `[RDI+0x50]` is `[RBP+0xb0]` and `[RDI+0x58]` is `[RBP+0xb8]` in `FUN_140855ad0`.
- **RDX is the live options+0x9f8, not a copy (VERIFIED, ev_25a16057, ev_4d4695c8).** `FUN_140855ad0` loads RCX = `[RSI+0xb8]` (the options object; `0x140855b1c`) and calls the getter `0x140e28400` at `0x140855b28`. The getter is `LEA RAX,[RCX+0x9f8]; RET`. The result goes to `[RBP-0x30]` (`0x140855b34`), then to `[RBP+0xb0]`, which is struct+0x50 (`MOV RAX,[RBP-0x30]` at `0x1408562e0`, `MOV [RBP+0xb0],RAX` at `0x1408562eb`). `[RBP-0x30]` is written only at `0x140855b34`. Nothing writes `RBP+0xb0`..`RBP+0xb7` between `0x1408562eb` and the call at `0x14085680b`, and no jump from outside enters that window. No call in the window receives a pointer into the `RBP+0x60` struct. The only loop copy writes upward from `RBP+0x110`.
- **R8 is options+0xa4c, so R8 - RDX = 0x54 (VERIFIED, ev_25a16057, ev_e021a757).** `FUN_140855ad0` loads the options object (`MOV RCX,[RCX+0xb8]` at `0x140855afa`). It calls the machine-struct getter `0x140e28240` at `0x140855b01`, which is `LEA RAX,[RCX+0x9c0]; RET`. It stores the result to `[RBP-0x28]` (`0x140855b13`), adds `0x8c` (`ADD RAX,0x8c` at `0x1408562fd`) and stores the sum to `[RBP+0xb8]` (`0x140856303`). So R8 = options+0xa4c (0x9c0 + 0x8c). That is `renderPreferences` (descriptor offset 0x88 + base 0x9c4). At the hook, R8 - RDX = 0xa4c - 0x9f8 = 0x54. This is a cheap check that both arguments come from one live options object.
- **The options object (VERIFIED, ev_d65c50ee, ev_17588f7b).** `FUN_140e24e90` allocates it: `MOV R8D,8` at `0x140e24e9e` and `MOV EDX,0xd78` at `0x140e24ea4`, then a call through the allocator's `[RAX+8]`. So the size is 0xd78 and the alignment is 8 (calling R8D the alignment is an inference). The defaults are:
  - byte `+0x9c4` = 1 (`0x140e24f2f`);
  - float `+0x9e8` = 1.0 (`0x140e24f52`);
  - dword `+0x9ec` = `0x10101` (`0x140e24f5c`), so the byte at `+0x9ed` (vsync) is 1;
  - qword `+0x9f0` = 1 (`0x140e24f66`), so nvidiaReflex = 1 (On) and nvidiaReflexDefaultOff = 0 (Off);
  - dword `+0x9f8` = 2 (`0x140e24f71`), the first render-settings field.
  
  So `+0x9ed` and `+0x9f0` lie well inside the allocation, and `+0x9f0` is naturally aligned. The only xref to `0x140e24e90` is in `FUN_140e27e20` (`0x140e27e5a`). `FUN_140e27e20` returns the new object, and `FUN_14084f070` stores it at `param_1+0x128`.
- **Field offsets (VERIFIED, ev_a32dc9eb, ev_0b962658, ev_332cfd41, ev_3635889c).** Reflection descriptor rows have stride 0x30 and the layout {name pointer, length, type pointer, offset}:
  - `0x14177a460` renderScale +0x24; `0x14177a490` automaticRenderScale +0x28;
  - `0x14177a4c0` `vsync` (name string `0x141c403b4`) +0x29;
  - `0x14177a4f0` hdrSettings (name length 0xb) +0x2a;
  - `0x14177a520` `nvidiaReflex` (name string `0x141c21ca0`, type `0x141779da0`) +0x2c;
  - `0x14177a550` `nvidiaReflexDefaultOff` (same type) +0x30;
  - `0x14177a580` render +0x34; `0x14177a5b0` renderPreferences +0x88. The earlier rows are windowMode +0 and windowPosition +4.
  
  The name strings have no Ghidra xrefs. A raw 8-byte pointer search found the rows. The machine struct starts at options+0x9c4. Three facts agree: render at 0x9c4 + 0x34 = 0x9f8 (the getter), renderPreferences at 0x9c4 + 0x88 = 0xa4c (above), and the windowMode default byte at `0x9c4` is 1. So, at the hook:
  - `nvidiaReflex` = `*(u32*)(RDX - 0x8)` (options+0x9f0);
  - `vsync` = `*(u8*)(RDX - 0xb)` (options+0x9ed);
  - `nvidiaReflexDefaultOff` = `*(u32*)(RDX - 0x4)` (options+0x9f4).
- **The enum (VERIFIED, ev_cb81b898, ev_59405ed6).** The type object `0x141779da0` is named `keen::g38_options::pc::machine::NvidiaReflexModeSettings`. It has count 3 and size 4, and it points to the member table `0x141779b80` (stride 0x28). The members are `0x141779b80` = {`0x141c68b08` "Off", length 3, value 0}, `0x141779ba8` = {`0x141c68978` "On", length 2, value 1} and `0x141779bd0` = {`0x141c2122c` "Boost", length 5, value 2}. So Off = 0, On = 1, Boost = 2.
- **The hooked function never reads those bytes (VERIFIED, ev_c08f32fe).** `FUN_1400f72b0` is a leaf: it has no `CALL`, and it ends with `RET` at `0x1400f7710`. It reads `[RDX+0x24]`, `+0x28`, `+0x2c`, `+0x34`, `+0x38`, `+0x3c`, `+0x44` (twice), `+0x4c` and `+0x50`. It reads `[R8+0]`, `+4` and `+8` (twice). It also reads three `.rdata` float constants (`0x1413b3c90`, `0x1413b88bc`, `0x1413b65d0`). All writes go to `[RCX+...]`. No displacement is negative, so it never touches `RDX-0xb` or `RDX-0x8`.
- **Other consumers of the options pointers (VERIFIED).**
  - The getter `0x140e28400` has three callers (ev_9339b888). `0x140840f44` passes the result as RDX to `0x1400f7ac0` (Path B). `0x140849104` passes it as R8 to `0x1400f82c0` (the texture-budget setup), which overwrites R8 at `0x1400f82fc` before any read (ev_2bfa11d0). `0x140855b28` is Path A. Neither `0x1400f7ac0` (it reads only `[RDX/RDI + positive]`; ev_8ccd39c8) nor `0x1400f82c0` has a memory operand with a negative displacement on a non-stack register. So no getter consumer reads `RDX-0x8` or `RDX-0xb`.
  - The machine-struct getter `0x140e28240` (options+0x9c0) has six callers (ev_6419367f). None reads +0x30 or +0x34 (the Reflex fields) from its result. `0x1403a8e5f` reads +0xa0. `0x14084f25a` reads +4, +8, +0x10, +0x14 and +0x2d. `0x140855b01` uses +0x8c. `0x14086802f` reads +0x9c. `0x140875594` reads +0xa4. `0x14086e0b8` copies all 0xa8 bytes to `[obj+0x33e08]`.
  - Apply, `FUN_140e275d0` (ev_d99b6427) (corrected by the byte verifier). It forms options+0x9f8 twice. `ADD RCX,0x9f8` at `0x140e275e9` (after `LEA RDX,[RCX+0x9b0]` at `0x140e275e2`) goes to `0x1400f8840` (ev_48605215). That function reads `[RCX+0]`, `[RCX+4]` and `[RCX+8]` and writes `[RCX+4..+0x50]`. So it changes the live render settings at +0x9fc..+0xa48. Through R9 (= options+0x9b0) it also reads options+0x9b0, +0x9b4 and the byte at +0x9b0. `LEA R8,[RBX+0x9f8]` at `0x140e2768a` goes to `0x1400f8530`, which is `RET 0` (ev_e7ab8450). Apply also passes options+0xa4c to `0x1400f87d0`, which clamps three floats at +0, +4 and +8. Neither callee reads a negative offset, and none touches +0x9ed or +0x9f0..+0x9f7.
- **No code was found that acts on the Reflex value (VERIFIED as far as scanned) (corrected by the byte verifier).** The earlier access list was incomplete. The accesses that overlap options+0x9f0..0x9f7 are writes and bulk copies, not decisions.
  - Displacement hits in the options code: `0x140e24f66` (defaults, qword = 1), `0x140e2c50e` (dword = 0), and `0x140e2c13f`/`0x140e2c14b` (the struct copy in `FUN_140e2bff0`).
  - Accesses through an options+0x9c0 base, which a displacement scan misses (ev_8210012e, ev_54ff8773). First, `0x140e2c520` (the `--use-default-settings` defaults) builds a stack template with qword 1 at +0x30 and writes it with a bulk copy (`MOVUPS [RDX+0x30]` at `0x140e2c635`). So it sets nvidiaReflex = 1 and DefaultOff = 0. `FUN_140e2c4f0` calls it first. Second, `FUN_140e2ce10` bulk-reads at `0x140e2ce4e` and bulk-writes from a caller struct at `0x140e2cfd6`. `0x1408468c6` calls it to restore the `obj+0x33e08` snapshot. Its compares cover +0x28 and +0x38..+0x88 only. Third, `FUN_140e2bff0` bulk-reads at `0x140e2c04e`. Fourth, the generic reflection load, save and diff paths get options+0x9c0 at `0x140e28b65`, `0x140e2da35`, `0x140e2dbea` and `0x140e2f5f2` (which goes to `0x140e31540`). The only other reads of the snapshot are bulk loads (`0x1401ec16c`).
  - Method: capstone over every `.pdata` function, plus a full `.text` linear sweep (restarted after bad bytes, base+index operands included). The sweep reaches leaf functions that have no unwind info, such as `0x1400f72b0` and `0x140e28a00`. It found no field-specific read of +0x30 or +0x34. This is the checked scope. It is not a proof for every code path.
  - This adds to the "Research results, 2026-10-25" bullet "No `.text` code reads either field": the notes missed the bulk writers above.
- **Start-up and reset paths that write the value (VERIFIED).**
  - `FUN_14084f070` creates the `G38Options` object (allocator `FUN_1408b3e00`; object via `FUN_140e27e20` to `0x140e24e90`; stored at `+0x128`). If `[param_1+0x1ec]` (the start-up init mode) is 2 or 3, it calls `FUN_140e2c4f0` at `0x14084f24e`. Otherwise `0x140887fa0(cmdline, "--use-default-settings")` (string at `0x141d48d68`) decides. True goes to `0x140e2c520`. False loads the settings file with `0x140e28a20` (ev_b9c01a51).
  - `FUN_140e2c4f0` (ev_462db712) first calls `0x140e2c520` (the defaults, On). Then it writes byte `[+0x9c4]` = 1 (windowMode), word `[+0x9ec]` = 0 (automaticRenderScale and vsync off) and dword `[+0x9f0]` = 0 (nvidiaReflex Off; `0x140e2c50e`). So init mode 2 or 3 forces Reflex Off together with vsync off. Its xrefs are `0x14084f24e` and `.pdata` (ev_1e1ce75b).
  - `FUN_140e2bff0` (the graphics reset; ev_3139afcd) builds a stack struct with `0x10101` at the `0x9ec` slot and qword 1 at the `0x9f0` slot. It copies the struct to `+0x9c4`..`+0xa5b` and calls apply `0x140e275d0` at `0x140e2c193`. So a graphics reset writes nvidiaReflex = 1 (On), DefaultOff = 0 and vsync = 1.
- **Where vsync is consumed (VERIFIED).**
  - At start-up: `XOR EAX,EAX` at `0x14084f2b0`, `CMP byte [RDX+0x2d],AL` at `0x14084f2b2` (RDX is the `0x140e28240` result, so options+0x9c0 + 0x2d = options+0x9ed), `SETNE`, then `MOV [RBP+0xb4],EAX` at `0x14084f2b8` (RBP = param_2). So it stores (vsync != 0) in `[cfg+0xb4]`. That this config feeds device creation is an inference. The bytes do not show it. The next call, `FUN_14084eb60` (ev_e55acd21), handles `last_crash.json` and does not create the device.
  - On apply: `FUN_140e275d0` (ev_d99b6427) calls `0x140fdcff0` at `0x140e2763b` (`LEA RAX,[RCX+4]; RET`; ev_a4d38e0f). At `0x140e27643` it does `CMP byte [RBX+0x9ed],R8B` and `SETNZ`. It compares the result with the dword at `RAX+0xc`, which is `[options+0x30]+0x10`. If they differ, it calls `0x140ff63b0` at `0x140e2766c` with RCX = `[options+0x28]` (the device), RDX = `[options+0x30]` (the swapchain) and R8 = vsync. `0x140ff63b0` (ev_57fd2725) calls `0x140898830` (a jump to the IAT `EnterCriticalSection`) on `dev+0x710`. Then it calls `(*[dev+0x278])->vtable[+0xd0](this, swap, vsync)`. Then it calls `LeaveCriticalSection`.
- **Other hits for offsets 0x9ec..0x9f8 are other structs (VERIFIED for the first two kinds).** `FUN_140667f10` reads dword `[RSI+0x9ec]`, `[RSI+0x9f0]` and `[RSI+0x9f4]` at `0x1406681e7`, `0x14066823c` and `0x140668291`, next to the strings "Failed to set TCP_KEEPIDLE" and "TCP_KEEPINTVL" (ev_b475e5df). These are libcurl TCP keepalive fields (idle, interval, count; defaults 60, 60 and 9) of a curl handle. `0x1406b92a5`, `0x1406b92af` and `0x1406b92b9` write 0x3c, 0x3c and 9. `FUN_1406a9480` writes at `0x1406aa4e5`..`0x1406aa525`. `0x140b2acd9` and `0x140b2b43c` are `[base+index+disp]` block copies. The hit list is not complete.
- **Thread note (reasoning, not a byte fact).** The menu and apply path can write the options object (for example the struct copy in `FUN_140e2bff0`) while the hook reads it. The u32 at options+0x9f0 is naturally aligned (the allocation is 8-aligned), and the vsync read is one byte. So the worst case is a stale value, never a torn one. This holds only while the allocation alignment of 8 (`0x140e24e9e`) holds. Which thread runs `FUN_1400f72b0`, compared with the thread that writes the options, was not checked. It matters only for staleness.
- **Rule for the reader (design, from the facts above).**
  - Read the values in the post-hook on `FUN_1400f72b0`, with `render = RDX` and `prefs = R8`. The original writes only the scene struct (RCX), so the hook may read before or after it calls the original.
  - Guard: `render != 0` and `prefs == render.wrapping_add(0x54)`. The guard shows that both pointers lie in one live options object, which is what the only caller passes. `RDX-0xb` is safe to read only inside this guard, because RDX = options+0x9f8 lies in the 0xd78-byte allocation. If the guard fails, read nothing and report "unknown".
  - Accept `nvidiaReflex` only when it is <= 2, and `vsync` only when it is <= 1. Use volatile reads.
  - Do not call the getter `0x140e28400` at start-up. It needs the options pointer (`[RSI+0xb8]` in `FUN_140855ad0`, or `[init+0x128]` in `FUN_14084f070`). The plugin has no verified way to get that pointer. At ASI load the object does not exist yet, because `FUN_14084f070` creates it later (reasoning).
- **What the value means (design).** No code that acts on the value was found (above), and the Reflex row is hidden on Vulkan (see "Decisions (the author, 2026-10-25, second questionnaire)"). So the value is the saved-file value or the default (On = 1), or Off in init modes 2 and 3. Treat it as a hint for the `auto` setting, not as an explicit choice by the player.
- **Not verified (HYPOTHESIS or open).**
  - Vtable slot `+0xd0` of the device sub-object (`[[dev+0x278]]+0xd0`) probably sets the swapchain present mode (FIFO when vsync is on). The indirect target was not resolved. The call happens only when the vsync bool differs from the swapchain state.
  - Hits at offsets 0x9ec..0x9f8 that were classified only by pattern (HYPOTHESIS): qword pointer-field loads at `+0x9e8`, `+0x9f0` and `+0x9f8` in `0x1400e5477`..`0x140205a5e` (renderer structs), and `LEA [reg+0x9f0]` sub-objects in the Vulkan device code at `0x140fa5ddf`, `0x140faa89c`, `0x140fbd86e`, `0x140fc3a6f`, `0x140fd05d2`, `0x140fd0e14` and `0x140fd8f43`. Further non-stack hits were only spot-checked as other objects.
  - Two snapshot copies were not followed downstream: options+0x9c0 (0xa8 bytes) to `[obj+0x33e08]` at `0x14086e0b8` (the copy run is `0x14086e112`..`0x14086e173`), and the bulk copy at `0x1401ec16c` that includes `[rdx+0x33e30..0x33e3f]`. Some code could read the copied Reflex byte there.
  - Open: what init modes `[init+0x1ec]` = 2 and = 3 mean (dedicated server, benchmark or safe mode?). Which function sits in vtable slot `+0xd0`. What the run-time value is on the author's machine (expected 1 = On).
