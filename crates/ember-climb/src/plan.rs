//! The bytes this mod writes into the game, and the check that each site holds what it should.
//!
//! Almost everything here is a hook, which leaves the game's own climb bytes alone. Three sites are
//! written, because no hook can reach them:
//! - the wall-lost return, so a voxel surface (hit id 0) does not read as "wall lost";
//! - the attachment store, so that return's stand-in value never enters the game's data;
//! - with `Glide = 1` only, the layer of the request to start climbing.
//!
//! The plan is pure. It runs the same on the bytes of a running game and on the bytes of an exe
//! file on disk, so the real-exe tests prove the plan the game will run.

use emberkit::patch::{rel32_target, PatchError, Patched};

use crate::sigs::{Found, Sig, ATTACH_STORE, ENTER_LAYER, WALL_LOST_RET};

/// The length of a `call rel32`.
const CALL_SIZE: usize = 5;
/// Where the return is in a [`WALL_LOST_RET`] match.
const WALL_LOST_AT: usize = 26;
/// The game's return: `mov eax,[rsp+0x70]` (the probe's hit id), `add rsp,0x88`, `ret`.
pub const WALL_LOST_GAME: [u8; 12] = [
    0x8b, 0x44, 0x24, 0x70, 0x48, 0x81, 0xc4, 0x88, 0x00, 0x00, 0x00, 0xc3,
];
/// The replacement: `mov eax,[rsp+0x70]`, `cmp eax,1`, `sbb eax,0`, then a jump back to the
/// epilogue at match +18, which the signature proves. Id 0 turns into 0xffffffff, and every other
/// id stays. It stays inside the function: the 12 bytes end at the original `ret`.
pub const WALL_LOST_HOLD: [u8; 12] = [
    0x8b, 0x44, 0x24, 0x70, 0x83, 0xf8, 0x01, 0x83, 0xd8, 0x00, 0xeb, 0xec,
];
/// What [`WALL_LOST_HOLD`] returns for a voxel surface.
pub const HELD: u32 = 0xffff_ffff;

/// The game's attachment store, up to its `call`: `mov rax,[rdi+0xa0]`, `cmp [rax+0x18],ebx`,
/// `je skip`, `lea rcx,[rdi+0xa0]`. The `call` and the `mov [rax+0x18],ebx` after it stay.
pub const ATTACH_GAME: [u8; 19] = [
    0x48, 0x8b, 0x87, 0xa0, 0x00, 0x00, 0x00, 0x39, 0x58, 0x18, 0x74, 0x0f, 0x48, 0x8d, 0x8f, 0xa0,
    0x00, 0x00, 0x00,
];
/// The replacement rejects only [`HELD`], then prepares the original accessor call.
///
/// Real entity IDs may have their top bit set. `cmp ebx,-1; je skip` compares the exact synthetic
/// sentinel, instead of filtering every signed-negative ID. The native helper and following
/// store stay in place. Valid IDs also take that helper when unchanged; it only marks the
/// already-writable accessor and returns its pointer, so no ID-domain assumption is needed.
/// Seven NOPs preserve the original call's address and all following instruction boundaries.
pub const ATTACH_HOLD: [u8; 19] = [
    0x83, 0xfb, 0xff, 0x74, 0x16, 0x48, 0x8d, 0x8f, 0xa0, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90, 0x90,
    0x90, 0x90, 0x90,
];

/// Where the layer byte is in an [`ENTER_LAYER`] match.
const LAYER_AT: usize = 18;
/// The game's layer for the request to start climbing. It reads CanClimb a second time.
const LAYER_GAME: u8 = 0x13;
/// The layer that does not. The first read still gates the climb.
const LAYER_GLIDE: u8 = 0x10;

/// A place in the game's code and the bytes found there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site<const N: usize> {
    /// The address of the first byte.
    pub addr: usize,
    /// The bytes there when the snapshot was taken.
    pub bytes: [u8; N],
}

/// The game's bytes the plan needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The exe's base, for the log.
    pub base: usize,
    /// The wall-lost return.
    pub wall_lost: Site<12>,
    /// The attachment store.
    pub attach: Site<19>,
    /// The layer byte.
    pub layer: Site<1>,
}

/// One checked write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    /// What it changes, for the log.
    pub what: &'static str,
    /// Where it goes.
    pub addr: usize,
    /// The bytes that must be there.
    pub old: Vec<u8>,
    /// The bytes it puts there.
    pub new: Vec<u8>,
}

