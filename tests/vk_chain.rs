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
        } else if n == "vkCreateInstance" {
            addr(fake_create_instance as *const ())
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
        "vkCreateInstance" => addr(fake_create_instance as *const ()),
        "vkGetDeviceProcAddr" => addr(fake_gdpa as *const ()),
        "vkCreateDevice" => addr(fake_create_device as *const ()),
        "vkQueuePresentKHR" => addr(fake_present as *const ()),
        "vkCreateSwapchainKHR" => addr(fake_create_swapchain as *const ()),
        "vkEnumeratePhysicalDevices" => addr(fake_enumerate_physical as *const ()),
        "vkGetPhysicalDeviceMemoryProperties" => addr(fake_memory_properties as *const ()),
        "vkGetPhysicalDeviceProperties" => addr(fake_properties as *const ()),
        _ => addr(other_vk as *const ()),
    })
}

/// Create the fake instance through the unchanged native observer.
/// # Safety
/// The caller supplies writable instance output; remaining inputs stay borrowed by the wrapper.
unsafe extern "system" fn fake_create_instance(
    _: *const vk::InstanceCreateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Instance,
) -> vk::Result {
    let body = || {
        note("create_instance".into());
        // SAFETY: the caller supplies one live output slot.
        unsafe { out.write(vk::Instance::from_raw(INSTANCE)) };
        vk::Result::SUCCESS
    };
    enshrouded_headroom::guard::guard(enshrouded_headroom::guard::Feature::VkWrap, body, body)
}

