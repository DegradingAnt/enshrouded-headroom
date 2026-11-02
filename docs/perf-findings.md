Measured by the author on 2026-10-23. The plugin now makes all three launcher changes in-process; do not use the launcher and the plugin together.

# Enshrouded performance findings (2026-10-23)

Target: `enshrouded.exe`, sha256 `b28b448a…c12a`, build `f68140a3`, log app version 1273043.
Rig: RTX 5080 16 GB, Ryzen 9 5900X (2 CCDs, 24 threads), 4K 120 Hz HDR.
Tools: REA 6.3.0 with Ghidra 12.1.4, and a capstone byte scan. Evidence IDs are REA records.

## Applied by `Launch-Enshrouded.ps1`

| Change | Evidence | Proof that it landed |
|---|---|---|
| Texture budget for cards with 10 GiB or more: 2 GiB → 6 GiB, in memory only | `FUN_1400f82c0` picks the budget from table `DAT_1413b86d0`. The rows are VRAM thresholds 0 / 4.5 / 6 / 8 / 10 GiB, with budgets 0.29 / 0.59 / 1.0 / 1.49 / 2.0 GiB. No row is above 10 GiB. (ev_eb387ca2…) | log: `Setting texture streaming budget to 6,442,450,944 Bytes.` |
| `--enable-async-compute` | The parser at `0x14084f711` sets an optional bool. The renderer create (call at `0x14084912f`) uses it in place of the automatic choice. The automatic choice was **disabled** on this card. (ev_8b734aed…) | log: `[render] async compute is enabled` |
| Priority AboveNormal | — | launcher output |

## Checked and not needed

- **Vulkan overlay layers.** The game already sets the `DISABLE_*` variables for the Overwolf, ReShade, OBS, RTSS, MangoHud, vkBasalt, EOS and Bandicam layers. Only `--keep-vulkan-layers` stops this (strings at file offsets `0x1da8360`–`0x1da84f8`).

## VRAM facts (from the game's own log)

- The game's peak device-local allocation in a full session at max settings was **8.6 GiB** of 15.6 GiB. That peak included the 2 GiB texture pool.
- Other pools in the same function are also capped by VRAM tier, at the 8 GiB tier or below (`DAT_1413b87a0`, `DAT_1413b87d0`). One of them is the 128 MiB streaming upload buffer. These were not changed.

## Not yet tried (candidates)

- `--large-pages`. The game calls its large-page setup. It needs the "Lock pages in memory" user right, which only the author can grant (secpol). Without the right, the game logs `Couldn't enable large pages` and continues.
- `--enable-dxgi`. The behaviour is not analysed.
- Vsync. The game always uses `FIFO` present mode. At 120 Hz without VRR, a frame slower than 8.3 ms waits for the next refresh, so the frame rate drops to 60. Check whether G-SYNC is on.

## Result

- 2026-10-23: the author reports that fps is better with the launcher. The change that caused it is not isolated. To isolate it, run with `-NoAsyncCompute` and compare.
- 2026-10-23, live measurement over 20 s in-game, with the launcher, 4K, DLSS Performance, max settings:
  - PresentMon, 1363 frames: average 68.3 fps, 1% low 54.2 fps. Average frame 14.63 ms, of which the GPU was busy 14.55 ms (GPUWait 0.08 ms). Present mode `Hardware: Independent Flip` on every frame.
  - nvidia-smi, 20 samples: GPU utilisation 96 % average (91–97 %), 2856 MHz, 282 W.
  - **The game is GPU-bound now.** This replaces the earlier guess from the DLSS steps that the limit was not the GPU.
  - VRAM: the adapter used 15,334 of 16,303 MiB. The game's own log total is 11.88 GiB, compared with 8.6 GiB before the budget change. DWM's "dedicated" counter read 3,487 MiB in one sample and 1,743 MiB averaged over 10 s; this counter is noisy. Adapter total in that second read: 15,349 MiB. Headroom is about 1 GiB.
  - CPU, 10 s over 5 samples (Processor Utility): 28 % total. The busiest logical CPUs were at 63–73 %, so no core was saturated. This agrees with the GPU-bound reading. It was not confirmed that the author was in gameplay during this sample.
  - If hitches show up while travelling, use `-TextureBudgetGiB 4`.
  - The NVIDIA Overlay process reports 25,386 MiB "dedicated" but only 174 MiB committed. This matches "overlay seems busted". The figure is not real VRAM use.
