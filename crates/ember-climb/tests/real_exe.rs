//! Checks against the real game exes, on the bytes of the files. Nothing is started.
//!
//! They need the files, so they are `#[ignore]`d, and without its file a test FAILS. A check that
//! passes when it could not look is worse than no check. Paths come from `ENSHROUDED_STEAM_EXE`
//! and `ENSHROUDED_SERVER_EXE`, or the default Steam folders.
//!
//! ```text
//! cargo test -p ember-climb --test real_exe -- --ignored
//! ```

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use ember_climb::{hooks, plan, sigs};
use emberkit::build_id::{self, test_support::STEAM_EXE, Build};
use emberkit::patch::test_support::Mem;
use emberkit::scan::{find_unique, parse_pe_file, Pattern, PeInfo};
use iced_x86::{Decoder, DecoderOptions, OpKind};

/// Held by the test that uses MinHook.
static MINHOOK: Mutex<()> = Mutex::new(());
/// `MEM_RESERVE | MEM_COMMIT`.
const MEM_RESERVE_COMMIT: u32 = 0x3000;
/// Windows' page protection for code that a test may change.
const PAGE_EXECUTE_READWRITE: u32 = 0x40;

/// The default path of the server exe.
const SERVER_EXE: &str =
    r"C:\Program Files (x86)\Steam\steamapps\common\EnshroudedServer\enshrouded_server.exe";

/// One exe file, read and parsed.
struct Exe {
    /// Where it was read from.
    path: PathBuf,
    /// Its bytes.
    bytes: Vec<u8>,
    /// Its headers.
    pe: PeInfo,
}

impl Exe {
    /// The exe at `$env`, or else at `default`. Panics if there is no file: see the module docs.
    fn open(env: &str, default: &str) -> Exe {
        let path = std::env::var_os(env).map_or_else(|| PathBuf::from(default), PathBuf::from);
        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "this test needs {}: {error}. Set {env} to run it.",
                path.display()
            )
        });
        let pe = parse_pe_file(&bytes).unwrap();
        Exe { path, bytes, pe }
    }

    /// The Steam client.
    fn client() -> Exe {
        let exe = Exe::open("ENSHROUDED_STEAM_EXE", STEAM_EXE);
        exe.assert_build("f68140a3-steam");
        exe
    }

    /// The dedicated server.
    fn server() -> Exe {
        let exe = Exe::open("ENSHROUDED_SERVER_EXE", SERVER_EXE);
        exe.assert_build("d7baa768-server");
        exe
    }

    /// The file is the build this mod was written against.
    fn assert_build(&self, label: &'static str) {
        let hash = build_id::sha256_file(&self.path).unwrap();
        assert_eq!(
            build_id::classify(hash),
            Build::Known(label),
            "{}",
            self.path.display()
        );
    }

    /// The signatures in the file's `.text`, found the way the game's memory is searched.
    fn find(&self) -> Result<sigs::Found, String> {
        let (va, text) = self.pe.section_va_bytes(&self.bytes, ".text")?;
        sigs::find_all(&sigs::ALL, |sig| {
            let pattern = Pattern::parse(sig.pattern).map_err(|error| error.to_string())?;
            let at = find_unique(text, &pattern).map_err(|error| error.to_string())?;
            Ok(va as usize + at)
        })
    }

    /// `len` bytes at the virtual address `addr`.
    fn read(&self, addr: usize, len: usize) -> Option<Vec<u8>> {
        self.pe
            .bytes_at_va(&self.bytes, addr as u64, len)
            .ok()
            .map(<[u8]>::to_vec)
    }

    /// The snapshot the plan runs on.
    fn snapshot(&self) -> plan::Snapshot {
        let found = self.find().unwrap();
        plan::snapshot(&found, self.pe.image_base as usize, |addr, len| {
            self.read(addr, len)
        })
        .unwrap()
    }
}

