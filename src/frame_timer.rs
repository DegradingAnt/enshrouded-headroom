//! Present timestamps: a lock-free ring that `vk::wrap_present` writes and the governor reads.
//!
//! [`FRAMES`] holds the `QueryPerformanceCounter` value of each of the last [`RING_LEN`] presents.
//! There is one producer, the game's present thread (the game has one present call site), and one
//! reader, the governor thread. Neither side blocks, allocates or takes a lock on the producer
//! side: a push is two atomic stores.
//!
//! # Lapping
//!
//! The ring overwrites its oldest slot. A reader that is slow, or that is in the middle of a read
//! while the producer goes round the ring, could copy a slot that has been overwritten with a
//! newer frame. [`Ring::read_since`] checks the head again after the copy and drops every entry
//! the producer may have reached in the meantime, so a value it returns is always the value of the
//! frame at that position.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// Slots in the ring.
pub const RING_LEN: usize = 4096;

/// [`RING_LEN`] as the type of the sequence numbers.
const RING_LEN_U64: u64 = RING_LEN as u64;

/// The last [`RING_LEN`] present timestamps, by sequence number. Sequence number `n` is the `n`th
/// push (from 0); it lives in slot `n % RING_LEN`.
pub struct Ring {
    slots: [AtomicU64; RING_LEN],
    /// Sequence number of the next push, so also the number of pushes so far.
    head: AtomicU64,
}

impl Default for Ring {
    fn default() -> Self {
        Self::new()
    }
}

impl Ring {
    /// An empty ring.
    pub const fn new() -> Ring {
        Ring {
            slots: [const { AtomicU64::new(0) }; RING_LEN],
            head: AtomicU64::new(0),
        }
    }

    /// The slot that holds sequence number `seq`.
    fn slot(&self, seq: u64) -> Option<&AtomicU64> {
        self.slots.get((seq % RING_LEN_U64) as usize)
    }