/// `N` bytes at `offset` past the match of `sig`, read with `read`.
fn site<const N: usize>(
    found: &Found,
    sig: &Sig,
    offset: usize,
    read: &impl Fn(usize, usize) -> Option<Vec<u8>>,
) -> Result<Site<N>, String> {
    let addr = found
        .address(sig)?
        .checked_add(offset)
        .ok_or_else(|| format!("{}: the address overflows", sig.name))?;
    let bytes = read(addr, N)
        .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
        .ok_or_else(|| format!("{}: cannot read {N} bytes at {addr:#x}", sig.name))?;
    Ok(Site { addr, bytes })
}

/// The bytes of every site. `read` gives `len` bytes at an address, or `None`.
pub fn snapshot(
    found: &Found,
    base: usize,
    read: impl Fn(usize, usize) -> Option<Vec<u8>>,
) -> Result<Snapshot, String> {
    Ok(Snapshot {
        base,
        wall_lost: site(found, &WALL_LOST_RET, WALL_LOST_AT, &read)?,
        attach: site(found, &ATTACH_STORE, 0, &read)?,
        layer: site(found, &ENTER_LAYER, LAYER_AT, &read)?,
    })
}

/// The function the `call` at `at` leads to, if it starts with `prologue`. `read` gives `len`
/// bytes at an address, or `None`.
pub fn follow_call(
    at: usize,
    prologue: &[u8],
    read: impl Fn(usize, usize) -> Option<Vec<u8>>,
) -> Result<usize, String> {
    let bytes = read(at, CALL_SIZE).ok_or_else(|| format!("cannot read the call at {at:#x}"))?;
    let [0xe8, a, b, c, d] = *bytes.as_slice() else {
        return Err(format!("no call at {at:#x}: read {bytes:02x?}"));
    };
    let target = at
        .checked_add(CALL_SIZE)
        .and_then(|next| rel32_target(next, i32::from_le_bytes([a, b, c, d])))
        .ok_or_else(|| format!("the call at {at:#x} leads nowhere"))?;
    let found = read(target, prologue.len())
        .ok_or_else(|| format!("cannot read the function at {target:#x}"))?;
    if found != prologue {
        return Err(format!(
            "the call at {at:#x} leads to {target:#x}, which starts with {found:02x?}"
        ));
    }
    Ok(target)
}

/// `addr` as `exe+offset` when it is inside the exe at `base`, else as an address.
pub fn in_exe(base: usize, addr: usize) -> String {
    match addr.checked_sub(base) {
        Some(offset) if base != 0 => format!("exe+{offset:#x}"),
        _ => format!("{addr:#x}"),
    }
}

/// The writes for this snapshot, or why not. `glide` adds the layer write.
///
/// Every site is checked against the game's bytes, whether or not it is written, so nothing is
/// written when any one of them is off. A site that holds other bytes was changed by another tool
/// or by a build this mod does not know.
pub fn plan(snapshot: &Snapshot, glide: bool) -> Result<Vec<Write>, String> {
    let refuse = |what: &str, addr: usize, found: &[u8]| {
        Err(format!(
            "{what} at {} is not the game's: read {found:02x?}, so nothing was changed",
            in_exe(snapshot.base, addr)
        ))
    };
    if snapshot.wall_lost.bytes != WALL_LOST_GAME {
        return refuse(
            "the wall-lost return",
            snapshot.wall_lost.addr,
            &snapshot.wall_lost.bytes,
        );
    }
    if snapshot.attach.bytes != ATTACH_GAME {
        return refuse(
            "the attachment store",
            snapshot.attach.addr,
            &snapshot.attach.bytes,
        );
    }
    if snapshot.layer.bytes != [LAYER_GAME] {
        return refuse(
            "the enter layer",
            snapshot.layer.addr,
            &snapshot.layer.bytes,
        );
    }
    let mut writes = vec![
        Write {
            what: "the wall-lost return",
            addr: snapshot.wall_lost.addr,
            old: WALL_LOST_GAME.to_vec(),
            new: WALL_LOST_HOLD.to_vec(),
        },
        Write {
            what: "the attachment store",
            addr: snapshot.attach.addr,
            old: ATTACH_GAME.to_vec(),
            new: ATTACH_HOLD.to_vec(),
        },
    ];
    if glide {
        writes.push(Write {
            what: "the enter layer",
            addr: snapshot.layer.addr,
            old: vec![LAYER_GAME],
            new: vec![LAYER_GLIDE],
        });
    }
    Ok(writes)
}

