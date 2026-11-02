//! The refresh period of the monitor that shows the game window: the governor's frame-time floor.
//!
//! The governor (Task 15) calls `period_ms(refresh_hz(game_window()?))` every few seconds from its
//! own thread, not the game's window thread. All three functions are read-only queries on user32.
//! They keep no state between calls, take no lock, touch no game memory and do not log (the caller
//! logs; the one exception is the panic guard around the `EnumWindows` callback, which logs once if
//! the callback ever panics). Each call asks Windows afresh, so a change of display mode, or of the
//! monitor the window is on, shows up on the next call. If the callback panics, the process-wide
//! guard disables the governor, so `game_window` returns `None` on later calls.

use crate::guard::{self, guard, Feature};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromWindow, DEVMODEW, ENUM_CURRENT_SETTINGS,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
};

/// The period [`period_ms`] reports when the refresh rate is unknown: 60 Hz.
const FALLBACK_PERIOD_MS: f64 = 1000.0 / 60.0;

/// `cbSize` for `GetMonitorInfoW`: Windows rejects the call if this is not the exact struct size.
const MONITORINFOEXW_SIZE: u32 = size_of::<MONITORINFOEXW>() as u32;

/// `dmSize` for `EnumDisplaySettingsW`: Windows rejects the call if this is not the exact struct
/// size.
const DEVMODEW_SIZE: u16 = size_of::<DEVMODEW>() as u16;

// The two casts above must not truncate. (The sizes are 104 and 220 bytes.)
const _: () = assert!(size_of::<MONITORINFOEXW>() <= u32::MAX as usize);
const _: () = assert!(size_of::<DEVMODEW>() <= u16::MAX as usize);

/// The largest visible, unowned top-level window of this process, or `None` if there is none (for
/// example in a test binary, or before the game has made its window).
///
/// "Largest" is by `GetWindowRect` area. Of equal areas the first one `EnumWindows` reports (in
/// practice the one highest in the Z order) wins. A window with no area never wins.
///
/// This is a heuristic for the window of the game's swapchain, not a lookup of it. A wrong pick
/// changes the result only when a larger window of this process sits on another monitor with a
/// different refresh rate. (Taking the window from the swapchain's surface instead is deferred.)
///
/// If the `EnumWindows` callback ever panics, the panic guard switches the governor off and this
/// returns `None` from then on.
pub fn game_window() -> Option<HWND> {
    scan().0
}

/// What [`game_window`] does: the pick (`None` if there is no candidate, if `EnumWindows` fails, or
/// if the governor has been switched off) and how many windows `EnumWindows` offered the callback,
/// which a test uses to prove that the enumeration ran at all.
fn scan() -> (Option<HWND>, u32) {
    let mut state = EnumState {
        // SAFETY: GetCurrentProcessId has no preconditions.
        pid: unsafe { GetCurrentProcessId() },
        largest: Largest::new(),
        seen: 0,
    };
    // SAFETY: `enum_proc` has the WNDENUMPROC signature. The LPARAM is the address of `state`,
    // which is alive and used by nothing else until EnumWindows returns: EnumWindows calls back
    // synchronously, on this thread, and only before it returns.
    let enumerated =
        unsafe { EnumWindows(Some(enum_proc), &mut state as *mut EnumState as LPARAM) };
    // `enum_proc` always returns TRUE, so a zero is EnumWindows itself failing. A switched-off
    // governor means the callback panicked (or did nothing): a partial pick is not to be trusted.
    if enumerated == 0 || guard::is_disabled(Feature::Governor) {
        return (None, state.seen);
    }
    (state.largest.winner(), state.seen)
}

/// What the `EnumWindows` callback needs, passed through its `LPARAM`.
struct EnumState {
    /// The id of this process.
    pid: u32,
    /// The best candidate so far.
    largest: Largest<HWND>,
    /// How many windows the callback has been offered, whether or not they qualify.
    seen: u32,
}

/// The `EnumWindows` callback. Like every extern function in the mod, its body runs inside
/// [`guard()`], as [`Feature::Governor`] (the governor is the only caller). A panic cannot unwind out
/// of an `extern "system"` function (it would abort the game), so the guard catches it, switches
/// the governor off and returns TRUE as usual; [`scan`] then returns no window. [`consider`] has no
/// indexing, unwrapping or arithmetic that can overflow, so this is a backstop. Both the guarded
/// and the fallback path return TRUE, to go on to the next window.
unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    guard(
        Feature::Governor,
        || TRUE,
        || {
            // SAFETY: `lparam` is the address of the `EnumState` that `scan` keeps alive, and
            // nothing else uses, for the whole of the EnumWindows call; `as_mut` turns a null into
            // `None`.
            if let Some(state) = unsafe { (lparam as *mut EnumState).as_mut() } {
                consider(state, hwnd);
            }
            TRUE
        },
    )
}

