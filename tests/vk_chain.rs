//! End to end: the Vulkan wrapper chain, driven the way the game drives it, in front of a fake
//! Vulkan in this process.
//!
//! The child process (the test binary started again with `CHILD_ENV` set, as in
//! `startup_logging.rs`, because the chain's statics are first-value-wins and process-global):
//!
//! 1. puts a fake `GetProcAddress` into this exe's KERNEL32 `GetProcAddress` import slot. It
//!    stands for whatever the slot held before the mod: kernel32's function or another tool's
//!    hook. It answers for the fake `vulkan-1.dll` (and for a null module) and passes everything
//!    else on to kernel32;
//! 2. runs `vk::init`, the start-up step `entry::start` runs on the game. It swaps that slot to
//!    `wrap_gpa`, keeping the fake in `REAL_GPA`;
//! 3. writes a DLL named `vulkan-1.dll` (headers and an empty relocation block: no code, no entry
//!    point, no imports) into a scratch folder and loads it with `LoadLibraryW`, as the game loads
//!    the real one. So `GetModuleHandleW("vulkan-1.dll")`, which `wrap_gpa` compares the module
//!    with, gives that DLL's handle;
//! 4. calls `GetProcAddress` THROUGH THE IMPORT SLOT, as the game's `call [rip+slot]` does, and
//!    then every pointer the chain hands back, with made-up handles, checking at each step what
//!    came back, which real (fake) function was called and with which arguments, and the statics.
//!
//! The parent checks that the child passed and what it wrote to the log.

