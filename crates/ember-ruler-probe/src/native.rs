//! OS byte copies and a single fault-guarded native getter. No pointer leaves the callback.
use crate::reader::Memory;
use std::ffi::c_void;
use windows_sys::Win32::System::{
    Diagnostics::Debug::ReadProcessMemory, Threading::GetCurrentProcess,
};
unsafe extern "C" {
    /// C++ leaf with the exact pinned GetComponent output ABI.
    fn ruler_component(target: usize, world: *mut c_void, player: u32, out: *mut u64) -> u8;
    /// Inert test driver; its archive object is not referenced in a release DLL.
    #[cfg(test)]
    fn ruler_cpp_checks() -> u32;
}
/// Startup-validated getter; each call checks its original prefix again.
pub(crate) struct NativeMemory {
    /// Exact-target component function, established during startup.
    pub getter: usize,
}
/// Original first24 bytes on the exact permitted client.
pub(crate) const GET_PREFIX: [u8; 24] = [
    0x48, 0x89, 0x5c, 0x24, 0x08, 0x48, 0x89, 0x74, 0x24, 0x10, 0x57, 0x48, 0x83, 0xec, 0x20, 0x48,
    0x8d, 0xba, 0xc0, 0x5d, 0xe1, 0x00, 0x44, 0x89,
];
impl Memory for NativeMemory {
    fn read(&self, address: usize, out: &mut [u8]) -> bool {
        if address == 0 || address.checked_add(out.len()).is_none() {
            return false;
        }
        let mut copied = 0usize;
        // SAFETY: GetCurrentProcess is a pseudo-handle; the OS validates the source address.
        // out owns exactly out.len initialized writable bytes and copied is a live size word.
        unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                address as *const c_void,
                out.as_mut_ptr().cast(),
                out.len(),
                &mut copied,
            ) != 0
                && copied == out.len()
        }
    }
    unsafe fn component(&self, world: usize, player: u32) -> Option<(usize, u64)> {
        let mut prefix = [0u8; 24];
        if !self.read(self.getter, &mut prefix) || prefix != GET_PREFIX {
            return None;
        }
        let mut out = [0u64; 2];
        // SAFETY: caller's owner-thread world contract, verified exact-target function ABI/prefix,
        // and two owned output words. C++ catches structured faults from this lookup only.
        let ok =
            unsafe { ruler_component(self.getter, world as *mut c_void, player, out.as_mut_ptr()) };
        if ok == 0 {
            return None;
        }
        let ptr = usize::try_from(*out.first()?).ok()?;
        let size = *out.get(1)?;
        (ptr != 0).then_some((ptr, size))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// The C++ driver checks call order, foreign unwind and SEH without a game.
    #[test]
    fn native_original_once_and_faults_stay_native() {
        // SAFETY: inert driver catches its own foreign exceptions; no game addresses are used.
        assert_eq!(unsafe { ruler_cpp_checks() }, 0);
    }
    /// OS copies are exact and invalid sources fail without Rust dereference.
    #[test]
    fn copies_owned_bytes_and_refuses_unmapped() {
        let memory = NativeMemory { getter: 0 };
        let source = [1u8, 2, 3];
        let mut out = [0u8; 3];
        assert!(memory.read(source.as_ptr() as usize, &mut out));
        assert_eq!(out, source);
        assert!(!memory.read(1, &mut out));
        assert!(!memory.read(usize::MAX, &mut out));
    }
}