/// Offers `hwnd` to `state` if it is a visible, unowned top-level window of this process. Any
/// Windows call that fails rules the window out.
fn consider(state: &mut EnumState, hwnd: HWND) {
    state.seen = state.seen.saturating_add(1);
    let mut pid = 0u32;
    // SAFETY: user32 validates the handle itself (a window that has gone since EnumWindows listed
    // it just makes the call fail), and `pid` is valid for writes.
    let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if thread == 0 || pid != state.pid {
        return;
    }
    // SAFETY: as above, user32 validates the handle.
    if unsafe { IsWindowVisible(hwnd) } == 0 {
        return;
    }
    // SAFETY: as above. A null result means no owner (or a window that has just gone, which the
    // GetWindowRect call below then rules out).
    if !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null() {
        return;
    }
    let mut rect = RECT::default();
    // SAFETY: as above, and `rect` is valid for writes.
    if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
        return;
    }
    state.largest.offer(rect_area(&rect), hwnd);
}

/// The refresh rate in Hz of the monitor that `hwnd` is on (the nearest one, if it is on none), or
/// `None` if Windows cannot tell or reports only the "hardware default" (0 or 1).
///
/// This is the `dmDisplayFrequency` of the monitor's current display mode. On a variable refresh
/// rate display (G-SYNC, FreeSync) that is the mode's maximum rate, not the rate the panel scans
/// at from moment to moment, so it is a ceiling: the governor treats it as the frame-time floor,
/// the shortest frame time the display can show.
///
/// The value is whole Hz: a 59.94 Hz mode reads 59.
// `HWND` is a raw pointer type, but a window handle is an opaque value that user32 looks up in its
// own tables: this process never dereferences it, so the function is safe for any value.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn refresh_hz(hwnd: HWND) -> Option<u32> {
    // SAFETY: `hwnd` is an opaque handle that user32 validates itself and this process never
    // dereferences, so any value is sound to pass. An invalid handle gives a null or a nearest
    // monitor, and the null is handled below.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_null() {
        return None;
    }

    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = MONITORINFOEXW_SIZE;
    // SAFETY: `info` is a valid MONITORINFOEXW with `cbSize` set to its size, and MONITORINFOEXW
    // begins with a MONITORINFO, so a pointer to it is a valid LPMONITORINFO for this call.
    let got_info = unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()) };
    // Windows NUL-terminates the device name; check anyway, because the next call reads it as a
    // C string.
    if got_info == 0 || !info.szDevice.contains(&0) {
        return None;
    }

    let mut mode = DEVMODEW {
        dmSize: DEVMODEW_SIZE,
        ..Default::default()
    };
    // SAFETY: `info.szDevice` is a NUL-terminated UTF-16 string that outlives the call, and `mode`
    // is a valid DEVMODEW with `dmSize` set to its size and no driver-extra bytes.
    let got_mode =
        unsafe { EnumDisplaySettingsW(info.szDevice.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) };
    if got_mode == 0 {
        return None;
    }
    hz_from_mode(mode.dmDisplayFrequency)
}

/// The frame period in milliseconds for a refresh rate: `1000 / hz`. `None` gives 60 Hz
/// (`1000 / 60`). A rate of 0 or 1 is the "hardware default" and gets the same answer as `None`,
/// so this never divides by zero and never returns infinity or NaN.
pub fn period_ms(hz: Option<u32>) -> f64 {
    match hz.and_then(hz_from_mode) {
        Some(hz) => 1000.0 / f64::from(hz),
        None => FALLBACK_PERIOD_MS,
    }
}

/// Turns the raw `dmDisplayFrequency` into a usable rate: 0 and 1 mean "hardware default".
fn hz_from_mode(raw: u32) -> Option<u32> {
    (raw > 1).then_some(raw)
}

/// Area in pixels of a window rectangle. A reversed or empty rectangle (negative width or height)
/// counts as 0, and the arithmetic cannot overflow.
fn rect_area(rect: &RECT) -> u64 {
    let side = |low: i32, high: i32| u64::try_from(high.saturating_sub(low)).unwrap_or(0);
    side(rect.left, rect.right).saturating_mul(side(rect.top, rect.bottom))
}

/// The "pick the largest window" rule, one candidate at a time (so the `EnumWindows` callback can
/// run it without allocating). `H` is the window handle.
struct Largest<H> {
    best: Option<(u64, H)>,
}