use std::ffi::{c_char, c_void, CStr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use ash::vk::{self, Handle};
use enshrouded_headroom::config::{Priority, TextureBudget};
use enshrouded_headroom::frame_timer::FRAMES;
use enshrouded_headroom::vk::{
    device_proc, game_device, types, wrap_create_device, wrap_create_swapchain, wrap_gdpa,
    wrap_gipa, wrap_gpa, wrap_present, GameDevice, TextureSettings, GAME_INSTANCE,
    REAL_CREATE_DEVICE, REAL_CREATE_SWAPCHAIN, REAL_GDPA, REAL_GET_MEMORY_PROPERTIES,
    REAL_GET_PROPERTIES, REAL_GIPA, REAL_GPA, REAL_PRESENT, WRAPPER_ENGAGED,
};
use windows_sys::core::PCSTR;
use windows_sys::Win32::Foundation::{FreeLibrary, FARPROC, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};

/// Names the folder the child works in. Unset in the parent.
const CHILD_ENV: &str = "ENSHROUDED_HEADROOM_VK_CHAIN_DIR";
const TEST_NAME: &str = "the_wrapper_chain_works_end_to_end";

type GpaFn = unsafe extern "system" fn(HMODULE, PCSTR) -> FARPROC;

// ---- made-up handles -------------------------------------------------------------------------

const INSTANCE: u64 = 0x1111_0000;
const PHYS: u64 = 0x2222_0000;
const PHYS_2: u64 = 0x2222_1000;
const DEVICE: u64 = 0x3333_0000;
const DEVICE_2: u64 = 0x3333_1000;
const QUEUE: u64 = 0x4444_0000;
const SWAPCHAIN: u64 = 0x5555_0000;

// ---- the fake Vulkan ---------------------------------------------------------------------------

/// Every call to a fake, in order, with its arguments.
static CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// kernel32's `GetProcAddress`: what the slot held before the fake went in.
static KERNEL_GPA: AtomicUsize = AtomicUsize::new(0);
/// The fake vulkan-1's module handle.
static FAKE_VK: AtomicUsize = AtomicUsize::new(0);
/// How many devices the fake has created.
static DEVICES: AtomicUsize = AtomicUsize::new(0);

fn note(call: String) {
    CALLS.lock().unwrap().push(call);
}

fn take_calls() -> Vec<String> {
    std::mem::take(&mut *CALLS.lock().unwrap())
}

/// The name a fake was asked for.
///
/// # Safety
///
/// `name` is a NUL-terminated string (the test passes only names to the fakes).
unsafe fn name_of(name: *const c_char) -> String {
    // SAFETY: the contract above.
    unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

// Each fake below is `unsafe extern "system"` to have the exact type of the function it stands for;
// its `# Safety` is that function's contract, as far as the fake relies on it.

/// A function the fake hands out for names nobody wraps. Never called.
///
/// # Safety
///
/// None needed: it does nothing.
unsafe extern "system" fn other_vk() {}

/// The same, as a `FARPROC`.
///
/// # Safety
///
/// None needed: it does nothing.
unsafe extern "system" fn other_farproc() -> isize {
    0
}

fn addr(f: *const ()) -> usize {
    f as usize
}

/// The fake `GetProcAddress`: answers for the fake vulkan-1 (and a null module), forwards the
/// rest to kernel32.
///
/// # Safety
///
/// `GetProcAddress`'s contract; for the fake vulkan-1 and a null module, `name` is a string.
unsafe extern "system" fn fake_gpa(module: HMODULE, name: PCSTR) -> FARPROC {
    let fake_vk = FAKE_VK.load(Ordering::Acquire);
    if module.is_null() || (fake_vk != 0 && module as usize == fake_vk) {
        if name as usize <= 0xFFFF {
            // An ordinal: noted by number, never read, and found.
            note(format!("gpa({:#x}, #{})", module as usize, name as usize));
            // SAFETY: a function address as the Option<fn> it is.
            return unsafe {
                std::mem::transmute::<usize, FARPROC>(addr(other_farproc as *const ()))
            };
        }
        // SAFETY: not an ordinal, and the test passes only names for these modules.
        let n = unsafe { name_of(name.cast()) };
        note(format!("gpa({:#x}, {n})", module as usize));
        let out = if n == "vkGetInstanceProcAddr" {
            addr(fake_gipa as *const ())
        } else {
            addr(other_farproc as *const ())
        };
        // SAFETY: a function address as the Option<fn> it is.
        return unsafe { std::mem::transmute::<usize, FARPROC>(out) };
    }
    // Everything else (the test's kernel32 lookups, and the Rust runtime's own) goes to kernel32.
    // SAFETY: KERNEL_GPA is kernel32's GetProcAddress, set before this fake went into the slot.
    let real = unsafe { std::mem::transmute::<usize, GpaFn>(KERNEL_GPA.load(Ordering::Acquire)) };
    // SAFETY: the caller's arguments, passed on.
    unsafe { real(module, name) }
}

fn to_void(addr: usize) -> vk::PFN_vkVoidFunction {
    // SAFETY: 0 or a function address, as the Option<fn> it is.
    unsafe { std::mem::transmute::<usize, vk::PFN_vkVoidFunction>(addr) }
}

/// The fake `vkGetInstanceProcAddr`.
///
/// # Safety
///
/// `name` is a NUL-terminated string.
unsafe extern "system" fn fake_gipa(
    instance: vk::Instance,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    // SAFETY: the chain passes the test's names on unchanged.
    let n = unsafe { name_of(name) };
    note(format!("gipa({:#x}, {n})", instance.as_raw()));
    to_void(match n.as_str() {
        "vkGetDeviceProcAddr" => addr(fake_gdpa as *const ()),
        "vkCreateDevice" => addr(fake_create_device as *const ()),
        "vkGetPhysicalDeviceMemoryProperties" => addr(fake_memory_properties as *const ()),
        "vkGetPhysicalDeviceProperties" => addr(fake_properties as *const ()),
        _ => addr(other_vk as *const ()),
    })
}

/// The fake `vkGetDeviceProcAddr`.
///
/// # Safety
///
/// `name` is a NUL-terminated string.
unsafe extern "system" fn fake_gdpa(
    device: vk::Device,
    name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    // SAFETY: the chain passes the test's names on unchanged.
    let n = unsafe { name_of(name) };
    note(format!("gdpa({:#x}, {n})", device.as_raw()));
    to_void(match (n.as_str(), device.as_raw()) {
        ("vkQueuePresentKHR", DEVICE) => addr(fake_present as *const ()),
        // The second device is on "another driver": its present is another function.
        ("vkQueuePresentKHR", _) => addr(fake_present_2 as *const ()),
        ("vkCreateSwapchainKHR", _) => addr(fake_create_swapchain as *const ()),
        // As the Khronos loader does: asked at device level, vkGetDeviceProcAddr gives the top of
        // the device's chain, not the trampoline vkGetInstanceProcAddr gave.
        ("vkGetDeviceProcAddr", _) => addr(fake_gdpa_top as *const ()),
        _ => addr(other_vk as *const ()),
    })
}

/// The top-of-chain `vkGetDeviceProcAddr` a device-level lookup gives. Never called.
///
/// # Safety
///
/// None needed: it reads neither argument.
unsafe extern "system" fn fake_gdpa_top(_: vk::Device, _: *const c_char) -> vk::PFN_vkVoidFunction {
    None
}

/// The fake `vkCreateDevice`: makes `DEVICE`, then `DEVICE_2`.
///
/// # Safety
///
/// `device` is valid to write a handle to; the other pointers are not read.
unsafe extern "system" fn fake_create_device(
    phys: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    device: *mut vk::Device,
) -> vk::Result {
    note(format!(
        "create_device({:#x}, {:#x}, {:#x}, {:#x})",
        phys.as_raw(),
        info as usize,
        allocator as usize,
        device as usize
    ));
    let made = if DEVICES.fetch_add(1, Ordering::Relaxed) == 0 {
        DEVICE
    } else {
        DEVICE_2
    };
    // SAFETY: the test passes a valid place for the new handle.
    unsafe { device.write(vk::Device::from_raw(made)) };
    vk::Result::SUCCESS
}

/// The fake `vkGetPhysicalDeviceMemoryProperties`: a 16 GiB device-local heap.
///
/// # Safety
///
/// `out` is a valid, writable struct.
unsafe extern "system" fn fake_memory_properties(
    phys: vk::PhysicalDevice,
    out: *mut vk::PhysicalDeviceMemoryProperties,
) {
    note(format!("memory_properties({:#x})", phys.as_raw()));
    // SAFETY: the chain passes a valid struct to fill.
    let out = unsafe { &mut *out };
    out.memory_heap_count = 2;
    out.memory_heaps[0] = vk::MemoryHeap {
        size: 32 << 30,
        flags: vk::MemoryHeapFlags::empty(),
    };
    out.memory_heaps[1] = vk::MemoryHeap {
        size: 16 << 30,
        flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
    };
}

/// The fake `vkGetPhysicalDeviceProperties`: a discrete GPU named "Fake GPU".
///
/// # Safety
///
/// `out` is a valid, writable struct.
unsafe extern "system" fn fake_properties(
    phys: vk::PhysicalDevice,
    out: *mut vk::PhysicalDeviceProperties,
) {
    note(format!("properties({:#x})", phys.as_raw()));
    // SAFETY: the chain passes a valid struct to fill.
    let out = unsafe { &mut *out };
    out.device_type = vk::PhysicalDeviceType::DISCRETE_GPU;
    for (dst, &src) in out.device_name.iter_mut().zip(b"Fake GPU\0") {
        *dst = src as c_char;
    }
}

/// The fake `vkCreateSwapchainKHR`: makes `SWAPCHAIN`.
///
/// # Safety
///
/// `swapchain` is valid to write a handle to; the other pointers are not read.
unsafe extern "system" fn fake_create_swapchain(
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    swapchain: *mut vk::SwapchainKHR,
) -> vk::Result {
    note(format!(
        "create_swapchain({:#x}, {:#x}, {:#x}, {:#x})",
        device.as_raw(),
        info as usize,
        allocator as usize,
        swapchain as usize
    ));
    // SAFETY: the test passes a valid place for the new handle.
    unsafe { swapchain.write(vk::SwapchainKHR::from_raw(SWAPCHAIN)) };
    vk::Result::SUCCESS
}

/// The fake `vkQueuePresentKHR` of the game's device.
///
/// # Safety
///
/// None needed: it reads neither argument.
unsafe extern "system" fn fake_present(
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    note(format!(
        "present({:#x}, {:#x})",
        queue.as_raw(),
        info as usize
    ));
    // Not SUCCESS, so the test sees that the real result is what comes back.
    vk::Result::SUBOPTIMAL_KHR
}

/// The second device's present: another driver's function. Never called.
///
/// # Safety
///
/// None needed: it reads neither argument.
unsafe extern "system" fn fake_present_2(
    _: vk::Queue,
    _: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    vk::Result::SUCCESS
}

// ---- a DLL named vulkan-1.dll ------------------------------------------------------------------

/// The smallest PE32+ DLL the Windows loader maps: headers, one section holding an empty base
/// relocation block (so it can be moved off its preferred base), no imports, no exports, no
/// entry point. Loading it runs no code.
fn empty_dll() -> Vec<u8> {
    fn put(b: &mut [u8], off: usize, bytes: &[u8]) {
        b[off..off + bytes.len()].copy_from_slice(bytes);
    }
    let mut b = vec![0u8; 0x400];
    put(&mut b, 0, b"MZ");
    put(&mut b, 0x3c, &0x40u32.to_le_bytes()); // e_lfanew
    put(&mut b, 0x40, b"PE\0\0");
    // COFF header: AMD64, 1 section, optional header 0xf0 bytes, EXECUTABLE_IMAGE |
    // LARGE_ADDRESS_AWARE | DLL.
    put(&mut b, 0x44, &0x8664u16.to_le_bytes());
    put(&mut b, 0x46, &1u16.to_le_bytes());
    put(&mut b, 0x54, &0xf0u16.to_le_bytes());
    put(&mut b, 0x56, &0x2022u16.to_le_bytes());
    // Optional header (PE32+).
    let o = 0x58;
    put(&mut b, o, &0x20bu16.to_le_bytes()); // Magic
    put(&mut b, o + 8, &0x200u32.to_le_bytes()); // SizeOfInitializedData
    put(&mut b, o + 20, &0x1000u32.to_le_bytes()); // BaseOfCode
    put(&mut b, o + 24, &0x1_8000_0000u64.to_le_bytes()); // ImageBase
    put(&mut b, o + 32, &0x1000u32.to_le_bytes()); // SectionAlignment
    put(&mut b, o + 36, &0x200u32.to_le_bytes()); // FileAlignment
    put(&mut b, o + 40, &6u16.to_le_bytes()); // MajorOperatingSystemVersion
    put(&mut b, o + 48, &6u16.to_le_bytes()); // MajorSubsystemVersion
    put(&mut b, o + 56, &0x2000u32.to_le_bytes()); // SizeOfImage
    put(&mut b, o + 60, &0x200u32.to_le_bytes()); // SizeOfHeaders
    put(&mut b, o + 68, &2u16.to_le_bytes()); // Subsystem: Windows GUI
    put(&mut b, o + 70, &0x0160u16.to_le_bytes()); // HIGH_ENTROPY_VA | DYNAMIC_BASE | NX_COMPAT
    put(&mut b, o + 72, &0x10_0000u64.to_le_bytes()); // SizeOfStackReserve
    put(&mut b, o + 80, &0x1000u64.to_le_bytes()); // SizeOfStackCommit
    put(&mut b, o + 88, &0x10_0000u64.to_le_bytes()); // SizeOfHeapReserve
    put(&mut b, o + 96, &0x1000u64.to_le_bytes()); // SizeOfHeapCommit
    put(&mut b, o + 108, &16u32.to_le_bytes()); // NumberOfRvaAndSizes
    put(&mut b, o + 112 + 5 * 8, &0x1000u32.to_le_bytes()); // base relocations: RVA
    put(&mut b, o + 112 + 5 * 8 + 4, &12u32.to_le_bytes()); //                    size
                                                            // Section header: .reloc at RVA 0x1000, file offset 0x200; INITIALIZED_DATA | DISCARDABLE |
                                                            // READ.
    let s = o + 0xf0;
    put(&mut b, s, b".reloc\0\0");
    put(&mut b, s + 8, &12u32.to_le_bytes()); // VirtualSize
    put(&mut b, s + 12, &0x1000u32.to_le_bytes()); // VirtualAddress
    put(&mut b, s + 16, &0x200u32.to_le_bytes()); // SizeOfRawData
    put(&mut b, s + 20, &0x200u32.to_le_bytes()); // PointerToRawData
    put(&mut b, s + 36, &0x4200_0040u32.to_le_bytes());
    // The relocation block: page 0x1000, 12 bytes, two IMAGE_REL_BASED_ABSOLUTE (padding) entries.
    put(&mut b, 0x200, &0x1000u32.to_le_bytes());
    put(&mut b, 0x204, &12u32.to_le_bytes());
    b
}

fn wide(s: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.as_os_str().encode_wide().chain(Some(0)).collect()
}

// ---- the scenario --------------------------------------------------------------------------------

/// Call `GetProcAddress` the way the game does: through the import slot.
///
/// # Safety
///
/// `slot` is this exe's GetProcAddress import slot; `name` is NUL-terminated.
unsafe fn gpa_via_slot(slot: *mut usize, module: HMODULE, name: &CStr) -> usize {
    // SAFETY: the slot holds a function of GetProcAddress's type (contract).
    let gpa = unsafe { std::mem::transmute::<usize, GpaFn>(slot.read_volatile()) };
    // SAFETY: valid GetProcAddress arguments.
    let f = unsafe { gpa(module, name.as_ptr().cast()) };
    f.map_or(0, |f| f as usize)
}

fn void_addr(f: vk::PFN_vkVoidFunction) -> usize {
    f.map_or(0, |f| f as usize)
}

fn load(slot: &AtomicUsize) -> usize {
    slot.load(Ordering::Acquire)
}

fn scenario(dir: &Path) {
    enshrouded_headroom::log::init(dir, log::LevelFilter::Debug);

    // SAFETY: literal module names; kernel32 is loaded.
    let kernel32 = unsafe { GetModuleHandleW(windows_sys::w!("kernel32.dll")) };
    // SAFETY: a live module and a NUL-terminated name.
    let sleep = unsafe { GetProcAddress(kernel32, c"Sleep".as_ptr().cast()) }.map(|f| f as usize);

    // 1. The fake goes into the slot, where kernel32's GetProcAddress was.
    let module = enshrouded_headroom::scan::main_module();
    // SAFETY: start-up-like: this thread is the only one using the slot.
    let slot =
        unsafe { enshrouded_headroom::iat::find_slot(&module, "kernel32.dll", "GetProcAddress") }
            .expect("this exe imports GetProcAddress from kernel32");
    // SAFETY: as above; the fake has GetProcAddress's type.
    let kernel_gpa =
        unsafe { enshrouded_headroom::iat::swap(slot, fake_gpa as *const () as usize) };
    assert_ne!(kernel_gpa, 0, "the fake must go into the slot");
    KERNEL_GPA.store(kernel_gpa, Ordering::Release);

    // 2. The start-up step, as entry::start runs it.
    enshrouded_headroom::vk::init(
        TextureSettings {
            mode: TextureBudget::Auto,
            reserve_gib: 11.0,
        },
        Priority::Normal,
    );
    assert_eq!(
        load(&REAL_GPA),
        fake_gpa as *const () as usize,
        "REAL_GPA is what the slot held"
    );
    // SAFETY: an aligned import slot of this exe.
    assert_eq!(
        unsafe { slot.read_volatile() },
        wrap_gpa as *const () as usize
    );
    assert!(!WRAPPER_ENGAGED.load(Ordering::Acquire));

    // Other lookups are not changed: a kernel32 name and a kernel32 ordinal.
    // SAFETY: the slot and valid arguments.
    unsafe {
        assert_eq!(gpa_via_slot(slot, kernel32, c"Sleep"), sleep.unwrap());
        let ordinal: PCSTR = 1usize as PCSTR;
        let gpa = std::mem::transmute::<usize, GpaFn>(slot.read_volatile());
        let direct = std::mem::transmute::<usize, GpaFn>(kernel_gpa);
        assert_eq!(
            gpa(kernel32, ordinal).map(|f| f as usize),
            direct(kernel32, ordinal).map(|f| f as usize)
        );
    }

    // A null module is never compared with vulkan-1 (which is not loaded yet, so its handle is
    // null too): the fake's answer comes back unwrapped.
    // SAFETY: the slot and valid arguments.
    let got = unsafe { gpa_via_slot(slot, std::ptr::null_mut(), c"vkGetInstanceProcAddr") };
    assert_eq!(got, fake_gipa as *const () as usize);
    assert_eq!(load(&REAL_GIPA), 0);
    assert!(!WRAPPER_ENGAGED.load(Ordering::Acquire));
    assert_eq!(take_calls(), ["gpa(0x0, vkGetInstanceProcAddr)"]);

    // 3. The game loads vulkan-1.dll.
    let dll = dir.join("vulkan-1.dll");
    std::fs::write(&dll, empty_dll()).unwrap();
    // SAFETY: a NUL-terminated path to a DLL that has no code to run.
    let vulkan = unsafe { LoadLibraryW(wide(&dll).as_ptr()) };
    assert!(
        !vulkan.is_null(),
        "LoadLibraryW: {}",
        std::io::Error::last_os_error()
    );
    FAKE_VK.store(vulkan as usize, Ordering::Release);
    // SAFETY: a literal module name.
    let by_name = unsafe { GetModuleHandleW(windows_sys::w!("vulkan-1.dll")) };
    assert_eq!(by_name, vulkan, "the handle wrap_gpa compares with");

    // 4. GetProcAddress(vulkan-1, "vkGetInstanceProcAddr"), through the slot.
    // SAFETY: the slot and valid arguments.
    let gipa = unsafe { gpa_via_slot(slot, vulkan, c"vkGetInstanceProcAddr") };
    assert_eq!(
        gipa, wrap_gipa as *const () as usize,
        "the game gets wrap_gipa"
    );
    assert!(WRAPPER_ENGAGED.load(Ordering::Acquire));
    assert_eq!(load(&REAL_GIPA), fake_gipa as *const () as usize);
    assert_eq!(
        take_calls(),
        [format!(
            "gpa({:#x}, vkGetInstanceProcAddr)",
            vulkan as usize
        )]
    );
    // Another vulkan-1 name passes through.
    // SAFETY: as above.
    let got = unsafe { gpa_via_slot(slot, vulkan, c"vkCreateInstance") };
    assert_eq!(got, other_farproc as *const () as usize);
    take_calls();
    // An ordinal on vulkan-1 itself, which the fake finds: the module matches, so only the
    // ordinal check keeps wrap_gpa from reading address 5 (never mapped: a read would kill this
    // process). It comes back as the fake gave it.
    // SAFETY: the slot holds a GetProcAddress-typed function; an ordinal is a valid name.
    let got = unsafe {
        let gpa = std::mem::transmute::<usize, GpaFn>(slot.read_volatile());
        gpa(vulkan, 5usize as PCSTR).map_or(0, |f| f as usize)
    };
    assert_eq!(got, other_farproc as *const () as usize);
    assert_eq!(take_calls(), [format!("gpa({:#x}, #5)", vulkan as usize)]);

    // SAFETY: what the chain handed out has vkGetInstanceProcAddr's type.
    let gipa = unsafe { std::mem::transmute::<usize, vk::PFN_vkGetInstanceProcAddr>(gipa) };
    let instance = vk::Instance::from_raw(INSTANCE);
    // SAFETY (the calls below): made-up handles the fakes never dereference, and literal names.
    unsafe {
        // A global command with no instance: passed through, no instance recorded.
        let got = gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr());
        assert_eq!(void_addr(got), other_vk as *const () as usize);
        assert_eq!(load(&GAME_INSTANCE), 0);

        let gdpa = gipa(instance, c"vkGetDeviceProcAddr".as_ptr());
        assert_eq!(void_addr(gdpa), wrap_gdpa as *const () as usize);
        assert_eq!(load(&GAME_INSTANCE), INSTANCE as usize);
        assert_eq!(load(&REAL_GDPA), fake_gdpa as *const () as usize);

        let create_device = gipa(instance, c"vkCreateDevice".as_ptr());
        assert_eq!(
            void_addr(create_device),
            wrap_create_device as *const () as usize
        );
        assert_eq!(
            load(&REAL_CREATE_DEVICE),
            fake_create_device as *const () as usize
        );
        assert_eq!(
            load(&REAL_GET_MEMORY_PROPERTIES),
            fake_memory_properties as *const () as usize
        );
        assert_eq!(
            load(&REAL_GET_PROPERTIES),
            fake_properties as *const () as usize
        );

        let got = gipa(instance, c"vkEnumeratePhysicalDevices".as_ptr());
        assert_eq!(void_addr(got), other_vk as *const () as usize);
        assert_eq!(
            take_calls(),
            [
                "gipa(0x0, vkCreateInstance)",
                "gipa(0x11110000, vkGetDeviceProcAddr)",
                "gipa(0x11110000, vkCreateDevice)",
                "gipa(0x11110000, vkGetPhysicalDeviceMemoryProperties)",
                "gipa(0x11110000, vkGetPhysicalDeviceProperties)",
                "gipa(0x11110000, vkEnumeratePhysicalDevices)",
            ]
        );

        // vkCreateDevice: the real one gets the game's own pointers.
        let create_device =
            std::mem::transmute::<vk::PFN_vkVoidFunction, vk::PFN_vkCreateDevice>(create_device);
        let names = [c"VK_KHR_swapchain".as_ptr()];
        let info = vk::DeviceCreateInfo::default().enabled_extension_names(&names);
        let allocator = vk::AllocationCallbacks::default();
        let mut device = vk::Device::null();
        let result = create_device(
            vk::PhysicalDevice::from_raw(PHYS),
            &info,
            &allocator,
            &mut device,
        );
        assert_eq!(result, vk::Result::SUCCESS);
        assert_eq!(device.as_raw(), DEVICE);
        let first = GameDevice {
            device: DEVICE as usize,
            phys: PHYS as usize,
        };
        assert_eq!(game_device(), Some(first), "device and phys, together");
        assert_eq!(
            take_calls(),
            [
                format!(
                    "create_device(0x22220000, {:#x}, {:#x}, {:#x})",
                    &info as *const _ as usize,
                    &allocator as *const _ as usize,
                    &mut device as *mut _ as usize
                ),
                "memory_properties(0x22220000)".to_owned(),
                "properties(0x22220000)".to_owned(),
            ]
        );
        // The extension list went through untouched.
        assert_eq!(info.enabled_extension_count, 1);
        assert_eq!(info.pp_enabled_extension_names, names.as_ptr());

        // A second device: created, but the first one stays the game's, and no second query.
        let mut device_2 = vk::Device::null();
        let result = create_device(
            vk::PhysicalDevice::from_raw(PHYS_2),
            &info,
            std::ptr::null(),
            &mut device_2,
        );
        assert_eq!(result, vk::Result::SUCCESS);
        assert_eq!(device_2.as_raw(), DEVICE_2);
        assert_eq!(
            game_device(),
            Some(first),
            "the first device stays the game's"
        );
        assert_eq!(take_calls().len(), 1, "only the create call");

        // vkGetDeviceProcAddr on the game's device.
        let gdpa = std::mem::transmute::<vk::PFN_vkVoidFunction, vk::PFN_vkGetDeviceProcAddr>(gdpa);
        let device = vk::Device::from_raw(DEVICE);
        let present = gdpa(device, c"vkQueuePresentKHR".as_ptr());
        assert_eq!(void_addr(present), wrap_present as *const () as usize);
        assert_eq!(load(&REAL_PRESENT), fake_present as *const () as usize);
        let create_swapchain = gdpa(device, c"vkCreateSwapchainKHR".as_ptr());
        assert_eq!(
            void_addr(create_swapchain),
            wrap_create_swapchain as *const () as usize
        );
        assert_eq!(
            load(&REAL_CREATE_SWAPCHAIN),
            fake_create_swapchain as *const () as usize
        );
        // No submit wrappers (ruling G10).
        assert_eq!(
            void_addr(gdpa(device, c"vkQueueSubmit".as_ptr())),
            other_vk as *const () as usize
        );
        assert_eq!(
            void_addr(gdpa(device, c"vkQueueSubmit2".as_ptr())),
            other_vk as *const () as usize
        );
        // T9-1: vkGetDeviceProcAddr asked at device level gives the top of the chain, which is not
        // the trampoline in REAL_GDPA; the trampoline serves the game's device, so it is wrapped.
        let again = gdpa(device, c"vkGetDeviceProcAddr".as_ptr());
        assert_eq!(void_addr(again), wrap_gdpa as *const () as usize);
        assert_eq!(load(&REAL_GDPA), fake_gdpa as *const () as usize);
        // The other device's present is another driver's function: handed back unwrapped (and
        // logged once, however often it is asked for).
        for _ in 0..2 {
            let got = gdpa(
                vk::Device::from_raw(DEVICE_2),
                c"vkQueuePresentKHR".as_ptr(),
            );
            assert_eq!(void_addr(got), fake_present_2 as *const () as usize);
        }
        assert_eq!(load(&REAL_PRESENT), fake_present as *const () as usize);
        // Its vkGetDeviceProcAddr is not the one in the slot either: unwrapped, logged once.
        let got = gdpa(
            vk::Device::from_raw(DEVICE_2),
            c"vkGetDeviceProcAddr".as_ptr(),
        );
        assert_eq!(void_addr(got), fake_gdpa_top as *const () as usize);
        // device_proc goes to the real vkGetDeviceProcAddr (Task 16's entry points).
        assert_eq!(
            device_proc(DEVICE as usize, c"vkLatencySleepNV"),
            Some(other_vk as *const () as usize)
        );
        assert_eq!(
            take_calls(),
            [
                "gdpa(0x33330000, vkQueuePresentKHR)",
                "gdpa(0x33330000, vkCreateSwapchainKHR)",
                "gdpa(0x33330000, vkQueueSubmit)",
                "gdpa(0x33330000, vkQueueSubmit2)",
                "gdpa(0x33330000, vkGetDeviceProcAddr)",
                "gdpa(0x33331000, vkQueuePresentKHR)",
                "gdpa(0x33331000, vkQueuePresentKHR)",
                "gdpa(0x33331000, vkGetDeviceProcAddr)",
                "gdpa(0x33330000, vkLatencySleepNV)",
            ]
        );

        // vkCreateSwapchainKHR: passed through with the game's pointers.
        let create_swapchain = std::mem::transmute::<
            vk::PFN_vkVoidFunction,
            vk::PFN_vkCreateSwapchainKHR,
        >(create_swapchain);
        let sc_info = vk::SwapchainCreateInfoKHR::default();
        let mut swapchain = vk::SwapchainKHR::null();
        let result = create_swapchain(device, &sc_info, &allocator, &mut swapchain);
        assert_eq!(result, vk::Result::SUCCESS);
        assert_eq!(swapchain.as_raw(), SWAPCHAIN);
        assert_eq!(
            take_calls(),
            [format!(
                "create_swapchain(0x33330000, {:#x}, {:#x}, {:#x})",
                &sc_info as *const _ as usize,
                &allocator as *const _ as usize,
                &mut swapchain as *mut _ as usize
            )]
        );

        // vkQueuePresentKHR, five frames, with the game's VkPresentId2KHR chained on.
        let present =
            std::mem::transmute::<vk::PFN_vkVoidFunction, vk::PFN_vkQueuePresentKHR>(present);
        let ids = [42u64];
        let id2 = types::PresentId2KHR {
            s_type: types::STRUCTURE_TYPE_PRESENT_ID_2_KHR,
            p_next: std::ptr::null(),
            swapchain_count: 1,
            p_present_ids: ids.as_ptr(),
        };
        let swapchains = [swapchain];
        let mut present_info = vk::PresentInfoKHR::default().swapchains(&swapchains);
        present_info.p_next = &id2 as *const types::PresentId2KHR as *const c_void;
        let bytes = |p: *const u8, n: usize| std::slice::from_raw_parts(p, n).to_vec();
        let info_before = bytes(
            (&present_info as *const vk::PresentInfoKHR).cast(),
            size_of_val(&present_info),
        );
        let id2_before = bytes(
            (&id2 as *const types::PresentId2KHR).cast(),
            size_of_val(&id2),
        );

        let mut stamps = Vec::new();
        let seq = FRAMES.read_since(0, &mut stamps);
        assert_eq!(
            (seq, stamps.len()),
            (0, 0),
            "no frame before the first present"
        );
        for _ in 0..5 {
            let result = present(vk::Queue::from_raw(QUEUE), &present_info);
            assert_eq!(
                result,
                vk::Result::SUBOPTIMAL_KHR,
                "the real result comes back"
            );
        }
        let want = format!(
            "present(0x44440000, {:#x})",
            &present_info as *const _ as usize
        );
        assert_eq!(take_calls(), vec![want; 5]);
        let seq = FRAMES.read_since(0, &mut stamps);
        assert_eq!(seq, 5, "one stamp per present");
        assert_eq!(stamps.len(), 5);
        assert!(
            stamps[0] > 0 && stamps.windows(2).all(|w| w[0] <= w[1]),
            "{stamps:?}"
        );
        // Ruling G12: the present info and its pNext chain are exactly as the game made them.
        assert_eq!(
            bytes(
                (&present_info as *const vk::PresentInfoKHR).cast(),
                size_of_val(&present_info)
            ),
            info_before
        );
        assert_eq!(
            bytes(
                (&id2 as *const types::PresentId2KHR).cast(),
                size_of_val(&id2)
            ),
            id2_before
        );
    }

    // SAFETY: nothing of the DLL is in use.
    unsafe { FreeLibrary(vulkan) };
}

fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir()
        .join("enshrouded-headroom-tests")
        .join(format!("vk-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_wrapper_chain_works_end_to_end() {
    if let Some(dir) = std::env::var_os(CHILD_ENV) {
        scenario(Path::new(&dir));
        return;
    }

    let dir = scratch_dir();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, &dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = std::fs::read_to_string(dir.join(enshrouded_headroom::log::LOG_FILE_NAME))
        .unwrap_or_else(|e| format!("<no log: {e}>"));
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child failed: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}\nlog:\n{log}",
        output.status
    );
    println!("child log:\n{log}");

    assert_eq!(log.matches("vk wrapper installed").count(), 1, "{log}");
    let engaged: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("wrapper chain engaged (device created)"))
        .collect();
    assert_eq!(
        engaged.len(),
        1,
        "logged once, for the first device only:\n{log}"
    );
    assert!(
        engaged[0].contains("GPU \"Fake GPU\", discrete, largest device-local heap 16.0 GiB"),
        "{log}"
    );
    assert_eq!(
        log.matches("priority: unchanged (setting)").count(),
        1,
        "priority is applied for the first device only:\n{log}"
    );
    // No texture-budget line: this exe has no texture table, so nothing was prepared.
    assert!(!log.contains("texture budget"), "{log}");
    // T9-2: each name handed back unwrapped is logged once, with both devices; nothing else is.
    for name in ["vkQueuePresentKHR", "vkGetDeviceProcAddr"] {
        let line = format!(
            "vk: {name} for device 0x33331000 handed back unwrapped: the game device is 0x33330000"
        );
        assert_eq!(log.matches(&line).count(), 1, "{line}\n{log}");
    }
    assert_eq!(log.matches("handed back unwrapped").count(), 2, "{log}");
    assert!(
        !log.contains("VkWrap disabled"),
        "no wrapper panicked:\n{log}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
