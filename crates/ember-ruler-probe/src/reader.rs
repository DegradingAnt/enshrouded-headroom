//! Callback-scoped raw positions; no coordinate calibration or retained native pointer.
/// A copied diagnostic observation, never a WorldEpoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawPoint {
    /// Diagnostic correlation tag; it is never dereferenced after the callback.
    pub world_tag: usize,
    /// Local entity in this callback's world.
    pub player: u32,
    /// Uncalibrated signed fixed-point coordinates.
    pub axes: [i64; 3],
}
/// Reasons that no raw point can be published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Missing {
    /// Counter failure or sampling exceeded the two-millisecond fuse.
    Budget,
    /// Memory could not be copied exactly.
    Read,
    /// The callback world is not a running client.
    World,
    /// The singleton map header violates the source bounds.
    Map,
    /// No validated LocalPlayerData entry was found within the probe bound.
    Singleton,
    /// The local entity is not available.
    Player,
    /// The getter or reflected component size was refused.
    Component,
    /// Owner assertions no longer match the start of this copy.
    Changed,
}
/// Memory services supplied by the native callback or an inert test address space.
pub(crate) trait Memory {
    /// Copy bytes into owned storage; false for a partial or refused read.
    fn read(&self, address: usize, out: &mut [u8]) -> bool;
    /// Resolve this callback world's component, without retaining its pointer.
    /// # Safety
    /// `world` belongs to this provider on its owner thread for this callback.
    unsafe fn component(&self, world: usize, player: u32) -> Option<(usize, u64)>;
}
/// Copy a point from a live owner-thread callback.
/// # Safety
/// `world` is this provider's valid client-world address for the whole call.
pub(crate) unsafe fn sample(memory: &impl Memory, world: usize) -> Result<RawPoint, Missing> {
    if byte(memory, world, 0)? != 2 || byte(memory, world, 0xe15dc8)? != 2 {
        return Err(Missing::World);
    }
    let entry = local_entry(memory, world)?;
    let object = pointer(memory, entry, 0xa8)?;
    let player = word(memory, object, 4)?;
    if player == 0 {
        return Err(Missing::Player);
    }
    // SAFETY: sample's caller guarantees this provider owns world on this callback thread.
    let (component, size) = unsafe { memory.component(world, player) }.ok_or(Missing::Component)?;
    if component == 0 || size != 56 {
        return Err(Missing::Component);
    }
    let bytes = field::<24>(memory, component, 0)?;
    let mut axes = [0i64; 3];
    for (axis, raw) in axes.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *axis = i64::from_le_bytes(*raw);
    }
    if byte(memory, world, 0)? != 2
        || byte(memory, world, 0xe15dc8)? != 2
        || local_entry(memory, world)? != entry
        || pointer(memory, entry, 0xa8)? != object
        || word(memory, object, 4)? != player
    {
        return Err(Missing::Changed);
    }
    Ok(RawPoint {
        world_tag: world,
        player,
        axes,
    })
}
/// Hash of the source API's exact singleton name.
const LOCAL_PLAYER: u32 = 0x9117771d;
/// The source map is bounded further to sixteen occupied slots per sample.
const MAX_PROBES: u64 = 16;
/// Resolve the local singleton in THIS callback's world, never a latest-world tracker.
fn local_entry(memory: &impl Memory, world: usize) -> Result<usize, Missing> {
    let map = field::<0x58>(memory, world, 0x7462f8)?;
    let bits = usize_field(&map, 8)?;
    let capacity = u64_field(&map, 16)?;
    let keys = usize_field(&map, 0x28)?;
    let values = usize_field(&map, 0x40)?;
    let count = u32::from_le_bytes(
        map.get(0x54..0x58)
            .and_then(|b| b.try_into().ok())
            .ok_or(Missing::Map)?,
    );
    if capacity == 0
        || !capacity.is_power_of_two()
        || capacity > (1 << 22)
        || bits == 0
        || keys == 0
        || values == 0
        || count == 0
        || u64::from(count) > capacity
    {
        return Err(Missing::Map);
    }
    let mut slot = u64::from(LOCAL_PLAYER) & (capacity - 1);
    for _ in 0..capacity.min(MAX_PROBES) {
        let occupancy = u64::from_le_bytes(field::<8>(memory, bits, index(slot >> 6, 8)?)?);
        if occupancy & (1u64 << (slot & 63)) == 0 {
            return Err(Missing::Singleton);
        }
        if word(memory, keys, index(slot, 4)?)? == LOCAL_PLAYER {
            let selected = u64::from(u16::from_le_bytes(field::<2>(
                memory,
                values,
                index(slot, 2)?,
            )?));
            let total = u64::from_le_bytes(field::<8>(memory, world, 0x7462d8)?);
            if total == 0 || total > 65536 || selected >= total {
                return Err(Missing::Singleton);
            }
            let entries = pointer(memory, world, 0x7462d0)?;
            let entry = entries
                .checked_add(index(selected, 0xe8)?)
                .ok_or(Missing::Read)?;
            if word(memory, entry, 0xa0)? != LOCAL_PLAYER {
                return Err(Missing::Singleton);
            }
            return Ok(entry);
        }
        slot = (slot + 1) & (capacity - 1);
    }
    Err(Missing::Singleton)
}
/// Checked offset into a bounded native array.
fn index(index: u64, stride: u64) -> Result<usize, Missing> {
    index
        .checked_mul(stride)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(Missing::Read)
}
/// Copy exactly N bytes, rejecting a null, overflowing or partial read.
fn field<const N: usize>(
    memory: &impl Memory,
    base: usize,
    offset: usize,
) -> Result<[u8; N], Missing> {
    let address = base.checked_add(offset).ok_or(Missing::Read)?;
    address.checked_add(N).ok_or(Missing::Read)?;
    let mut bytes = [0u8; N];
    if base == 0 || !memory.read(address, &mut bytes) {
        return Err(Missing::Read);
    }
    Ok(bytes)
}
/// Copy one byte.
fn byte(memory: &impl Memory, base: usize, offset: usize) -> Result<u8, Missing> {
    Ok(u8::from_le_bytes(field::<1>(memory, base, offset)?))
}
/// Copy a 32-bit word.
fn word(memory: &impl Memory, base: usize, offset: usize) -> Result<u32, Missing> {
    Ok(u32::from_le_bytes(field::<4>(memory, base, offset)?))
}
/// Copy a native x64 address; this does not dereference it.
fn pointer(memory: &impl Memory, base: usize, offset: usize) -> Result<usize, Missing> {
    usize::try_from(u64::from_le_bytes(field::<8>(memory, base, offset)?))
        .map_err(|_| Missing::Read)
}
/// Decode one unaligned map field from an owned copy.
fn u64_field(bytes: &[u8], offset: usize) -> Result<u64, Missing> {
    let end = offset.checked_add(8).ok_or(Missing::Map)?;
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..end)
            .and_then(|b| b.try_into().ok())
            .ok_or(Missing::Map)?,
    ))
}
/// Decode a map address from owned bytes.
fn usize_field(bytes: &[u8], offset: usize) -> Result<usize, Missing> {
    usize::try_from(u64_field(bytes, offset)?).map_err(|_| Missing::Map)
}
#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