/// Every signature matches exactly once, or the error names each one that does not.
fn every_signature_matches_once(exe: &Exe) {
    let found = exe.find().unwrap_or_else(|error| panic!("{error}"));
    for sig in sigs::ALL {
        let at = found.address(sig).unwrap();
        println!("{:<16} {at:#x}", sig.name);
    }
}

/// The plan accepts the file's bytes, with and without the glider, and applying it leaves bytes
/// the plan itself would refuse. So it cannot be applied twice.
fn the_plan_applies(exe: &Exe) {
    let snapshot = exe.snapshot();
    assert_eq!(plan::plan(&snapshot, false).unwrap().len(), 2);
    let writes = plan::plan(&snapshot, true).unwrap();
    assert_eq!(writes.len(), 3);
    for write in &writes {
        let found = exe.read(write.addr, write.old.len()).unwrap();
        assert_eq!(found, write.old, "{}", write.what);
    }
    let mut applied = snapshot;
    applied.wall_lost.bytes = plan::WALL_LOST_HOLD;
    assert!(plan::plan(&applied, false).is_err());
    let mut applied = snapshot;
    applied.attach.bytes = plan::ATTACH_HOLD;
    assert!(plan::plan(&applied, false).is_err());
}

/// The reordered attachment store is safe only if the code at its skip target loads `rcx` again,
/// because the new bytes leave a different value there. This checks that on the file.
fn the_skip_target_reloads_rcx(exe: &Exe) {
    let found = exe.find().unwrap();
    let store = found.address(&sigs::ATTACH_STORE).unwrap();
    let skip = exe.read(store + 27, 3).unwrap();
    assert_eq!(skip, [0x48, 0x8b, 0x0f], "mov rcx,[rdi]");
}

/// MinHook moves the first instructions of each hooked function into a trampoline. None of those
/// may read relative to its own address or leave the block, which is what the start-up's SAFETY
/// comment says.
fn hooked_prologues_can_be_moved(exe: &Exe) {
    let found = exe.find().unwrap();
    let everything = ember_climb::config::Settings {
        glide: true,
        stamina_multiplier: 2.0,
        ..Default::default()
    };
    let sites = hooks::wanted(&everything);
    assert_eq!(sites.len(), 11);
    for site in sites {
        let at = site
            .target(&found, |addr, len| exe.read(addr, len))
            .unwrap();
        let bytes = exe.read(at, 32).unwrap();
        let mut decoder = Decoder::with_ip(64, &bytes, at as u64, DecoderOptions::NONE);
        let mut moved = 0;
        while moved < 5 {
            let instruction = decoder.decode();
            assert!(!instruction.is_invalid(), "{}", site.sig.name);
            assert!(!instruction.is_ip_rel_memory_operand(), "{}", site.sig.name);
            let branches = (0..instruction.op_count())
                .any(|index| instruction.op_kind(index) == OpKind::NearBranch64);
            assert!(!branches, "{}", site.sig.name);
            moved += instruction.len();
        }
    }
}

