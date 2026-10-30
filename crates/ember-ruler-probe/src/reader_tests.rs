//! Sparse synthetic address-space tests. No game or native pointer is supplied.
use super::*;
use std::cell::Cell;
use std::collections::BTreeMap;
/// Independent fixture for the named LocalPlayerData singleton.
const HASH: u32 = 0x9117771d;
/// Sparse owned bytes and a synthetic component response.
struct Fake {
    /// Bytes owned by this synthetic address-space provider.
    bytes: BTreeMap<usize, u8>,
    /// Reported native component byte count.
    size: u64,
    /// Observe whether a refused sample reached the component boundary.
    calls: Cell<u32>,
    /// Count all reads to bound a hostile or damaged map.
    reads: Cell<u32>,
    /// Simulate an owner replacement during the copy.
    change_after_component: bool,
}
impl Fake {
    /// Write a byte field into this address space.
    fn put(&mut self, at: usize, value: &[u8]) {
        for (i, b) in value.iter().enumerate() {
            self.bytes.insert(at + i, *b);
        }
    }
    /// Create a running client and one negative-coordinate point.
    fn new() -> Self {
        let mut m = Self {
            bytes: BTreeMap::new(),
            size: 56,
            calls: Cell::new(0),
            reads: Cell::new(0),
            change_after_component: false,
        };
        m.put(0x1000, &[2]);
        m.put(0x1000 + 0xe15dc8, &[2]);
        m.put(0x1000 + 0x7462d0, &0x2000u64.to_le_bytes());
        m.put(0x1000 + 0x7462d8, &1u64.to_le_bytes());
        let mut map = [0u8; 0x58];
        map[8..16].copy_from_slice(&0x3000u64.to_le_bytes());
        map[16..24].copy_from_slice(&8u64.to_le_bytes());
        map[0x28..0x30].copy_from_slice(&0x4000u64.to_le_bytes());
        map[0x40..0x48].copy_from_slice(&0x5000u64.to_le_bytes());
        map[0x54..0x58].copy_from_slice(&1u32.to_le_bytes());
        m.put(0x1000 + 0x7462f8, &map);
        let slot = HASH as usize & 7;
        m.put(0x3000, &(1u64 << slot).to_le_bytes());
        m.put(0x4000 + slot * 4, &HASH.to_le_bytes());
        m.put(0x5000 + slot * 2, &0u16.to_le_bytes());
        m.put(0x20a0, &HASH.to_le_bytes());
        m.put(0x20a8, &0x6000u64.to_le_bytes());
        m.put(0x6004, &42u32.to_le_bytes());
        m.put(0x7000, &(-4294967296i64).to_le_bytes());
        m.put(0x7008, &8589934592i64.to_le_bytes());
        m.put(0x7010, &0i64.to_le_bytes());
        m
    }
    /// Read this provider's callback using its synthetic world.
    fn point(&self) -> Result<RawPoint, Missing> {
        // SAFETY: 0x1000 is the live world of this owned fake address space.
        unsafe { sample(self, 0x1000) }
    }
}
impl Memory for Fake {
    fn read(&self, at: usize, out: &mut [u8]) -> bool {
        self.reads.set(self.reads.get() + 1);
        if self.change_after_component && self.calls.get() > 0 && at == 0x6004 {
            out.copy_from_slice(&43u32.to_le_bytes());
            return true;
        }
        for (i, b) in out.iter_mut().enumerate() {
            let Some(v) = self.bytes.get(&(at + i)) else {
                return false;
            };
            *b = *v;
        }
        true
    }
    unsafe fn component(&self, world: usize, player: u32) -> Option<(usize, u64)> {
        assert_eq!((world, player), (0x1000, 42));
        self.calls.set(self.calls.get() + 1);
        Some((0x7000, self.size))
    }
}
/// A raw signed point is copied without claiming metres or an epoch.
#[test]
fn copies_signed_axes_from_same_world() {
    let m = Fake::new();
    assert_eq!(
        m.point(),
        Ok(RawPoint {
            world_tag: 0x1000,
            player: 42,
            axes: [-4294967296, 8589934592, 0]
        })
    );
    assert_eq!(m.calls.get(), 1);
}
/// A server world never reaches the component getter.
#[test]
fn refuses_non_client() {
    let mut m = Fake::new();
    m.put(0x1000 + 0xe15dc8, &[0]);
    assert_eq!(m.point(), Err(Missing::World));
    assert_eq!(m.calls.get(), 0);
}
/// A loading world never reaches the component getter.
#[test]
fn refuses_loading() {
    let mut m = Fake::new();
    m.put(0x1000, &[1]);
    assert_eq!(m.point(), Err(Missing::World));
    assert_eq!(m.calls.get(), 0);
}
/// A component with the wrong reflected size is not read.
#[test]
fn refuses_wrong_component_size() {
    let mut m = Fake::new();
    m.size = 55;
    assert_eq!(m.point(), Err(Missing::Component));
}
/// An absent local player is explicit, not a zero point.
#[test]
fn refuses_no_player() {
    let mut m = Fake::new();
    m.put(0x6004, &0u32.to_le_bytes());
    assert_eq!(m.point(), Err(Missing::Player));
    assert_eq!(m.calls.get(), 0);
}
/// The local owner is checked again after copying.
#[test]
fn refuses_owner_change() {
    let mut m = Fake::new();
    m.change_after_component = true;
    assert_eq!(m.point(), Err(Missing::Changed));
}
/// Sparse or unreadable coordinates never become a partial point.
#[test]
fn refuses_partial_position() {
    let mut m = Fake::new();
    m.bytes.remove(&0x7017);
    assert_eq!(m.point(), Err(Missing::Read));
}
/// An overflowing address stops before any component call.
#[test]
fn rejects_overflow() {
    let m = Fake::new(); // SAFETY: fake provider treats all unowned addresses as refused reads.
    assert!(unsafe { sample(&m, usize::MAX) }.is_err());
    assert_eq!(m.calls.get(), 0);
}