/// Actual enumeration output establishes the fake device's native parent.
/// # Safety
/// The test passes one writable physical handle and count storage, or a count-only query.
unsafe extern "system" fn fake_enumerate_physical(
    _: vk::Instance,
    count: *mut u32,
    out: *mut vk::PhysicalDevice,
) -> vk::Result {
    let body = || {
        // SAFETY: caller provides the described storage; exactly one output is written.
        unsafe {
            if !out.is_null() {
                out.write(vk::PhysicalDevice::from_raw(PHYS));
            }
            count.write(1);
        }
        vk::Result::SUCCESS
    };
    enshrouded_headroom::guard::guard(enshrouded_headroom::guard::Feature::VkWrap, body, body)
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
    out.api_version = vk::API_VERSION_1_2;
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
    let got = unsafe { gpa_via_slot(slot, vulkan, c"vkEnumerateInstanceVersion") };
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
        // Global creation is observed, while the ordinary instance lookup slot stays unset.
        let got = gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr());
        assert_eq!(
            void_addr(got),
            enshrouded_headroom::vk::wrap_create_instance as *const () as usize
        );
        let create = std::mem::transmute::<vk::PFN_vkVoidFunction, vk::PFN_vkCreateInstance>(got);
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_2);
        let mut created = vk::Instance::null();
        assert_eq!(
            create(
                &vk::InstanceCreateInfo::default().application_info(&app),
                std::ptr::null(),
                &mut created
            ),
            vk::Result::SUCCESS
        );
        assert_eq!(created.as_raw(), instance.as_raw());
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
        assert_ne!(
            void_addr(got),
            fake_enumerate_physical as *const () as usize
        );
        let enumerate =
            std::mem::transmute::<vk::PFN_vkVoidFunction, vk::PFN_vkEnumeratePhysicalDevices>(got);
        let mut count = 1;
        let mut physical = vk::PhysicalDevice::null();
        assert_eq!(
            enumerate(instance, &mut count, &mut physical),
            vk::Result::SUCCESS
        );
        assert_eq!(physical.as_raw(), PHYS);
        assert_eq!(
            take_calls(),
            [
                "gipa(0x0, vkCreateInstance)",
                "create_instance",
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
                "gipa(0x11110000, vkDestroyDevice)".to_owned(),
                "gipa(0x11110000, vkGetPhysicalDeviceMemoryProperties)".to_owned(),
                "gipa(0x11110000, vkGetPhysicalDeviceProperties)".to_owned(),
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
                "gipa(0x11110000, vkGetDeviceProcAddr)",
                "gdpa(0x33331000, vkQueuePresentKHR)",
                "gipa(0x11110000, vkQueuePresentKHR)",
                "gdpa(0x33331000, vkQueuePresentKHR)",
                "gipa(0x11110000, vkQueuePresentKHR)",
                "gdpa(0x33331000, vkGetDeviceProcAddr)",
                "gipa(0x11110000, vkGetDeviceProcAddr)",
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
        let indices = [0u32];
        let mut present_info = vk::PresentInfoKHR::default()
            .swapchains(&swapchains)
            .image_indices(&indices);
        present_info.p_next = &id2 as *const types::PresentId2KHR as *const c_void;
        // Compare initialized fields; repr(C) padding is not initialized Rust byte data.
        let info_fields = |info: &vk::PresentInfoKHR<'_>| {
            (
                info.s_type.as_raw(),
                info.p_next,
                info.wait_semaphore_count,
                info.p_wait_semaphores,
                info.swapchain_count,
                info.p_swapchains,
                info.p_image_indices,
                info.p_results,
            )
        };
        let id_fields = |id: &types::PresentId2KHR| {
            (
                id.s_type.as_raw(),
                id.p_next,
                id.swapchain_count,
                id.p_present_ids,
            )
        };
        let info_before = info_fields(&present_info);
        let id2_before = id_fields(&id2);
        let ids_before = ids;
        let swapchains_before = swapchains;
        let indices_before = indices;

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
        assert_eq!(info_fields(&present_info), info_before);
        assert_eq!(id_fields(&id2), id2_before);
        assert_eq!(ids, ids_before);
        assert_eq!(swapchains, swapchains_before);
        assert_eq!(indices, indices_before);
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

/// Isolated fake-driver cases exercise the real create wrappers, including exact-pointer retries.
mod reflex_driver {
    use super::*;
    use enshrouded_headroom::{
        guard::{guard, Feature},
        reflex,
    };
    static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
    static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
    static CHANGED_OK: AtomicUsize = AtomicUsize::new(0);
    static RETRY_OK: AtomicUsize = AtomicUsize::new(0);
    static A_DESTROYS: AtomicUsize = AtomicUsize::new(0);
    static B_DESTROYS: AtomicUsize = AtomicUsize::new(0);
    static WRONG_GPU_QUERY: AtomicUsize = AtomicUsize::new(0);
    static QUERY_INSTANCE: AtomicUsize = AtomicUsize::new(0);
    static ENUM_RESULT: AtomicUsize = AtomicUsize::new(0);
    static VENDOR: AtomicUsize = AtomicUsize::new(0x10de);
    static REFUSE: AtomicUsize = AtomicUsize::new(0);

    /// Physical API and instance creation facts are independent inputs to route selection.
    static PHYSICAL_API: AtomicUsize = AtomicUsize::new(vk::API_VERSION_1_2 as usize);
    /// Route cases start without newer core feature nodes; other cases retain their chain checks.
    static CAPS_CASE: AtomicUsize = AtomicUsize::new(0);
    static INSTANCE_FAILURE: AtomicUsize = AtomicUsize::new(0);
    static CORE_LOOKUPS: AtomicUsize = AtomicUsize::new(0);
    static KHR_LOOKUPS: AtomicUsize = AtomicUsize::new(0);
    static CORE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static KHR_CALLS: AtomicUsize = AtomicUsize::new(0);
    static MISSING_CORE: AtomicUsize = AtomicUsize::new(0);
    static MISSING_KHR: AtomicUsize = AtomicUsize::new(0);
    /// # Safety
    /// Inputs/output obey vkCreateInstance; successful output is the fake B instance.
    unsafe extern "system" fn create_instance(
        _: *const vk::InstanceCreateInfo<'_>,
        _: *const vk::AllocationCallbacks<'_>,
        out: *mut vk::Instance,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                if INSTANCE_FAILURE.load(Ordering::Relaxed) != 0 {
                    return vk::Result::ERROR_INITIALIZATION_FAILED;
                }
                // SAFETY: caller supplies writable output; no real Vulkan objects are used.
                unsafe { out.write(vk::Instance::from_raw(INSTANCE + 1)) };
                vk::Result::SUCCESS
            },
        )
    }
    /// Create through the real lookup observer, never inject capability metadata.
    fn observe_instance(case: &str) {
        if case == "caps-unknown" {
            return;
        }
        let requested = if matches!(case, "caps-khr" | "caps-no-route" | "caps-khr-missing") {
            vk::API_VERSION_1_0
        } else if case == "caps-core" {
            vk::API_VERSION_1_1
        } else {
            vk::API_VERSION_1_2
        };
        let khr = matches!(
            case,
            "caps-khr" | "caps-khr-missing" | "caps-low-physical-khr" | "caps-core-missing"
        );
        let app = vk::ApplicationInfo::default().api_version(requested);
        let names = [c"VK_KHR_get_physical_device_properties2".as_ptr()];
        let info = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_extension_names(if khr { &names } else { &[] });
        if case == "caps-failed-create" {
            INSTANCE_FAILURE.store(1, Ordering::Relaxed);
        }
        let mut instance = vk::Instance::null();
        // SAFETY: exact fake global entry point, initialized inputs and live output.
        let result = unsafe {
            let raw = wrap_gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr()).unwrap();
            let create =
                std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkCreateInstance>(raw);
            create(&info, std::ptr::null(), &mut instance)
        };
        if case == "caps-failed-create" {
            assert_eq!(result, vk::Result::ERROR_INITIALIZATION_FAILED);
            assert!(instance.is_null());
            // Subsequent enumeration represents a separate pre-existing, unobserved B instance.
        } else {
            assert_eq!(result, vk::Result::SUCCESS);
            assert_eq!(instance.as_raw(), INSTANCE + 1);
        }
    }
    /// # Safety
    /// Exact core alias; counter distinguishes it from the independently returned KHR pointer.
    unsafe extern "system" fn features_core(
        phys: vk::PhysicalDevice,
        out: *mut vk::PhysicalDeviceFeatures2<'_>,
    ) {
        let body = || {
            CORE_CALLS.fetch_add(1, Ordering::Relaxed);
            // SAFETY: this wrapper forwards its exact live input to the shared fake implementation.
            unsafe { features(phys, out) };
        };
        guard(Feature::VkWrap, body, body)
    }
    /// # Safety
    /// Exact KHR alias; its pointer is distinct even though its Vulkan layout matches core.
    unsafe extern "system" fn features_khr(
        phys: vk::PhysicalDevice,
        out: *mut vk::PhysicalDeviceFeatures2<'_>,
    ) {
        let body = || {
            KHR_CALLS.fetch_add(1, Ordering::Relaxed);
            // SAFETY: this wrapper forwards its exact live input to the shared fake implementation.
            unsafe { features(phys, out) };
        };
        guard(Feature::VkWrap, body, body)
    }
    /// # Safety
    /// The test passes a writable physical-properties output.
    unsafe extern "system" fn properties(
        _: vk::PhysicalDevice,
        out: *mut vk::PhysicalDeviceProperties,
    ) {
        guard(
            Feature::VkWrap,
            || (),
            || {
                // SAFETY: the query supplies this live output object.
                unsafe {
                    (*out).vendor_id = VENDOR.load(Ordering::Relaxed) as u32;
                    (*out).api_version = PHYSICAL_API.load(Ordering::Relaxed) as u32;
                }
            },
        );
    }
    /// Fake parent-specific memory query; the output belongs to the caller.
    unsafe extern "system" fn memory(
        _: vk::PhysicalDevice,
        out: *mut vk::PhysicalDeviceMemoryProperties,
    ) {
        let body = || {
            // SAFETY: caller supplies the exact writable Vulkan output type.
            unsafe { out.write(vk::PhysicalDeviceMemoryProperties::default()) };
        };
        guard(Feature::VkWrap, body, body);
    }
    /// A stale instance A properties pointer must never be used for physical device B.
    unsafe extern "system" fn wrong_properties(
        _: vk::PhysicalDevice,
        _: *mut vk::PhysicalDeviceProperties,
    ) {
        let body = || {
            WRONG_GPU_QUERY.fetch_add(1, Ordering::Relaxed);
        };
        guard(Feature::VkWrap, body, body);
    }
    /// # Safety
    /// Vulkan's extension-query contract, supplied by the wrapper under test.
    unsafe extern "system" fn extensions(
        _: vk::PhysicalDevice,
        _: *const c_char,
        count: *mut u32,
        out: *mut vk::ExtensionProperties,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                let names = [
                    c"VK_NV_low_latency2",
                    c"VK_KHR_timeline_semaphore",
                    c"VK_KHR_present_id",
                ];
                // SAFETY: query output capacity is at least the count previously advertised.
                unsafe {
                    if !out.is_null() {
                        for (index, name) in names.iter().enumerate() {
                            let mut entry = vk::ExtensionProperties {
                                spec_version: 3,
                                ..Default::default()
                            };
                            for (dst, byte) in entry
                                .extension_name
                                .iter_mut()
                                .zip(name.to_bytes_with_nul())
                            {
                                *dst = *byte as c_char;
                            }
                            out.add(index).write(entry);
                        }
                    }
                    count.write(3);
                }
                vk::Result::SUCCESS
            },
        )
    }
    /// # Safety
    /// The wrapper supplies a writable Features2 chain with the queried feature layouts.
    unsafe extern "system" fn features(
        _: vk::PhysicalDevice,
        out: *mut vk::PhysicalDeviceFeatures2<'_>,
    ) {
        guard(
            Feature::VkWrap,
            || (),
            || {
                // SAFETY: the query's output chain consists of these live feature structs.
                unsafe {
                    let mut node = (*out).p_next.cast::<vk::BaseOutStructure<'_>>();
                    while !node.is_null() {
                        match (*node).s_type {
                            vk::StructureType::PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES => {
                                (*node.cast::<vk::PhysicalDeviceTimelineSemaphoreFeatures<'_>>())
                                    .timeline_semaphore = vk::TRUE
                            }
                            vk::StructureType::PHYSICAL_DEVICE_PRESENT_ID_FEATURES_KHR => {
                                (*node.cast::<vk::PhysicalDevicePresentIdFeaturesKHR<'_>>())
                                    .present_id = vk::TRUE
                            }
                            _ => (),
                        }
                        node = (*node).p_next;
                    }
                }
            },
        );
    }
    /// Enumerate distinct physical devices for two live fake instances.
    unsafe extern "system" fn enumerate_physical(
        instance: vk::Instance,
        count: *mut u32,
        out: *mut vk::PhysicalDevice,
    ) -> vk::Result {
        let body = || {
            // SAFETY: the test provides one writable output or a count-only query.
            unsafe {
                if !out.is_null() {
                    out.write(vk::PhysicalDevice::from_raw(
                        if instance.as_raw() == INSTANCE {
                            PHYS + 1
                        } else {
                            PHYS
                        },
                    ));
                }
                count.write(1);
            }
            match ENUM_RESULT.load(Ordering::Relaxed) {
                1 => vk::Result::INCOMPLETE,
                2 => vk::Result::ERROR_UNKNOWN,
                _ => vk::Result::SUCCESS,
            }
        };
        guard(Feature::VkWrap, body, body)
    }
    /// Actual group outputs have the same parentage contract for the core and KHR aliases.
    unsafe extern "system" fn enumerate_groups(
        instance: vk::Instance,
        count: *mut u32,
        out: *mut vk::PhysicalDeviceGroupProperties<'_>,
    ) -> vk::Result {
        let body = || {
            // SAFETY: count and optional one-element initialized group storage are supplied by the test.
            unsafe {
                if !out.is_null() {
                    (*out).physical_device_count = 1;
                    (*out).physical_devices[0] =
                        vk::PhysicalDevice::from_raw(if instance.as_raw() == INSTANCE {
                            PHYS + 1
                        } else {
                            PHYS
                        });
                }
                count.write(1);
            }
            vk::Result::SUCCESS
        };
        guard(Feature::VkWrap, body, body)
    }
    /// B's native destructor records every actual invocation, including after mod disable.
    unsafe extern "system" fn destroy_device(_: vk::Device, _: *const vk::AllocationCallbacks<'_>) {
        let body = || {
            B_DESTROYS.fetch_add(1, Ordering::Relaxed);
        };
        guard(Feature::VkWrap, body, body);
    }
    /// A's instance-specific destructor must never receive B's child device.
    unsafe extern "system" fn other_destroy_device(
        _: vk::Device,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        let body = || {
            A_DESTROYS.fetch_add(1, Ordering::Relaxed);
        };
        guard(Feature::VkWrap, body, body);
    }
    /// B's native swapchain destructor records actual invocation.
    unsafe extern "system" fn destroy_swapchain(
        _: vk::Device,
        _: vk::SwapchainKHR,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        let body = || {
            B_DESTROYS.fetch_add(1, Ordering::Relaxed);
        };
        guard(Feature::VkWrap, body, body);
    }
    /// A's instance-specific swapchain destructor must never receive B's child device.
    unsafe extern "system" fn other_destroy_swapchain(
        _: vk::Device,
        _: vk::SwapchainKHR,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        let body = || {
            A_DESTROYS.fetch_add(1, Ordering::Relaxed);
        };
        guard(Feature::VkWrap, body, body);
    }
    /// B's device-level dispatch returns its own destructors, regardless of the mod's flag.
    unsafe extern "system" fn gdpa(_: vk::Device, name: *const c_char) -> vk::PFN_vkVoidFunction {
        let body = || {
            // SAFETY: every test lookup supplies a static terminated name.
            to_void(match unsafe { CStr::from_ptr(name) }.to_bytes() {
                b"vkDestroyDevice" => destroy_device as *const () as usize,
                b"vkDestroySwapchainKHR" => destroy_swapchain as *const () as usize,
                _ if PRESENT_CASE.load(Ordering::Relaxed) != 0 => present_entry(name),
                _ => 0,
            })
        };
        guard(Feature::VkWrap, body, body)
    }
    /// The fake owns no native instance resources.
    unsafe extern "system" fn destroy_instance(
        _: vk::Instance,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        guard(Feature::VkWrap, || (), || ());
    }
    /// # Safety
    /// Called with a terminated function name.
    unsafe extern "system" fn gipa(
        instance: vk::Instance,
        name: *const c_char,
    ) -> vk::PFN_vkVoidFunction {
        let body = || {
            // SAFETY: every request uses a valid NUL-terminated name.
            let name = unsafe { CStr::from_ptr(name) };
            if matches!(
                name.to_bytes(),
                b"vkGetPhysicalDeviceProperties"
                    | b"vkEnumerateDeviceExtensionProperties"
                    | b"vkGetPhysicalDeviceFeatures2"
            ) {
                QUERY_INSTANCE.store(instance.as_raw() as usize, Ordering::Relaxed);
            }
            to_void(match name.to_bytes() {
                b"vkEnumeratePhysicalDeviceGroups" | b"vkEnumeratePhysicalDeviceGroupsKHR" => {
                    enumerate_groups as *const () as usize
                }
                b"vkEnumeratePhysicalDevices" => enumerate_physical as *const () as usize,
                b"vkDestroyInstance" => destroy_instance as *const () as usize,
                b"vkDestroyDevice" => {
                    if instance.as_raw() == INSTANCE {
                        other_destroy_device as *const () as usize
                    } else {
                        destroy_device as *const () as usize
                    }
                }
                b"vkDestroySwapchainKHR" => {
                    if instance.as_raw() == INSTANCE {
                        other_destroy_swapchain as *const () as usize
                    } else {
                        destroy_swapchain as *const () as usize
                    }
                }
                b"vkGetDeviceProcAddr" => gdpa as *const () as usize,
                b"vkGetPhysicalDeviceMemoryProperties" => memory as *const () as usize,
                b"vkGetPhysicalDeviceProperties" => properties as *const () as usize,
                b"vkEnumerateDeviceExtensionProperties" => extensions as *const () as usize,
                b"vkCreateInstance" => create_instance as *const () as usize,
                b"vkGetPhysicalDeviceFeatures2" => {
                    CORE_LOOKUPS.fetch_add(1, Ordering::Relaxed);
                    if MISSING_CORE.load(Ordering::Relaxed) != 0 {
                        0
                    } else {
                        features_core as *const () as usize
                    }
                }
                b"vkGetPhysicalDeviceFeatures2KHR" => {
                    KHR_LOOKUPS.fetch_add(1, Ordering::Relaxed);
                    if MISSING_KHR.load(Ordering::Relaxed) != 0 {
                        0
                    } else {
                        features_khr as *const () as usize
                    }
                }
                _ => 0,
            })
        };
        guard(Feature::VkWrap, body, body)
    }
    /// # Safety
    /// Vulkan device-create inputs and output remain live during this fake call.
    unsafe extern "system" fn create(
        _: vk::PhysicalDevice,
        info: *const vk::DeviceCreateInfo<'_>,
        _: *const vk::AllocationCallbacks<'_>,
        out: *mut vk::Device,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                let attempt = ATTEMPTS.fetch_add(1, Ordering::Relaxed);
                if info as usize == ORIGINAL.load(Ordering::Relaxed) {
                    RETRY_OK.fetch_add(1, Ordering::Relaxed);
                } else {
                    // SAFETY: the wrapper's owned create-info/chain/list remain live during the driver call.
                    unsafe {
                        let info = &*info;
                        let names = std::slice::from_raw_parts(
                            info.pp_enabled_extension_names,
                            info.enabled_extension_count as usize,
                        );
                        let low = names
                            .iter()
                            .any(|n| CStr::from_ptr(*n) == c"VK_NV_low_latency2");
                        let mut timeline = false;
                        let mut present = false;
                        let mut address_preserved = false;
                        let mut node = info.p_next.cast::<vk::BaseInStructure<'_>>();
                        while !node.is_null() {
                            if (*node).s_type
                                == vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES
                            {
                                let f = &*node.cast::<vk::PhysicalDeviceVulkan12Features<'_>>();
                                timeline = f.timeline_semaphore == vk::TRUE;
                                address_preserved = f.buffer_device_address == vk::TRUE;
                            }
                            if (*node).s_type
                                == vk::StructureType::PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES
                            {
                                timeline = (*node
                                    .cast::<vk::PhysicalDeviceTimelineSemaphoreFeatures<'_>>())
                                .timeline_semaphore
                                    == vk::TRUE;
                            }
                            if (*node).s_type
                                == vk::StructureType::PHYSICAL_DEVICE_PRESENT_ID_FEATURES_KHR
                            {
                                present = (*node
                                    .cast::<vk::PhysicalDevicePresentIdFeaturesKHR<'_>>())
                                .present_id
                                    == vk::TRUE;
                            }
                            node = (*node).p_next;
                        }
                        if low
                            && timeline
                            && present
                            && (address_preserved || CAPS_CASE.load(Ordering::Relaxed) != 0)
                        {
                            CHANGED_OK.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                if REFUSE.load(Ordering::Relaxed) != 0 && attempt == 0 {
                    return vk::Result::ERROR_EXTENSION_NOT_PRESENT;
                }
                // SAFETY: caller output storage is writable.
                unsafe {
                    out.write(vk::Device::from_raw(DEVICE));
                }
                vk::Result::SUCCESS
            },
        )
    }

    /// Enable the complete fake table only for the actual-present child cases.
    static PRESENT_CASE: AtomicUsize = AtomicUsize::new(0);
    /// Inject native failures without unwinding across the ABI.
    static PRESENT_RESULT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    static WAIT_RESULT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    /// Sleep failures stop before issuing any wait.
    static SLEEP_RESULT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    static PRESENT_INFO: AtomicUsize = AtomicUsize::new(0);
    static PRESENT_TAIL: AtomicUsize = AtomicUsize::new(0);

    /// Resolve only the native functions required to arm and exercise Reflex.
    fn present_entry(name: *const c_char) -> usize {
        // SAFETY: gdpa received a terminated Vulkan function name.
        match unsafe { CStr::from_ptr(name) }.to_bytes() {
            b"vkCreateSemaphore" => latency_semaphore as *const () as usize,
            b"vkDestroySemaphore" => latency_destroy as *const () as usize,
            b"vkSetLatencySleepModeNV" => latency_mode as *const () as usize,
            b"vkSetLatencyMarkerNV" => latency_marker as *const () as usize,
            b"vkLatencySleepNV" => latency_sleep as *const () as usize,
            b"vkWaitSemaphores" | b"vkWaitSemaphoresKHR" => latency_wait as *const () as usize,
            b"vkGetLatencyTimingsNV" => latency_timings as *const () as usize,
            b"vkCreateSwapchainKHR" => latency_swapchain as *const () as usize,
            b"vkQueuePresentKHR" => latency_present as *const () as usize,
            _ => 0,
        }
    }
    /// # Safety
    /// The wrapper provides initialized semaphore input and writable output.
    unsafe extern "system" fn latency_semaphore(
        _: vk::Device,
        info: *const vk::SemaphoreCreateInfo<'_>,
        _: *const vk::AllocationCallbacks<'_>,
        out: *mut vk::Semaphore,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                // SAFETY: Runtime supplies this exact initialized timeline node and output.
                unsafe {
                    let timeline = &*((*info).p_next.cast::<vk::SemaphoreTypeCreateInfo<'_>>());
                    assert!(timeline.semaphore_type == vk::SemaphoreType::TIMELINE);
                    out.write(vk::Semaphore::from_raw(77));
                }
                vk::Result::SUCCESS
            },
        )
    }
    /// # Safety
    /// The fake semaphore remains live until wrapped retirement.
    unsafe extern "system" fn latency_destroy(
        _: vk::Device,
        _: vk::Semaphore,
        _: *const vk::AllocationCallbacks<'_>,
    ) {
        let body = || note("destroy semaphore".into());
        guard(Feature::VkWrap, body, body)
    }
    /// # Safety
    /// The wrapper provides a live sleep-mode input.
    unsafe extern "system" fn latency_mode(
        _: vk::Device,
        _: vk::SwapchainKHR,
        info: *const vk::LatencySleepModeInfoNV<'_>,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                // SAFETY: the native call borrows this initialized input.
                let info = unsafe { &*info };
                note(format!(
                    "mode:{}:{}",
                    info.low_latency_mode, info.low_latency_boost
                ));
                vk::Result::SUCCESS
            },
        )
    }
    /// # Safety
    /// The wrapper supplies a valid marker input.
    unsafe extern "system" fn latency_marker(
        _: vk::Device,
        _: vk::SwapchainKHR,
        info: *const vk::SetLatencyMarkerInfoNV<'_>,
    ) {
        guard(
            Feature::VkWrap,
            || (),
            || {
                // SAFETY: live initialized input for this synchronous call.
                let info = unsafe { &*info };
                note(format!(
                    "marker:{}:{}",
                    info.present_id,
                    info.marker.as_raw()
                ));
            },
        )
    }
    /// # Safety
    /// The wrapper supplies a valid sleep input.
    unsafe extern "system" fn latency_sleep(
        _: vk::Device,
        _: vk::SwapchainKHR,
        info: *const vk::LatencySleepInfoNV<'_>,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                // SAFETY: live initialized input for this synchronous call.
                note(format!("sleep:{}", unsafe { (*info).value }));
                vk::Result::from_raw(SLEEP_RESULT.load(Ordering::Relaxed))
            },
        )
    }
    /// # Safety
    /// The wrapper supplies valid wait arrays, retained through the call.
    unsafe extern "system" fn latency_wait(
        _: vk::Device,
        _: *const vk::SemaphoreWaitInfo<'_>,
        timeout: u64,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                note(format!("wait:{timeout}"));
                if timeout == u64::MAX {
                    vk::Result::SUCCESS
                } else {
                    vk::Result::from_raw(WAIT_RESULT.load(Ordering::Relaxed))
                }
            },
        )
    }
    /// # Safety
    /// No timings are written; the initialized zero reports remain untouched.
    unsafe extern "system" fn latency_timings(
        _: vk::Device,
        _: vk::SwapchainKHR,
        _: *mut vk::GetLatencyMarkerInfoNV<'_>,
    ) {
        guard(Feature::VkWrap, || (), || ())
    }
    /// # Safety
    /// The fake receives live Vulkan create info and writable output.
    unsafe extern "system" fn latency_swapchain(
        _: vk::Device,
        _: *const vk::SwapchainCreateInfoKHR<'_>,
        _: *const vk::AllocationCallbacks<'_>,
        out: *mut vk::SwapchainKHR,
    ) -> vk::Result {
        guard(
            Feature::VkWrap,
            || vk::Result::ERROR_UNKNOWN,
            || {
                // SAFETY: caller supplies writable output.
                unsafe { out.write(vk::SwapchainKHR::from_raw(SWAPCHAIN)) };
                vk::Result::SUCCESS
            },
        )
    }
    /// # Safety
    /// The actual present wrapper must forward the test's original live info unchanged.
    unsafe extern "system" fn latency_present(
        queue: vk::Queue,
        info: *const vk::PresentInfoKHR<'_>,
    ) -> vk::Result {
        let body = || {
            assert_eq!(queue.as_raw(), QUEUE);
            assert_eq!(info as usize, PRESENT_INFO.load(Ordering::Relaxed));
            // SAFETY: present_child retains the info and its chain through this call.
            assert_eq!(
                unsafe { (*info).p_next } as usize,
                PRESENT_TAIL.load(Ordering::Relaxed)
            );
            note("present".into());
            vk::Result::from_raw(PRESENT_RESULT.load(Ordering::Relaxed))
        };
        guard(Feature::VkWrap, body, body)
    }
    /// Drive the real lookup, swapchain and present wrappers after parsed-mode device creation.
    fn present_child(case: &str, device: vk::Device) {
        // SAFETY: the exact fake resolver returns these matching native signatures.
        let (create, present) = unsafe {
            wrap_gipa(
                vk::Instance::from_raw(INSTANCE + 1),
                c"vkGetDeviceProcAddr".as_ptr(),
            );
            let create = wrap_gdpa(device, c"vkCreateSwapchainKHR".as_ptr()).unwrap();
            let present = wrap_gdpa(device, c"vkQueuePresentKHR".as_ptr()).unwrap();
            (
                std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkCreateSwapchainKHR>(
                    create,
                ),
                std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkQueuePresentKHR>(
                    present,
                ),
            )
        };
        assert_eq!(present as usize, wrap_present as *const () as usize);
        let mut swapchain = vk::SwapchainKHR::null();
        // SAFETY: initialized inputs and live fake device/output.
        assert_eq!(
            unsafe {
                (if case == "present-no-runtime" {
                    latency_swapchain
                } else {
                    create
                })(
                    device,
                    &vk::SwapchainCreateInfoKHR::default(),
                    std::ptr::null(),
                    &mut swapchain,
                )
            },
            vk::Result::SUCCESS
        );
        let setup = take_calls();
        if matches!(case, "present-off" | "present-no-runtime") {
            assert!(!setup.iter().any(|call| call.starts_with("mode:")));
            assert_eq!(
                CHANGED_OK.load(Ordering::Relaxed),
                usize::from(case != "present-off")
            );
        } else {
            assert!(setup.iter().any(|call| call == "mode:1:0"));
        }
        let result = match case {
            "present-lost" => vk::Result::ERROR_DEVICE_LOST,
            "present-outdated" => vk::Result::ERROR_OUT_OF_DATE_KHR,
            _ => vk::Result::SUBOPTIMAL_KHR,
        };
        PRESENT_RESULT.store(result.as_raw(), Ordering::Relaxed);
        WAIT_RESULT.store(
            match case {
                "present-timeout" => vk::Result::TIMEOUT,
                "present-wait-lost" => vk::Result::ERROR_DEVICE_LOST,
                "present-wait-error" => vk::Result::ERROR_UNKNOWN,
                _ => vk::Result::SUCCESS,
            }
            .as_raw(),
            Ordering::Relaxed,
        );
        SLEEP_RESULT.store(
            if case == "present-sleep-error" {
                vk::Result::ERROR_UNKNOWN.as_raw()
            } else {
                0
            },
            Ordering::Relaxed,
        );
        if case == "present-disabled" {
            enshrouded_headroom::guard::disable(Feature::VkWrap, "test disabled forwarding");
        }
        let plain = matches!(
            case,
            "present-off" | "present-no-runtime" | "present-other-swapchain" | "present-disabled"
        );
        let swaps = [if case == "present-other-swapchain" {
            vk::SwapchainKHR::from_raw(SWAPCHAIN + 1)
        } else {
            swapchain
        }];
        let indices = [7];
        let tail = vk::BaseInStructure {
            s_type: vk::StructureType::from_raw(1_000_479_001),
            ..Default::default()
        };
        let mut info = vk::PresentInfoKHR::default()
            .swapchains(&swaps)
            .image_indices(&indices);
        info.p_next = (&tail as *const vk::BaseInStructure<'_>).cast();
        PRESENT_INFO.store(&info as *const _ as usize, Ordering::Relaxed);
        PRESENT_TAIL.store(info.p_next as usize, Ordering::Relaxed);
        let mut timestamps = Vec::new();
        let before = FRAMES.read_since(0, &mut timestamps);
        for frame in 1..=2 {
            // SAFETY: live fake handles and immutable info/chain through the real wrapper.
            assert_eq!(
                unsafe { present(vk::Queue::from_raw(QUEUE), &info) },
                result
            );
            let calls = take_calls();
            if plain || (frame == 2 && case != "present-success") {
                assert_eq!(calls, ["present"]);
            } else {
                let mut expected = vec![
                    format!("marker:{frame}:4"),
                    "present".into(),
                    format!("marker:{frame}:5"),
                ];
                if !matches!(case, "present-lost" | "present-outdated") {
                    expected.push(format!("sleep:{frame}"));
                    if case != "present-sleep-error" {
                        expected.push("wait:10000000".into());
                    }
                    if case == "present-success" {
                        expected.extend([
                            format!("marker:{}:0", frame + 1),
                            format!("marker:{}:2", frame + 1),
                        ]);
                    }
                }
                assert_eq!(calls, expected);
            }
            assert_eq!(info.p_next as usize, PRESENT_TAIL.load(Ordering::Relaxed));
            assert_eq!(tail.s_type.as_raw(), 1_000_479_001);
            assert!(tail.p_next.is_null());
            assert_eq!(indices, [7]);
        }
        let recorded = if case == "present-disabled" { 0 } else { 2 };
        assert_eq!(
            FRAMES.read_since(before, &mut timestamps),
            before + recorded
        );
        assert_eq!(timestamps.len() as u64, recorded);
        assert!(timestamps.iter().all(|t| *t != 0));
        assert_eq!(
            enshrouded_headroom::guard::is_disabled(Feature::Reflex),
            !plain && case != "present-success"
        );
        // SAFETY: retire while fake device/semaphore handles remain live; timeout cleanup is outside present.
        unsafe { enshrouded_headroom::vk::wrap_destroy_device(device, std::ptr::null()) };
    }
    /// Actual wrapper behavior, not a helper-only acceptance test; each mode/failure gets fresh globals.
    #[test]
    fn actual_present_uses_parsed_mode_and_preserves_native_call() {
        const NAME: &str =
            "reflex_driver::actual_present_uses_parsed_mode_and_preserves_native_call";
        if let Ok(case) = std::env::var("W23_PRESENT_CHILD") {
            child(&case);
            return;
        }
        for case in [
            "present-success",
            "present-timeout",
            "present-lost",
            "present-outdated",
            "present-wait-lost",
            "present-wait-error",
            "present-sleep-error",
            "present-off",
            "present-no-runtime",
            "present-other-swapchain",
            "present-disabled",
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env("W23_PRESENT_CHILD", case)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: {}\\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    /// Non-null pointers cannot authorize an alias; successful instance observation is required.
    #[test]
    fn feature_query_requires_proven_core_or_khr_route() {
        const NAME: &str = "reflex_driver::feature_query_requires_proven_core_or_khr_route";
        if let Ok(case) = std::env::var("W23_CAPS_CHILD") {
            child(&case);
            return;
        }
        for case in [
            "caps-khr",
            "caps-core",
            "caps-unknown",
            "caps-failed-create",
            "caps-no-route",
            "caps-low-physical",
            "caps-low-physical-khr",
            "caps-core-missing",
            "caps-khr-missing",
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--nocapture"])
                .env("W23_CAPS_CHILD", case)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    fn child(case: &str) {
        let (config, warnings) = enshrouded_headroom::config::parse(if case == "present-off" {
            "[General]\nReflex = off\n"
        } else {
            "[General]\nReflex = auto\n"
        });
        assert!(warnings.is_empty());
        reflex::init(config.general.reflex);
        CAPS_CASE.store(usize::from(case.starts_with("caps-")), Ordering::Relaxed);
        PRESENT_CASE.store(usize::from(case.starts_with("present-")), Ordering::Relaxed);
        REAL_GIPA.store(gipa as *const () as usize, Ordering::Release);
        observe_instance(case);
        if matches!(case, "caps-low-physical" | "caps-low-physical-khr") {
            PHYSICAL_API.store(vk::API_VERSION_1_0 as usize, Ordering::Relaxed);
        }
        if case == "caps-core" {
            PHYSICAL_API.store(vk::API_VERSION_1_1 as usize, Ordering::Relaxed);
        }
        MISSING_CORE.store(usize::from(case == "caps-core-missing"), Ordering::Relaxed);
        MISSING_KHR.store(usize::from(case == "caps-khr-missing"), Ordering::Relaxed);
        GAME_INSTANCE.store(INSTANCE as usize, Ordering::Release);
        REAL_CREATE_DEVICE.store(create as *const () as usize, Ordering::Release);
        if case.starts_with("dispatch-conflict") {
            // SAFETY: obtain distinct A-native destructors through the actual installed GIPA wrapper.
            unsafe {
                if !case.contains("-query-") {
                    enshrouded_headroom::vk::wrap_gipa(
                        vk::Instance::from_raw(INSTANCE),
                        c"vkDestroyDevice".as_ptr(),
                    );
                }
                enshrouded_headroom::vk::wrap_gipa(
                    vk::Instance::from_raw(INSTANCE),
                    c"vkDestroySwapchainKHR".as_ptr(),
                );
            }
        }
        if case == "incomplete" {
            ENUM_RESULT.store(1, Ordering::Relaxed);
        }
        if case == "enum-failure" {
            ENUM_RESULT.store(2, Ordering::Relaxed);
        }
        if case != "unobserved" {
            // SAFETY: fake instances and initialized one-element outputs obey the exact fake APIs.
            unsafe {
                for instance in if case == "parentage" {
                    vec![INSTANCE, INSTANCE + 1]
                } else {
                    vec![INSTANCE + 1]
                } {
                    let instance = vk::Instance::from_raw(instance);
                    if matches!(case, "groups" | "groups-khr") {
                        let name = if case == "groups" {
                            c"vkEnumeratePhysicalDeviceGroups"
                        } else {
                            c"vkEnumeratePhysicalDeviceGroupsKHR"
                        };
                        let raw =
                            enshrouded_headroom::vk::wrap_gipa(instance, name.as_ptr()).unwrap();
                        let enumerate = std::mem::transmute::<
                            unsafe extern "system" fn(),
                            vk::PFN_vkEnumeratePhysicalDeviceGroups,
                        >(raw);
                        let mut count = 1;
                        let mut group = vk::PhysicalDeviceGroupProperties::default();
                        assert_eq!(
                            enumerate(instance, &mut count, &mut group),
                            vk::Result::SUCCESS
                        );
                        assert_eq!(group.physical_device_count, 1);
                    } else {
                        let raw = enshrouded_headroom::vk::wrap_gipa(
                            instance,
                            c"vkEnumeratePhysicalDevices".as_ptr(),
                        )
                        .unwrap();
                        let enumerate = std::mem::transmute::<
                            unsafe extern "system" fn(),
                            vk::PFN_vkEnumeratePhysicalDevices,
                        >(raw);
                        let mut count = 1;
                        let mut physical = vk::PhysicalDevice::null();
                        let output = if case == "count-only" {
                            std::ptr::null_mut()
                        } else {
                            &mut physical
                        };
                        let result = enumerate(instance, &mut count, output);
                        assert_eq!(
                            result,
                            if case == "incomplete" {
                                vk::Result::INCOMPLETE
                            } else if case == "enum-failure" {
                                vk::Result::ERROR_UNKNOWN
                            } else {
                                vk::Result::SUCCESS
                            }
                        );
                    }
                }
            }
        }
        if case == "parentage" {
            enshrouded_headroom::vk::REAL_GET_MEMORY_PROPERTIES
                .store(memory as *const () as usize, Ordering::Release);
            enshrouded_headroom::vk::REAL_GET_PROPERTIES
                .store(wrong_properties as *const () as usize, Ordering::Release);
        }
        if case == "vendor" {
            VENDOR.store(0x1002, Ordering::Relaxed);
        }
        if case == "refusal" {
            REFUSE.store(1, Ordering::Relaxed);
        }
        let mut tail = vk::PhysicalDeviceVulkan13Features::default().dynamic_rendering(true);
        let mut v12 = vk::PhysicalDeviceVulkan12Features::default().buffer_device_address(true);
        v12.p_next = (&mut tail as *mut vk::PhysicalDeviceVulkan13Features<'_>).cast();
        let original_next = v12.p_next;
        let names = [c"VK_KHR_swapchain".as_ptr()];
        let mut info = vk::DeviceCreateInfo::default().enabled_extension_names(&names);
        info.p_next = if case.starts_with("caps-") {
            // The 1.0 route cases must not request newer core feature structs themselves.
            std::ptr::null()
        } else {
            (&v12 as *const vk::PhysicalDeviceVulkan12Features<'_>).cast()
        };
        let original_chain = info.p_next;
        ORIGINAL.store(&info as *const _ as usize, Ordering::Relaxed);
        let mut device = vk::Device::null();
        // SAFETY: live fake inputs/outputs and fake function table for the made-up physical device.
        let result = unsafe {
            wrap_create_device(
                vk::PhysicalDevice::from_raw(PHYS),
                &info,
                std::ptr::null(),
                &mut device,
            )
        };
        assert_eq!(result, vk::Result::SUCCESS);
        assert_eq!(info.p_next, original_chain);
        assert_eq!(v12.p_next, original_next);
        assert_eq!(v12.timeline_semaphore, vk::FALSE);
        assert_eq!(v12.buffer_device_address, vk::TRUE);
        if case.starts_with("caps-") {
            let (core, khr) = match case {
                "caps-core" => (1, 0),
                "caps-khr" | "caps-low-physical-khr" => (0, 1),
                _ => (0, 0),
            };
            assert_eq!(
                CORE_CALLS.load(Ordering::Relaxed),
                core,
                "forbidden core feature call"
            );
            assert_eq!(KHR_CALLS.load(Ordering::Relaxed), khr, "KHR feature call");
            assert_eq!(
                CORE_LOOKUPS.load(Ordering::Relaxed),
                core + usize::from(case == "caps-core-missing"),
                "only resolve a permitted core route"
            );
            assert_eq!(
                KHR_LOOKUPS.load(Ordering::Relaxed),
                khr + usize::from(case == "caps-khr-missing"),
                "only resolve a permitted KHR route"
            );
            let modified = usize::from(core + khr != 0);
            assert_eq!(CHANGED_OK.load(Ordering::Relaxed), modified);
            assert_eq!(
                RETRY_OK.load(Ordering::Relaxed),
                1 - modified,
                "unknown/missing route forwards the original pointer unchanged"
            );
            assert_eq!(ATTEMPTS.load(Ordering::Relaxed), 1);
            return;
        }
        if case.starts_with("present-") {
            present_child(case, device);
            return;
        }
        if case.starts_with("dispatch-conflict-") {
            assert_eq!(
                enshrouded_headroom::vk::with_game_device(|_, _, _| ()).is_some(),
                case.contains("-query-")
            );
            assert_eq!(CHANGED_OK.load(Ordering::Relaxed), 0);
            assert_eq!(RETRY_OK.load(Ordering::Relaxed), 1);
            // SAFETY: B's actual GIPA supplies the fake device resolver before ordinary wrapping is disabled.
            let get_device = unsafe {
                let raw = enshrouded_headroom::vk::wrap_gipa(
                    vk::Instance::from_raw(INSTANCE + 1),
                    c"vkGetDeviceProcAddr".as_ptr(),
                )
                .unwrap();
                std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkGetDeviceProcAddr>(raw)
            };
            if case.ends_with("disabled") {
                enshrouded_headroom::guard::disable(Feature::VkWrap, "test");
            }
            let name = if case.contains("swapchain") {
                c"vkDestroySwapchainKHR"
            } else {
                c"vkDestroyDevice"
            };
            // SAFETY: look up and invoke the actual signature for B's native object once.
            unsafe {
                let raw = if case.contains("-gipa-") {
                    enshrouded_headroom::vk::wrap_gipa(
                        vk::Instance::from_raw(INSTANCE + 1),
                        name.as_ptr(),
                    )
                } else {
                    get_device(device, name.as_ptr())
                }
                .unwrap();
                if case.contains("swapchain") {
                    std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkDestroySwapchainKHR>(
                        raw,
                    )(device, vk::SwapchainKHR::from_raw(44), std::ptr::null());
                } else {
                    std::mem::transmute::<unsafe extern "system" fn(), vk::PFN_vkDestroyDevice>(
                        raw,
                    )(device, std::ptr::null());
                }
            }
            assert_eq!(
                A_DESTROYS.load(Ordering::Relaxed),
                0,
                "A dispatch must never destroy B's object"
            );
            assert_eq!(
                B_DESTROYS.load(Ordering::Relaxed),
                1,
                "B's matched native destructor runs once"
            );
            return;
        }
        match case {
            "unobserved" | "enum-failure" | "count-only" | "dispatch-conflict" => {
                assert!(enshrouded_headroom::vk::with_game_device(|_, _, _| ()).is_none());
                assert_eq!(CHANGED_OK.load(Ordering::Relaxed), 0);
                assert_eq!(RETRY_OK.load(Ordering::Relaxed), 1);
                if case != "dispatch-conflict" {
                    assert_eq!(QUERY_INSTANCE.load(Ordering::Relaxed), 0);
                }
            }
            "parentage" => {
                assert_eq!(
                    WRONG_GPU_QUERY.load(Ordering::Relaxed),
                    0,
                    "instance A GPU dispatch must not query B"
                );
                assert_eq!(
                    QUERY_INSTANCE.load(Ordering::Relaxed),
                    (INSTANCE + 1) as usize
                );
                assert_eq!(
                    enshrouded_headroom::vk::with_game_device(|d, _, i| (d.phys, i)),
                    Some((PHYS as usize, (INSTANCE + 1) as usize))
                );
                // Destroying unrelated A cannot retire B's context; destroying B must.
                for instance in [INSTANCE, INSTANCE + 1] {
                    if instance == INSTANCE + 1 {
                        // SAFETY: Vulkan requires the child device to be destroyed before its parent.
                        unsafe {
                            enshrouded_headroom::vk::wrap_destroy_device(device, std::ptr::null())
                        };
                    }
                    // SAFETY: each live fake instance is destroyed exactly once through its lookup.
                    unsafe {
                        let raw = enshrouded_headroom::vk::wrap_gipa(
                            vk::Instance::from_raw(instance),
                            c"vkDestroyInstance".as_ptr(),
                        )
                        .unwrap();
                        let destroy = std::mem::transmute::<
                            unsafe extern "system" fn(),
                            vk::PFN_vkDestroyInstance,
                        >(raw);
                        destroy(vk::Instance::from_raw(instance), std::ptr::null());
                    }
                    assert_eq!(
                        enshrouded_headroom::vk::with_game_device(|_, _, i| i),
                        if instance == INSTANCE {
                            Some((INSTANCE + 1) as usize)
                        } else {
                            None
                        }
                    );
                }
            }
            "extension" | "groups" | "groups-khr" | "incomplete" => {
                assert_eq!(ATTEMPTS.load(Ordering::Relaxed), 1);
                assert_eq!(CHANGED_OK.load(Ordering::Relaxed), 1);
                assert_eq!(RETRY_OK.load(Ordering::Relaxed), 0);
            }
            "refusal" => {
                assert_eq!(ATTEMPTS.load(Ordering::Relaxed), 2);
                assert_eq!(CHANGED_OK.load(Ordering::Relaxed), 1);
                assert_eq!(RETRY_OK.load(Ordering::Relaxed), 1);
                assert!(enshrouded_headroom::guard::is_disabled(Feature::Reflex));
            }
            "vendor" => {
                assert_eq!(ATTEMPTS.load(Ordering::Relaxed), 1);
                assert_eq!(CHANGED_OK.load(Ordering::Relaxed), 0);
                assert_eq!(RETRY_OK.load(Ordering::Relaxed), 1);
                assert!(!enshrouded_headroom::guard::is_disabled(Feature::Reflex));
            }
            _ => unreachable!(),
        }
    }
    #[test]
    fn listed_extension_original_retry_and_vendor_gate() {
        if let Ok(case) = std::env::var("W23_FAKE_DRIVER") {
            child(&case);
            return;
        }
        for case in [
            "dispatch-conflict-query-gdpa-swapchain-enabled",
            "dispatch-conflict-query-gdpa-swapchain-disabled",
            "dispatch-conflict-query-gipa-swapchain-enabled",
            "dispatch-conflict-query-gipa-swapchain-disabled",
            "dispatch-conflict-gdpa-device-enabled",
            "dispatch-conflict-gdpa-device-disabled",
            "dispatch-conflict-gdpa-swapchain-enabled",
            "dispatch-conflict-gdpa-swapchain-disabled",
            "dispatch-conflict-gipa-device-enabled",
            "dispatch-conflict-gipa-device-disabled",
            "dispatch-conflict-gipa-swapchain-enabled",
            "dispatch-conflict-gipa-swapchain-disabled",
            "dispatch-conflict",
            "parentage",
            "unobserved",
            "enum-failure",
            "count-only",
            "groups",
            "groups-khr",
            "incomplete",
            "extension",
            "refusal",
            "vendor",
        ] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reflex_driver::listed_extension_original_retry_and_vendor_gate",
                    "--nocapture",
                ])
                .env("W23_FAKE_DRIVER", case)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