    /// Add `qpc` as the next frame. Single producer: only the present thread calls this.
    ///
    /// The slot is written before the head is published (`Release`), so a reader that sees the new
    /// head also sees the slot. Two threads pushing at once would not break memory safety (it is
    /// all atomics), but one of the two frames could be lost.
    pub fn push(&self, qpc: u64) {
        // Relaxed: only this thread writes the head.
        let head = self.head.load(Ordering::Relaxed);
        if let Some(slot) = self.slot(head) {
            // Release, so a reader whose Acquire load sees this value also sees the head store
            // that came before it (the lapping check in `read_since_with` relies on that).
            slot.store(qpc, Ordering::Release);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
    }

    /// Append to `out` every frame from sequence number `seq` on that is still in the ring, oldest
    /// first, and return the sequence number to pass next time.
    ///
    /// - `seq` older than the ring holds: the read starts at the oldest frame it still holds.
    /// - `seq` at or past the head: nothing is appended, and the head is returned.
    /// - A frame the producer may have overwritten while it was being copied is not appended (see
    ///   the module docs), so `out` may get fewer frames than the head moved by.
    pub fn read_since(&self, seq: u64, out: &mut Vec<u64>) -> u64 {
        self.read_since_with(seq, out, || {})
    }

    /// The body of [`Ring::read_since`]. `between` runs after the head is read and before the slots
    /// are copied, so a test can make the producer lap the reader at the worst moment.
    fn read_since_with(&self, seq: u64, out: &mut Vec<u64>, between: impl FnOnce()) -> u64 {
        let head = self.head.load(Ordering::Acquire);
        let start = seq.max(head.saturating_sub(RING_LEN_U64)).min(head);
        between();
        let first = out.len();
        for n in start..head {
            if let Some(slot) = self.slot(n) {
                out.push(slot.load(Ordering::Acquire));
            }
        }
        // Which of the copied frames can the producer have reached? It had published `now` and may
        // be writing sequence number `now`, whose slot is that of `now - RING_LEN`. So every
        // sequence number up to and including `now - RING_LEN` may hold a newer frame. (The slots
        // are read with Acquire and written with Release: a copied value from a later lap implies
        // that this load sees at least the head that lap started from.)
        let now = self.head.load(Ordering::Acquire);
        let trusted_from = now.saturating_add(1).saturating_sub(RING_LEN_U64);
        let stale = trusted_from
            .saturating_sub(start)
            .min(head.saturating_sub(start));
        let stale = usize::try_from(stale).unwrap_or(usize::MAX);
        let end = first.saturating_add(stale).min(out.len());
        out.drain(first..end);
        head
    }
}

/// The present timestamps of the game: written by `vk::wrap_present`, read by the governor.
pub static FRAMES: Ring = Ring::new();

/// `QueryPerformanceCounter` now.
pub fn qpc_now() -> u64 {
    let mut now = 0i64;
    // SAFETY: `now` is a valid place for the one i64 the call writes. It cannot fail on Windows
    // XP or later; if it did, `now` stays 0.
    unsafe { QueryPerformanceCounter(&mut now) };
    now.max(0) as u64
}

/// `QueryPerformanceFrequency`, read once. 0 only if Windows reports no counter, which it does
/// not since Windows XP.
fn qpc_frequency() -> u64 {
    static FREQUENCY: OnceLock<u64> = OnceLock::new();
    *FREQUENCY.get_or_init(|| {
        let mut frequency = 0i64;
        // SAFETY: `frequency` is a valid place for the one i64 the call writes; on failure it
        // stays 0.
        unsafe { QueryPerformanceFrequency(&mut frequency) };
        frequency.max(0) as u64
    })
}

/// `delta` QPC ticks in milliseconds. 0.0 if the frequency is unknown (0).
pub fn qpc_to_ms(delta: u64) -> f64 {
    match qpc_frequency() {
        0 => 0.0,
        frequency => delta as f64 * 1000.0 / frequency as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_order_and_wraps() {
        let r = Ring::new();
        for i in 0..5000u64 {
            r.push(i);
        }
        let mut v = Vec::new();
        let s = r.read_since(5000 - 10, &mut v);
        assert_eq!(s, 5000);
        assert_eq!(v, (4990..5000).collect::<Vec<_>>());
    }

    #[test]
    fn an_empty_ring_gives_nothing() {
        let r = Ring::new();
        let mut v = Vec::new();
        assert_eq!(r.read_since(0, &mut v), 0);
        assert!(v.is_empty());
    }

    #[test]
    fn reads_continue_where_the_last_one_stopped() {
        let r = Ring::new();
        let mut v = Vec::new();
        for i in 0..3u64 {
            r.push(100 + i);
        }
        let s = r.read_since(0, &mut v);
        assert_eq!((s, v.as_slice()), (3, &[100, 101, 102][..]));
        r.push(103);
        r.push(104);
        let s = r.read_since(s, &mut v);
        // Appended after what `v` already held.
        assert_eq!((s, v.as_slice()), (5, &[100, 101, 102, 103, 104][..]));
    }

    #[test]
    fn a_seq_past_the_head_gives_nothing() {
        let r = Ring::new();
        for i in 0..10u64 {
            r.push(i);
        }
        let mut v = Vec::new();
        assert_eq!(r.read_since(10, &mut v), 10);
        assert_eq!(r.read_since(u64::MAX, &mut v), 10);
        assert!(v.is_empty());
    }

    #[test]
    fn a_reader_that_fell_a_lap_behind_gets_what_the_ring_still_holds() {
        let r = Ring::new();
        for i in 0..5000u64 {
            r.push(i);
        }
        let mut v = Vec::new();
        assert_eq!(r.read_since(0, &mut v), 5000);
        // The oldest slot is the one the next push overwrites, so it is not trusted: 4095 frames.
        assert_eq!(v, (5000 - 4095..5000).collect::<Vec<_>>());
    }

    #[test]
    fn frames_overwritten_during_the_read_are_dropped() {
        let r = Ring::new();
        for i in 0..RING_LEN_U64 {
            r.push(i);
        }
        let mut v = vec![7];
        // While the reader is between reading the head and copying, the producer pushes 10 more
        // frames: sequence numbers 0..10 are overwritten (and 10 is the next to go).
        let s = r.read_since_with(0, &mut v, || {
            for i in RING_LEN_U64..RING_LEN_U64 + 10 {
                r.push(i);
            }
        });
        assert_eq!(s, RING_LEN_U64, "the head as it was when the read started");
        // Every value kept is the value of its own sequence number: nothing overwritten got in.
        let mut want = vec![7];
        want.extend(11..RING_LEN_U64);
        assert_eq!(v, want);
        // The next read picks up the 10 new frames.
        let mut next = Vec::new();
        assert_eq!(r.read_since(s, &mut next), RING_LEN_U64 + 10);
        assert_eq!(next, (RING_LEN_U64..RING_LEN_U64 + 10).collect::<Vec<_>>());
    }

    #[test]
    fn a_whole_lap_during_the_read_drops_everything() {
        let r = Ring::new();
        for i in 0..100u64 {
            r.push(i);
        }
        let mut v = Vec::new();
        let s = r.read_since_with(0, &mut v, || {
            for i in 100..100 + RING_LEN_U64 + 1 {
                r.push(i);
            }
        });
        assert_eq!(s, 100);
        assert!(v.is_empty(), "{} stale frames kept", v.len());
    }

    #[test]
    fn qpc_moves_forward_and_converts_to_ms() {
        let a = qpc_now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = qpc_now();
        assert!(a > 0 && b > a, "{a} then {b}");
        let freq = qpc_frequency();
        assert!(freq > 0);
        assert!((qpc_to_ms(freq) - 1000.0).abs() < 1e-9);
        assert_eq!(qpc_to_ms(0), 0.0);
        let ms = qpc_to_ms(b - a);
        assert!((1.0..1000.0).contains(&ms), "{ms} ms for a 2 ms sleep");
    }
}