/// An invalid map capacity is refused before looking up native components.
#[test]
fn refuses_malformed_map() {
    let mut m = Fake::new();
    m.put(0x1000 + 0x7462f8 + 16, &3u64.to_le_bytes());
    assert_eq!(m.point(), Err(Missing::Map));
    assert_eq!(m.calls.get(), 0);
}
/// A map index must be inside the currently copied singleton array.
#[test]
fn refuses_out_of_range_singleton() {
    let mut m = Fake::new();
    let slot = HASH as usize & 7;
    m.put(0x5000 + slot * 2, &1u16.to_le_bytes());
    assert_eq!(m.point(), Err(Missing::Singleton));
    assert_eq!(m.calls.get(), 0);
}
/// A stale map entry cannot substitute another singleton with a different name hash.
#[test]
fn refuses_wrong_singleton_hash() {
    let mut m = Fake::new();
    m.put(0x20a0, &0u32.to_le_bytes());
    assert_eq!(m.point(), Err(Missing::Singleton));
    assert_eq!(m.calls.get(), 0);
}
/// Hashing the public API's exact singleton name agrees with the fixture value.
#[test]
fn singleton_hash_is_source_name() {
    let hash = b"LocalPlayerData".iter().fold(0x811c9dc5u32, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(0x01000193)
    });
    assert_eq!(hash, HASH);
    assert_eq!(hash, LOCAL_PLAYER);
}

/// An occupied map with no matching key cannot make more than sixteen probes.
#[test]
fn bounds_map_probe_work() {
    let mut m = Fake::new();
    let map = 0x1000 + 0x7462f8;
    m.put(map + 16, &32u64.to_le_bytes());
    m.put(map + 0x54, &32u32.to_le_bytes());
    m.put(0x3000, &0xffff_ffffu64.to_le_bytes());
    for slot in 0..32 {
        m.put(0x4000 + slot * 4, &0u32.to_le_bytes());
    }
    assert_eq!(m.point(), Err(Missing::Singleton));
    assert_eq!(m.calls.get(), 0);
    assert!(
        m.reads.get() <= 35,
        "{} reads exceeded the work bound",
        m.reads.get()
    );
}
