//! Opt-in bootstrap and worker; no file work or formatting occurs in the native callback.
use crate::{
    native::NativeMemory,
    reader::{self, Memory, Missing, RawPoint},
    Feature,
};
use emberkit::{build_id, guard, hooks, logging};
use std::{
    ffi::c_void,
    io::Read,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};
use windows_sys::Win32::System::{
    LibraryLoader::{
        GetModuleHandleExW, GetModuleHandleW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_PIN,
    },
    Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
    SystemInformation::GetTickCount64,
};
/// The only whole-file identity admitted by the static receipts.
const CLIENT_SHA: &str = "d64966e23b1a458ee6f147bcbceac0b577fa7e39a212f371287d8cc119817882";
/// Pinned source SimStep first24 bytes on this exact file.
const STEP_PREFIX: [u8; 24] = [
    0x40, 0x55, 0x53, 0x56, 0x41, 0x55, 0x48, 0x8d, 0xac, 0x24, 0xf8, 0xff, 0xfd, 0xff, 0xb8, 0x08,
    0x01, 0x02, 0x00, 0xe8, 0x88, 0xd7, 0x90, 0x00,
];
/// Process-lifetime one-shot initialization. A failed attempt is not retried blindly.
static STARTED: AtomicBool = AtomicBool::new(false);
/// Observation may be disabled independently of the mandatory native original call.
static ARMED: AtomicBool = AtomicBool::new(false);
/// Startup-checked native component function. Never changed after hooks activate.
static GETTER: AtomicUsize = AtomicUsize::new(0);
/// Earliest permitted client sample; shared across client callbacks.
static NEXT_SAMPLE: AtomicU64 = AtomicU64::new(0);
/// Startup-checked counter frequency for a two-millisecond sampling fuse.
static FREQUENCY: AtomicI64 = AtomicI64::new(0);
/// One owned sample is enough for calibration; contention drops a publication.
static LATEST: Mutex<Option<Diagnostic>> = Mutex::new(None);
/// Raw data only: this type cannot construct an ember-measurement WorldEpoch or Point.
#[derive(Clone, Copy)]
struct Diagnostic {
    /// Uptime of the copied diagnostic, not a world generation.
    sampled_ms: u64,
    /// Measured callback-copy cost; absent means timing was refused.
    sample_us: Option<u64>,
    /// Only owned raw data or an explicit unavailable reason.
    point: Result<RawPoint, Missing>,
}
unsafe extern "C" {
    /// Own detour function; no game code is called by this accessor.
    fn ruler_hook_address() -> usize;
    /// Publish a nonzero trampoline before activating the detour.
    fn ruler_set_original(original: usize) -> u8;
}
/// Is this exactly the retained client, never the server or a renamed test host?
fn identity_ok(path: &Path, sha: &str) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("enshrouded.exe"))
        && sha == CLIENT_SHA
}
/// Parse at most4KiB of this probe's own startup-only opt-in file.
fn enabled(path: &Path) -> Result<bool, String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    let mut text = String::new();
    file.take(4097)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    parse_enabled(&text)
}
/// A malformed, oversized or absent opt-in never enables the probe.
fn parse_enabled(text: &str) -> Result<bool, String> {
    if text.len() > 4096 {
        return Err("probe config exceeds 4096 bytes".to_owned());
    }
    let mut ini = configparser::ini::Ini::new();
    ini.read(text.to_owned())?;
    Ok(ini.getboolcoerce("Probe", "Enabled")?.unwrap_or(false))
}
/// Start only in the exact client and after an explicit opt-in; all fallible work is guarded.
pub(crate) fn start() -> Result<(), String> {
    if STARTED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let folder = exe.parent().ok_or("no executable directory")?;
    let _log_dir = logging::init_named(
        "enshrouded-ruler-probe.log",
        "enshrouded-ruler-probe",
        folder,
        log::LevelFilter::Info,
    );
    log::info!("Ruler calibration probe; no measurements, world epoch, menu or graphics changes");
    if !enabled(&folder.join("enshrouded-ruler-probe.ini"))? {
        log::info!("inactive: Probe.Enabled is off or absent");
        return Ok(());
    }
    let sha = build_id::to_hex(&build_id::sha256_file(&exe).map_err(|e| e.to_string())?);
    if !identity_ok(&exe, &sha) {
        log::warn!("inactive: executable identity is not the exact permitted client");
        return Ok(());
    }
    // SAFETY: null asks for the current executable module; no new module is loaded.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    if base == 0 {
        return Err("main module unavailable".to_owned());
    }
    let getter = base.checked_add(0x9a7570).ok_or("getter overflow")?;
    let target = base.checked_add(0xa1fe10).ok_or("step overflow")?;
    let memory = NativeMemory { getter };
    let mut prefix = [0u8; 24];
    if !memory.read(target, &mut prefix)
        || prefix != STEP_PREFIX
        || !memory.read(getter, &mut prefix)
        || prefix != crate::native::GET_PREFIX
    {
        return Err("native entry differs from retained bytes; no hook enabled".to_owned());
    }
    let mut frequency = 0i64;
    // SAFETY: frequency is an owned writable counter word.
    if unsafe { QueryPerformanceFrequency(&mut frequency) } == 0 || frequency <= 0 {
        return Err("performance counter unavailable; no hook enabled".to_owned());
    }
    FREQUENCY.store(frequency, Ordering::Release);
    let mut own = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS interprets our own function's address without dereferencing it.
    // PIN retains this DLL, its C++ detour and worker state until process termination.
    if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            crate::InitializeASI as *const () as *const u16,
            &mut own,
        )
    } == 0
    {
        return Err("could not pin the probe module".to_owned());
    }
    // SAFETY: own C++ accessor only returns the live, pinned detour with the Step ABI.
    let detour = unsafe { ruler_hook_address() };
    // SAFETY: exact whole-file hash and fresh entry bytes identify the source's Step ABI.
    // The C++ detour preserves that ABI and calls the original outside any Rust frame.
    let original = unsafe { hooks::create_and_queue(target, detour) }?;
    // SAFETY: this nonzero executable trampoline has the exact Step ABI and the module is pinned.
    if unsafe { ruler_set_original(original) } != 1 {
        return Err("trampoline publication refused".to_owned());
    }
    GETTER.store(getter, Ordering::Release);
    hooks::apply_queued()?;
    std::thread::Builder::new()
        .name("ruler-calibration-log".to_owned())
        .spawn(worker)
        .map_err(|e| e.to_string())?;
    ARMED.store(true, Ordering::Release);
    log::info!("raw client sampling enabled at most every 500 ms for 600 s; world_tag is NOT an epoch; coordinates are UNCALIBRATED");
    Ok(())
}
/// Copy one callback's data after the native original has returned normally.
/// # Safety
/// storage belongs to the exact admitted SimStep callback on its owning thread.
pub(crate) unsafe fn observe(storage: *mut c_void) {
    if !ARMED.load(Ordering::Acquire) {
        return;
    }
    let memory = NativeMemory {
        getter: GETTER.load(Ordering::Acquire),
    };
    let Some(world) = (storage as usize).checked_sub(0xe15dc0) else {
        return;
    };
    let Some(kind_address) = (storage as usize).checked_add(8) else {
        return;
    };
    let mut kind = [0u8; 1];
    if !memory.read(kind_address, &mut kind) || kind != [2] {
        return;
    }
    // SAFETY: uptime query has no pointer arguments.
    let now = unsafe { GetTickCount64() };
    let next = NEXT_SAMPLE.load(Ordering::Relaxed);
    if now < next
        || NEXT_SAMPLE
            .compare_exchange(
                next,
                now.saturating_add(500),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_err()
    {
        return;
    }
    let (mut point, cost) = if let Some(begin) = counter() {
        // SAFETY: native Step original returned on this callback's owner thread. The pinned
        // source API defines access here; no native borrow or callback lease is retained.
        let point = unsafe { reader::sample(&memory, world) };
        let cost =
            counter().and_then(|end| sample_us(begin, end, FREQUENCY.load(Ordering::Acquire)));
        (point, cost)
    } else {
        (Err(Missing::Budget), None)
    };
    if cost.is_none_or(|us| us > 2000) {
        ARMED.store(false, Ordering::Release);
        point = Err(Missing::Budget);
    }
    if let Ok(mut slot) = LATEST.try_lock() {
        *slot = Some(Diagnostic {
            sampled_ms: now,
            sample_us: cost,
            point,
        });
    }
}
/// Read an owned counter word without allocating or waiting for initialization.
fn counter() -> Option<i64> {
    let mut value = 0i64;
    // SAFETY: value is an owned, live writable i64.
    (unsafe { QueryPerformanceCounter(&mut value) } != 0).then_some(value)
}
/// Ordinary worker alone formats and writes the copied observations.
fn worker() {
    // SAFETY: uptime query has no pointer arguments.
    let begin = unsafe { GetTickCount64() };
    let mut last = None;
    let mut stale = false;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        // SAFETY: uptime query has no pointer arguments.
        let now = unsafe { GetTickCount64() };
        if now.saturating_sub(begin) >= 600000 {
            ARMED.store(false, Ordering::Release);
            log::info!("calibration capture ended; native original remains unchanged");
            return;
        }
        guard::guard(
            Feature::Worker,
            || (),
            || {
                let current = LATEST.try_lock().ok().and_then(|s| *s);
                if let Some(sample) = current.filter(|s| last != Some(s.sampled_ms)) {
                    last = Some(sample.sampled_ms);
                    stale = false;
                    match sample.point {
                        Ok(RawPoint { world_tag, player, axes:[x,y,z] }) => log::info!(
                        "raw_uncalibrated t={} sample_us={:?} world_tag={:#x} player={} x={} y={} z={}",
                        sample.sampled_ms,sample.sample_us,world_tag,player,x,y,z
                    ),
                    Err(why) => log::info!(
                            "position_unavailable t={} reason={why:?}",
                            sample.sampled_ms
                        ),
                    }
                }
                if now.saturating_sub(last.unwrap_or(begin)) > 1500 && !stale {
                    log::info!("position_unavailable: no fresh client callback");
                    stale = true;
                }
            },
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// File identity includes both the name and the full retained hash.
    #[test]
    fn identity_is_exact_client_only() {
        assert!(identity_ok(Path::new("enshrouded.exe"), CLIENT_SHA));
        assert!(!identity_ok(Path::new("enshrouded_server.exe"), CLIENT_SHA));
        assert!(!identity_ok(Path::new("asi_host.exe"), CLIENT_SHA));
        assert!(!identity_ok(Path::new("enshrouded.exe"), "other"));
    }
    /// Enabling requires a valid explicit setting; malformed values stay errors.
    #[test]
    fn config_is_opt_in_and_bounded() {
        assert!(!parse_enabled(crate::DEFAULT_INI).unwrap());
        assert!(!parse_enabled("").unwrap());
        assert!(!parse_enabled("[Probe]\nEnabled=0").unwrap());
        assert!(parse_enabled("[Probe]\nEnabled=1").unwrap());
        assert!(parse_enabled("[Probe]\nEnabled=banana").is_err());
        assert!(parse_enabled(&"x".repeat(4097)).is_err());
    }
}

/// Convert a bounded timing sample; invalid clocks never receive a false fast result.
fn sample_us(start: i64, end: i64, frequency: i64) -> Option<u64> {
    if frequency <= 0 {
        return None;
    }
    let ticks = u128::try_from(end.checked_sub(start)?).ok()?;
    let micros = ticks
        .checked_mul(1_000_000)?
        .checked_div(u128::try_from(frequency).ok()?)?;
    u64::try_from(micros).ok()
}
#[cfg(test)]
mod budget_tests {
    /// Raw QPC timing is checked before deciding that a sample met its budget.
    #[test]
    fn timing_has_units_and_refuses_invalid_clocks() {
        assert_eq!(super::sample_us(10, 1010, 1_000_000), Some(1000));
        assert_eq!(super::sample_us(10, 10, 1_000_000), Some(0));
        assert_eq!(super::sample_us(10, 9, 1_000_000), None);
        assert_eq!(super::sample_us(0, 1, 0), None);
        assert_eq!(super::sample_us(i64::MIN, i64::MAX, 1), None);
    }
}