/// Make `writes` in order with `write(addr, old, new)`. If one fails, the writes before it are put
/// back, last first. `Err` names the write that failed and says what was put back.
pub fn apply_with(
    writes: &[Write],
    mut write: impl FnMut(usize, &[u8], &[u8]) -> Result<Patched, PatchError>,
) -> Result<(), String> {
    for (done, failed) in writes.iter().enumerate() {
        let Err(error) = write(failed.addr, &failed.old, &failed.new) else {
            continue;
        };
        let back = undo_with(writes.get(..done).unwrap_or_default(), &mut write);
        return Err(format!(
            "{} at {:#x} was not written: {error}; {back}",
            failed.what, failed.addr
        ));
    }
    Ok(())
}

/// Put `writes` back, last first, and say how that went.
pub fn undo_with(
    writes: &[Write],
    mut write: impl FnMut(usize, &[u8], &[u8]) -> Result<Patched, PatchError>,
) -> String {
    let mut stuck = Vec::new();
    for undo in writes.iter().rev() {
        if let Err(error) = write(undo.addr, &undo.new, &undo.old) {
            stuck.push(format!("{} at {:#x} ({error})", undo.what, undo.addr));
        }
    }
    if stuck.is_empty() {
        format!("the {} writes before it were put back", writes.len())
    } else {
        format!("could not put back: {}", stuck.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sigs::{self, Sig};
    use iced_x86::{Code, Decoder, DecoderOptions, Instruction};

    /// A snapshot with the game's own bytes at made-up addresses.
    fn game() -> Snapshot {
        Snapshot {
            base: 0x1_4000_0000,
            wall_lost: Site {
                addr: 0x1_4024_b4a5,
                bytes: WALL_LOST_GAME,
            },
            attach: Site {
                addr: 0x1_4041_8db8,
                bytes: ATTACH_GAME,
            },
            layer: Site {
                addr: 0x1_4041_9947,
                bytes: [LAYER_GAME],
            },
        }
    }

    /// Decode `bytes` as they sit at `ip`.
    fn decode(bytes: &[u8], ip: u64) -> Vec<Instruction> {
        Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE)
            .iter()
            .collect()
    }

    /// Without the glider, exactly the two hold sites are written, with the planned bytes.
    #[test]
    fn the_plan_writes_the_two_hold_sites() {
        let writes = plan(&game(), false).unwrap();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].addr, 0x1_4024_b4a5);
        assert_eq!(writes[0].old, WALL_LOST_GAME);
        assert_eq!(writes[0].new, WALL_LOST_HOLD);
        assert_eq!(writes[1].addr, 0x1_4041_8db8);
        assert_eq!(writes[1].old, ATTACH_GAME);
        assert_eq!(writes[1].new, ATTACH_HOLD);
    }

    /// The layer changes only when the player asked for the glider.
    #[test]
    fn the_layer_is_written_only_for_the_glider() {
        let writes = plan(&game(), true).unwrap();
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[2].addr, 0x1_4041_9947);
        assert_eq!(writes[2].old, [0x13]);
        assert_eq!(writes[2].new, [0x10]);
    }

    /// Bytes that are not the game's stop everything, whichever site it is.
    #[test]
    fn a_changed_site_refuses_the_whole_plan() {
        let mut changed = game();
        changed.wall_lost.bytes = WALL_LOST_HOLD;
        assert!(plan(&changed, false)
            .unwrap_err()
            .contains("wall-lost return"));
        let mut changed = game();
        changed.attach.bytes[8] = 0x48;
        assert!(plan(&changed, false)
            .unwrap_err()
            .contains("attachment store"));
    }

    /// The layer site is checked even when it is not written: it is one more proof of the build.
    #[test]
    fn the_layer_site_is_checked_even_without_the_glider() {
        let mut changed = game();
        changed.layer.bytes = [0x12];
        let error = plan(&changed, false).unwrap_err();
        assert!(error.contains("enter layer"), "{error}");
        assert!(error.contains("exe+0x419947"), "{error}");
        assert!(error.contains("nothing was changed"), "{error}");
    }

    /// The return patch does what its comment says, instruction by instruction.
    #[test]
    fn the_hold_return_decodes_as_documented() {
        let ip = 0x1_4024_b4a5;
        let code: Vec<Code> = decode(&WALL_LOST_HOLD, ip)
            .iter()
            .map(Instruction::code)
            .collect();
        assert_eq!(
            code,
            [
                Code::Mov_r32_rm32,
                Code::Cmp_rm32_imm8,
                Code::Sbb_rm32_imm8,
                Code::Jmp_rel8_64
            ]
        );
    }

    /// The jump lands on the epilogue the signature proves: match +18, which is 8 bytes before
    /// the site.
    #[test]
    fn the_hold_return_jumps_to_the_proven_epilogue() {
        let ip = 0x1_4024_b4a5;
        let instructions = decode(&WALL_LOST_HOLD, ip);
        let jump = instructions.last().unwrap();
        assert_eq!(jump.near_branch_target(), ip - 8);
        let pattern = sigs::WALL_LOST_RET.pattern.replace(' ', "");
        let epilogue = &pattern[18 * 2..26 * 2];
        assert_eq!(epilogue, "4881c488000000c3");
    }

    /// The hold return gives 0xffffffff for id 0 and the id for anything else, exactly.
    #[test]
    fn the_hold_return_maps_ids_as_documented() {
        let run = |id: u32| {
            let (value, borrow) = id.overflowing_sub(1);
            let _ = value;
            id.wrapping_sub(u32::from(borrow))
        };
        assert_eq!(run(0), HELD);
        assert_eq!(run(1), 1);
        assert_eq!(run(0x1234), 0x1234);
        assert_eq!(run(0x7fff_ffff), 0x7fff_ffff);
    }

    /// Execute only our replacement bytes in a private inert harness, never game code.
    /// Valid IDs span all 32 bits; only the exact synthetic sentinel may skip the native store.
    #[test]
    fn the_attachment_filter_preserves_full_width_entity_ids() {
        use emberkit::patch::test_support::Mem;
        use std::sync::atomic::{AtomicU32, Ordering};
        static WRITES: AtomicU32 = AtomicU32::new(0);
        /// Inert stand-in for the native accessor's writable-pointer helper.
        ///
        /// # Safety
        ///
        /// `slot` points to an initialized data pointer for this fixture.
        unsafe extern "system" fn writable(slot: *const *mut u8) -> *mut u8 {
            emberkit::guard::guard(crate::Feature::Startup, std::ptr::null_mut, || {
                WRITES.fetch_add(1, Ordering::Relaxed);
                // SAFETY: the fixture supplies one initialized pointer at this argument.
                unsafe { slot.read_unaligned() }
            })
        }
        // Save the two nonvolatile registers used at the real patch site, and reserve shadow
        // space with the Windows x64 stack alignment before calling the inert helper.
        let mut code = vec![
            0x53, 0x57, 0x48, 0x83, 0xec, 0x28, 0x48, 0x89, 0xcf, 0x89, 0xd3,
        ];
        code.extend_from_slice(&ATTACH_HOLD);
        let call = code.len();
        code.extend_from_slice(&[0xe8, 0, 0, 0, 0, 0x89, 0x58, 0x18]);
        code.extend_from_slice(&[
            0x48, 0x8b, 0x0f, // original common block: reload RCX
            0x48, 0x8b, 0x87, 0xa0, 0, 0, 0, // load fixture component
            0x8b, 0x40, 0x18, // return its attachment ID
            0x48, 0x83, 0xc4, 0x28, 0x5f, 0x5b, 0xc3,
        ]);
        let thunk = code.len();
        code.extend_from_slice(&[0x48, 0xb8]);
        code.extend_from_slice(&(writable as *const () as u64).to_le_bytes());
        code.extend_from_slice(&[0xff, 0xe0]);
        let delta = i32::try_from(thunk - (call + 5)).unwrap();
        code[call + 1..call + 5].copy_from_slice(&delta.to_le_bytes());
        let memory = Mem::new(4096, 0x3000, 0x40);
        // SAFETY: fresh private executable pages owned by this test, with an all-zero initial
        // range. The checked write also flushes the instruction cache before any execution.
        unsafe { emberkit::patch::write_checked(memory.base, &vec![0; code.len()], &code) }
            .unwrap();
        type Harness = unsafe extern "system" fn(*mut u8, u32) -> u32;
        // SAFETY: the reviewed inert wrapper above implements this exact ABI, preserves its
        // nonvolatile registers and stack, and its allocation lasts through every call.
        let run = unsafe { std::mem::transmute::<usize, Harness>(memory.base) };
        for id in [0x8000_0000, 0x8000_0001, 0xffff_fffe, 1, 0x7fff_fffe, HELD] {
            for old in [0x1234, id] {
                let mut component = [0u8; 32];
                component[0x18..0x1c].copy_from_slice(&old.to_le_bytes());
                let mut context = [0usize; 22];
                context[20] = component.as_mut_ptr() as usize;
                WRITES.store(0, Ordering::Relaxed);
                // SAFETY: the context's initialized accessor at +0xa0 points to a complete
                // writable fixture component, and only this synchronous call uses it.
                let stored = unsafe { run(context.as_mut_ptr().cast(), id) };
                let expected = if id == HELD { old } else { id };
                assert_eq!(stored, expected, "id {id:#x}, old {old:#x}");
                assert_eq!(WRITES.load(Ordering::Relaxed), u32::from(id != HELD));
            }
        }
    }

    /// The exact sentinel branch skips the preserved call/store, with every boundary unchanged.
    #[test]
    fn the_attachment_store_decodes_as_documented() {
        let ip = 0x1_4041_8db8u64;
        let skip = ip + 27;
        let new = decode(&ATTACH_HOLD, ip);
        let code: Vec<Code> = new.iter().map(Instruction::code).collect();
        assert_eq!(
            code[..3],
            [Code::Cmp_rm32_imm8, Code::Je_rel8_64, Code::Lea_r64_m]
        );
        assert_eq!(new[0].immediate8to32(), -1);
        assert_eq!(new[1].near_branch_target(), skip);
        assert_eq!(new[2].memory_displacement64(), 0xa0);
        assert!(code[3..].iter().all(|code| *code == Code::Nopd));
        let old = decode(&ATTACH_GAME, ip);
        assert_eq!(old[2].near_branch_target(), skip);
    }

    /// The patch must end on the `call`, so the call and everything after it stay as they are.
    #[test]
    fn the_attachment_store_ends_where_the_call_starts() {
        assert_eq!(ATTACH_GAME.len(), ATTACH_HOLD.len());
        let pattern = Pattern::of(&sigs::ATTACH_STORE);
        assert_eq!(pattern.get(19), Some(&0xe8));
        assert_eq!(&pattern[..19], ATTACH_GAME);
    }

    /// The signatures themselves contain the bytes the plan expects.
    #[test]
    fn the_signatures_hold_the_planned_bytes() {
        let layer = Pattern::of(&sigs::ENTER_LAYER);
        assert_eq!(layer.get(LAYER_AT), Some(&LAYER_GAME));
        let lost = Pattern::of(&sigs::WALL_LOST_RET);
        assert_eq!(&lost[WALL_LOST_AT..], WALL_LOST_GAME);
    }

    /// The bytes of a pattern with no wildcards in the part a test looks at.
    struct Pattern;

    impl Pattern {
        /// `sig`'s bytes, with 0 for each wildcard.
        fn of(sig: &Sig) -> Vec<u8> {
            sig.pattern
                .split_whitespace()
                .map(|token| u8::from_str_radix(token, 16).unwrap_or(0))
                .collect()
        }
    }

    /// `snapshot` reads each site at its signature's offset.
    #[test]
    fn the_snapshot_reads_each_site_at_its_offset() {
        let found = sigs::find_all(&sigs::ALL, |sig| match sig.name {
            "WALL_LOST_RET" => Ok(1000),
            "ATTACH_STORE" => Ok(2000),
            "ENTER_LAYER" => Ok(3000),
            _ => Ok(0),
        })
        .unwrap();
        let snap = snapshot(&found, 0, |addr, len| Some(vec![addr as u8; len])).unwrap();
        assert_eq!(snap.wall_lost.addr, 1026);
        assert_eq!(snap.attach.addr, 2000);
        assert_eq!(snap.layer.addr, 3018);
    }

    /// A site that cannot be read refuses the plan and names it.
    #[test]
    fn an_unreadable_site_is_named() {
        let found = sigs::find_all(&sigs::ALL, |_| Ok(0x1000)).unwrap();
        let error = snapshot(&found, 0, |_, _| None).unwrap_err();
        assert!(error.contains("WALL_LOST_RET"), "{error}");
    }

    /// A read that comes back short is refused, not padded.
    #[test]
    fn a_short_read_is_refused() {
        let found = sigs::find_all(&sigs::ALL, |_| Ok(0x1000)).unwrap();
        assert!(snapshot(&found, 0, |_, _| Some(vec![0; 3])).is_err());
    }

    /// Writes go in order, each with its old and new bytes.
    #[test]
    fn writes_go_in_order() {
        let writes = plan(&game(), true).unwrap();
        let mut seen = Vec::new();
        apply_with(&writes, |addr, old, new| {
            seen.push((addr, old.to_vec(), new.to_vec()));
            Ok(Patched {
                addr,
                original: old.to_vec(),
            })
        })
        .unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[1].1, ATTACH_GAME);
        assert_eq!(seen[1].2, ATTACH_HOLD);
    }

    /// If one write fails, the earlier ones go back, last first, so the game is left whole.
    #[test]
    fn a_failed_write_puts_back_what_went_before() {
        let writes = plan(&game(), true).unwrap();
        let mut log = Vec::new();
        let error = apply_with(&writes, |addr, old, new| {
            log.push((addr, new.to_vec()));
            if addr == 0x1_4041_9947 {
                Err(PatchError::ReadBack)
            } else {
                Ok(Patched {
                    addr,
                    original: old.to_vec(),
                })
            }
        })
        .unwrap_err();
        assert_eq!(log.len(), 5, "{log:02x?}");
        assert_eq!(log[3], (0x1_4041_8db8, ATTACH_GAME.to_vec()));
        assert_eq!(log[4], (0x1_4024_b4a5, WALL_LOST_GAME.to_vec()));
        assert!(error.contains("the enter layer"), "{error}");
        assert!(
            error.contains("the 2 writes before it were put back"),
            "{error}"
        );
    }

    /// A write that cannot go back is named: the log must not claim a clean game.
    #[test]
    fn a_write_that_cannot_go_back_is_named() {
        let writes = plan(&game(), false).unwrap();
        let back = undo_with(&writes, |_, _, _| Err(PatchError::ReadBack));
        assert!(back.contains("could not put back"), "{back}");
        assert!(back.contains("the attachment store"), "{back}");
        assert!(back.contains("the wall-lost return"), "{back}");
    }

    /// A fake memory: a `call` at 0x1000 that leads to 0x2000, which holds `head`.
    fn memory(head: &'static [u8]) -> impl Fn(usize, usize) -> Option<Vec<u8>> {
        move |addr, len| {
            let bytes: &[u8] = match addr {
                // rel32 0x0ffb from the end of the call, 0x1005.
                0x1000 => &[0xe8, 0xfb, 0x0f, 0x00, 0x00],
                0x2000 => head,
                _ => return None,
            };
            bytes.get(..len).map(<[u8]>::to_vec)
        }
    }

    /// The call is followed to its function.
    #[test]
    fn a_call_is_followed() {
        let read = memory(&[1, 2, 3]);
        assert_eq!(follow_call(0x1000, &[1, 2, 3], read), Ok(0x2000));
    }

    /// A target that starts with other bytes is the wrong function: nothing may hook it.
    #[test]
    fn a_call_to_another_function_is_refused() {
        let error = follow_call(0x1000, &[1, 2, 3], memory(&[9, 9, 9])).unwrap_err();
        assert!(error.contains("starts with"), "{error}");
    }

    /// Anything but a `call` opcode is refused, and so is memory that cannot be read.
    #[test]
    fn a_site_without_a_call_is_refused() {
        let read = |_: usize, len: usize| Some(vec![0x90; len]);
        assert!(follow_call(0x1000, &[1], read)
            .unwrap_err()
            .contains("no call"));
        assert!(follow_call(0x3000, &[1], memory(&[1])).is_err());
        let unreadable_target = |addr: usize, len: usize| {
            (addr == 0x1000).then(|| vec![0xe8, 0xfb, 0x0f, 0, 0][..len].to_vec())
        };
        assert!(follow_call(0x1000, &[1], unreadable_target).is_err());
    }

    /// `in_exe` prints a stable offset, and an address outside the exe as it is.
    #[test]
    fn addresses_print_relative_to_the_exe() {
        assert_eq!(in_exe(0x1_4000_0000, 0x1_4000_1000), "exe+0x1000");
        assert_eq!(in_exe(0x1_4000_0000, 0x1000), "0x1000");
        assert_eq!(in_exe(0, 0x1000), "0x1000");
    }
}