impl<H: Copy> Largest<H> {
    fn new() -> Self {
        Largest { best: None }
    }

    /// Offers a candidate. It replaces the best so far only if its area is strictly larger, so of
    /// equal areas the first offered wins. A zero area is never a winner.
    fn offer(&mut self, area: u64, hwnd: H) {
        let to_beat = self.best.map_or(0, |(best, _)| best);
        if area > to_beat {
            self.best = Some((area, hwnd));
        }
    }

    /// The handle of the largest candidate offered so far.
    fn winner(&self) -> Option<H> {
        self.best.map(|(_, hwnd)| hwnd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, PoisonError};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, GetDesktopWindow, WS_EX_NOACTIVATE, WS_POPUP, WS_VISIBLE,
    };

    /// Serialises the tests that depend on which windows this process owns.
    static WINDOWS: Mutex<()> = Mutex::new(());

    /// Offers `list` (area, id) to a [`Largest`] in order and returns its winner.
    fn pick(list: &[(u64, u32)]) -> Option<u32> {
        let mut largest = Largest::new();
        for &(area, id) in list {
            largest.offer(area, id);
        }
        largest.winner()
    }

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn period_matches_the_rate() {
        assert_eq!(period_ms(Some(120)), 1000.0 / 120.0);
        assert_eq!(period_ms(Some(60)), 1000.0 / 60.0);
        assert_eq!(period_ms(Some(144)), 1000.0 / 144.0);
        assert_eq!(period_ms(None), 1000.0 / 60.0);
    }

    #[test]
    fn period_never_divides_by_zero() {
        // 0 and 1 are the "hardware default" values: same as None, and finite.
        assert_eq!(period_ms(Some(0)), 1000.0 / 60.0);
        assert_eq!(period_ms(Some(1)), 1000.0 / 60.0);
        assert_eq!(period_ms(Some(2)), 500.0);
        let huge = period_ms(Some(u32::MAX));
        assert!(huge.is_finite() && huge > 0.0);
    }

    #[test]
    fn hardware_default_rates_are_unknown() {
        assert_eq!(hz_from_mode(0), None);
        assert_eq!(hz_from_mode(1), None);
        assert_eq!(hz_from_mode(59), Some(59));
        assert_eq!(hz_from_mode(60), Some(60));
        assert_eq!(hz_from_mode(165), Some(165));
    }

    #[test]
    fn rect_area_is_width_times_height() {
        assert_eq!(rect_area(&rect(0, 0, 1920, 1080)), 1920 * 1080);
        assert_eq!(rect_area(&rect(-10, -20, 10, 20)), 20 * 40);
        assert_eq!(rect_area(&rect(5, 5, 5, 100)), 0);
        assert_eq!(rect_area(&rect(5, 5, 100, 5)), 0);
    }

    #[test]
    fn rect_area_counts_negative_sizes_as_zero() {
        // Reversed in one axis.
        assert_eq!(rect_area(&rect(100, 0, 0, 50)), 0);
        assert_eq!(rect_area(&rect(0, 100, 50, 0)), 0);
        // Reversed in both: a naive product of two negatives would be positive.
        assert_eq!(rect_area(&rect(100, 100, 0, 0)), 0);
    }

    #[test]
    fn rect_area_cannot_overflow() {
        let full = rect_area(&rect(i32::MIN, i32::MIN, i32::MAX, i32::MAX));
        assert!(full > 0);
        assert_eq!(rect_area(&rect(i32::MAX, i32::MAX, i32::MIN, i32::MIN)), 0);
    }

