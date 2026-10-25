# Enshrouded reverse-engineering notes

Target: `enshrouded.exe`, sha256 `b28b448a8dbf48c932b717f035031f5a89712a7cccfcc2d6cac0d47e6421c12a`, build `f68140a3`, image base `0x140000000`.
Tools: REA 6.3.0 + Ghidra 12.1.4, and capstone/pefile scratch probes (`xref.py`, `callers.py`, `func.py`, `stores.py` in the session scratchpad).
Evidence labels: **VERIFIED** = read from bytes, decompile, or game log. **HYPOTHESIS** = inferred from names or neighbours.

## Shipped in the launcher (`C:\Games\Enshrouded\_perf\`)

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