/// MinHook itself accepts every site. The eight hooks of the climb experiment have run in the game,
/// but the move check, the exit query and the stamina cost never have. So the real `.text` goes
/// into private executable memory and all 11 hooks are enabled on that copy. Nothing in the copy
/// ever runs: this tests MinHook's decoder on the real bytes, not the game.
fn minhook_accepts_every_site(exe: &Exe) {
    // MinHook's queue is one per process, and the count below must be this run's alone.
    let _alone = MINHOOK.lock().unwrap_or_else(PoisonError::into_inner);
    let (text_va, text) = exe.pe.section_va_bytes(&exe.bytes, ".text").unwrap();
    let copy = Mem::new(text.len(), MEM_RESERVE_COMMIT, PAGE_EXECUTE_READWRITE);
    copy.put(0, text);
    let found = exe.find().unwrap();
    let everything = ember_climb::config::Settings {
        glide: true,
        stamina_multiplier: 2.0,
        ..Default::default()
    };
    let sites = hooks::wanted(&everything);
    let mut targets = Vec::new();
    for site in &sites {
        let at = site
            .target(&found, |addr, len| exe.read(addr, len))
            .unwrap();
        let target = copy.base + (at - text_va as usize);
        // SAFETY: `target` is inside the private copy, which nothing executes, and it stays
        // allocated until after the hooks are enabled. This test is the only user of MinHook here.
        let trampoline = unsafe { emberkit::hooks::create_and_queue(target, site.detour) }
            .unwrap_or_else(|why| panic!("{}: {why}", site.sig.name));
        assert_ne!(trampoline, 0, "{}", site.sig.name);
        targets.push((site.sig.name, at - text_va as usize));
    }
    assert_eq!(emberkit::hooks::apply_queued(), Ok(11));
    for (name, offset) in targets {
        assert_eq!(copy.get(offset, 1), [0xe9], "{name} has no jump");
    }
}

/// The stamina hook goes on the function the design names. Its signature finds a `call`, and the
/// function behind it has twins, so this pins the address on the file.
#[test]
#[ignore = "needs the Steam client exe"]
fn the_stamina_call_leads_to_the_cost_function_in_the_client() {
    let exe = Exe::client();
    let found = exe.find().unwrap();
    let settings = ember_climb::config::Settings {
        stamina_multiplier: 2.0,
        ..Default::default()
    };
    let sites = hooks::wanted(&settings);
    let site = sites
        .iter()
        .find(|site| site.sig.name == "STAMINA_CALL")
        .unwrap();
    let target = site
        .target(&found, |addr, len| exe.read(addr, len))
        .unwrap();
    assert_eq!(target, 0x1_4024_a360);
}

/// The Steam client: MinHook on a private copy.
#[test]
#[ignore = "needs the Steam client exe"]
fn minhook_accepts_every_site_in_the_client() {
    minhook_accepts_every_site(&Exe::client());
}

/// The server: MinHook on a private copy.
#[test]
#[ignore = "needs the dedicated server exe"]
fn minhook_accepts_every_site_in_the_server() {
    minhook_accepts_every_site(&Exe::server());
}

/// The Steam client: every signature.
#[test]
#[ignore = "needs the Steam client exe"]
fn every_signature_matches_once_in_the_client() {
    every_signature_matches_once(&Exe::client());
}

/// The Steam client: the plan.
#[test]
#[ignore = "needs the Steam client exe"]
fn the_plan_applies_to_the_client() {
    the_plan_applies(&Exe::client());
}

/// The Steam client: the reordered store.
#[test]
#[ignore = "needs the Steam client exe"]
fn the_skip_target_reloads_rcx_in_the_client() {
    the_skip_target_reloads_rcx(&Exe::client());
}

/// The Steam client: the hook sites.
#[test]
#[ignore = "needs the Steam client exe"]
fn hooked_prologues_can_be_moved_in_the_client() {
    hooked_prologues_can_be_moved(&Exe::client());
}

/// The server: every signature.
#[test]
#[ignore = "needs the dedicated server exe"]
fn every_signature_matches_once_in_the_server() {
    every_signature_matches_once(&Exe::server());
}

/// The server: the plan.
#[test]
#[ignore = "needs the dedicated server exe"]
fn the_plan_applies_to_the_server() {
    the_plan_applies(&Exe::server());
}

/// The server: the reordered store.
#[test]
#[ignore = "needs the dedicated server exe"]
fn the_skip_target_reloads_rcx_in_the_server() {
    the_skip_target_reloads_rcx(&Exe::server());
}

/// The server: the hook sites.
#[test]
#[ignore = "needs the dedicated server exe"]
fn hooked_prologues_can_be_moved_in_the_server() {
    hooked_prologues_can_be_moved(&Exe::server());
}