    #[test]
    fn largest_of_an_empty_list_is_none() {
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn largest_picks_the_biggest_area_in_any_order() {
        assert_eq!(pick(&[(10, 1)]), Some(1));
        assert_eq!(pick(&[(10, 1), (30, 2), (20, 3)]), Some(2));
        assert_eq!(pick(&[(30, 2), (20, 3), (10, 1)]), Some(2));
        assert_eq!(pick(&[(10, 1), (20, 3), (30, 2)]), Some(2));
    }

    #[test]
    fn largest_keeps_the_first_of_equal_areas() {
        assert_eq!(pick(&[(10, 1), (10, 2)]), Some(1));
        assert_eq!(pick(&[(10, 1), (30, 2), (30, 3), (30, 4)]), Some(2));
    }

    #[test]
    fn largest_never_picks_a_zero_area() {
        assert_eq!(pick(&[(0, 1)]), None);
        assert_eq!(pick(&[(0, 1), (0, 2)]), None);
        assert_eq!(pick(&[(0, 1), (5, 2)]), Some(2));
        assert_eq!(pick(&[(5, 2), (0, 1)]), Some(2));
    }

    #[test]
    fn struct_sizes_match_the_win32_abi() {
        // Windows rejects a MONITORINFOEXW or DEVMODEW whose size field is not exactly this.
        assert_eq!(size_of::<MONITORINFOEXW>(), 104);
        assert_eq!(size_of::<DEVMODEW>(), 220);
    }

    #[test]
    fn refresh_hz_is_safe_for_any_handle() {
        // A window handle is an opaque value that user32 validates, never a pointer this process
        // follows (the reason `refresh_hz` is a safe fn). Whatever the handle, the answer is
        // either "unknown" or a usable rate.
        for hwnd in [std::ptr::null_mut(), 0x1234usize as HWND] {
            let hz = refresh_hz(hwnd);
            assert!(
                hz.is_none_or(|hz| hz > 1),
                "handle {hwnd:?} gave {hz:?}, not None or a rate above 1"
            );
        }
    }

    #[test]
    fn no_game_window_in_the_test_binary() {
        let _lock = WINDOWS.lock().unwrap_or_else(PoisonError::into_inner);
        // The test harness makes no window; this walks every window on the desktop through the
        // EnumWindows callback and rejects them all (they belong to other processes).
        let (found, seen) = scan();
        assert_eq!(found, None);
        // An empty answer is also what a failed or skipped enumeration gives, so check that the
        // callback really was offered windows.
        assert!(seen > 0, "EnumWindows offered the callback no windows");
        assert_eq!(game_window(), None);
    }

    /// Owns the windows a test made, and destroys them when it ends, passed or failed.
    struct Made(Vec<HWND>);

    impl Drop for Made {
        fn drop(&mut self) {
            for &hwnd in self.0.iter().rev() {
                // SAFETY: `hwnd` was made on this thread, which is the thread that runs the drop.
                unsafe { DestroyWindow(hwnd) };
            }
        }
    }

    /// Makes a window of the system class STATIC, `width` x `height` at the origin.
    fn make_window(owner: HWND, style: u32, width: i32, height: i32) -> HWND {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: `class` is NUL-terminated and outlives the call; the window name is null; every
        // other argument is a plain value or null. `owner` is null or a window this thread made.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE,
                class.as_ptr(),
                std::ptr::null(),
                style,
                0,
                0,
                width,
                height,
                owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null(), "CreateWindowExW failed");
        hwnd
    }

    #[test]
    #[ignore = "needs an interactive desktop: it makes real windows"]
    fn game_window_is_the_largest_visible_unowned_window() {
        let _lock = WINDOWS.lock().unwrap_or_else(PoisonError::into_inner);
        let mut made = Made(Vec::new());
        assert_eq!(game_window(), None);

        let small = make_window(std::ptr::null_mut(), WS_POPUP | WS_VISIBLE, 100, 50);
        made.0.push(small);
        assert_eq!(game_window(), Some(small));

        let big = make_window(std::ptr::null_mut(), WS_POPUP | WS_VISIBLE, 300, 200);
        made.0.push(big);
        assert_eq!(game_window(), Some(big));

        // Bigger, but owned by `big`: not a top-level window of its own, so skipped.
        let owned = make_window(big, WS_POPUP | WS_VISIBLE, 600, 400);
        made.0.push(owned);
        assert_eq!(game_window(), Some(big));

        // Bigger still, but not visible: skipped.
        let hidden = make_window(std::ptr::null_mut(), WS_POPUP, 800, 600);
        made.0.push(hidden);
        assert_eq!(game_window(), Some(big));

        // A real window of ours, not only the desktop window, has a refresh rate.
        assert!(refresh_hz(big).is_some_and(|hz| hz >= 30));

        // What it returns belongs to this process.
        let mut pid = 0u32;
        // SAFETY: `big` is a window this thread made and `pid` is valid for writes.
        unsafe { GetWindowThreadProcessId(big, &mut pid) };
        // SAFETY: GetCurrentProcessId has no preconditions.
        assert_eq!(pid, unsafe { GetCurrentProcessId() });

        drop(made);
        assert_eq!(game_window(), None);
    }

    #[test]
    #[ignore = "needs a display"]
    fn primary_monitor_has_rate() {
        // SAFETY: GetDesktopWindow has no preconditions.
        let desktop = unsafe { GetDesktopWindow() };
        let hz = refresh_hz(desktop).expect("the primary monitor reports a refresh rate");
        println!(
            "primary monitor: {hz} Hz, period {:.3} ms",
            period_ms(Some(hz))
        );
        assert!(hz >= 30, "refresh rate {hz} Hz");
    }
}
