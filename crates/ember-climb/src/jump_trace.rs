//! Read-only observations of native Jump acceptance and calculated Jump deltas.
//!
//! Native facts are in docs/re-notes.md, "W52 diagnostic request and impulse observation".
//! Only copied scalar records leave a native owner invocation. The lexical TLS frame is restored
//! before its owner advances; no pointer becomes a queued request or a gameplay marker. Rust
//! guards cover Rust unwinding, under the existing native no-foreign-unwind assumption.

use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use emberkit::guard::guard;

use crate::hooks::Slot;
use crate::trace_sigs::{Layout, Targets};
use crate::Feature;

/// Whether the complete optional observer set is active.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// The preflighted entity table layout; zero means no identity may be read yet.
static ENTITY_TABLE: AtomicUsize = AtomicUsize::new(0);
/// Bounded records waiting for the existing log thread.
static RECORDS: Ring = Ring::new();
/// Records dropped at a busy/full queue or unavailable TLS. This diagnostic count may wrap.
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// Intentionally omitted checks: zero native AL and no matching publication accessor.
/// Atomic addition is defined to wrap; this counter never drives gameplay or queue admission.
static UNPUBLISHED: AtomicU64 = AtomicU64::new(0);
/// Copied scalar records retained between drain calls.
const CAPACITY: usize = 32;
/// Native recursion never requires an unbounded observer stack.
const MAX_DEPTH: u8 = 8;
/// The current owner's native ActorInput write-view.
const INPUT_VIEW: usize = 0xe8;
/// The initialized native external request type.
const REQUEST_TYPE: usize = 0x179;
/// The initialized native external request simulation time.
const REQUEST_TIME: usize = 0x180;
/// Processed local upward input, as float bits.
const LOCAL_Z: usize = 0x194;
/// ActorInput pointer in the native dispatcher context.
const DISPATCH_INPUT: usize = 0x20;

/// Why an input observation is present or missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleStatus {
    /// A matching native accessor returned the current owner's input.
    Captured,
    /// The native helper did not call the matching accessor.
    NoAccessor,
    /// The live owner view no longer names the observed component.
    PointerChanged,
    /// A required native pointer was null.
    NullPointer,
    /// A checked address calculation failed.
    AddressOverflow,
    /// Nested native calls exceeded the bounded observation depth.
    NestedLimit,
    /// The lexical TLS frame was not available or did not belong to this invocation.
    ScopeUnavailable,
}

/// Copied native input fields. This is a correlation candidate, not a unique paid event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// The native external Jump type.
    pub jump_type: u8,
    /// The native simulation request time, in nanoseconds.
    pub time: i64,
    /// The processed local upward input, copied as float bits.
    pub z_bits: u32,
}

/// One native acceptance invocation, copied before its owner advances the iterator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accepted {
    /// The native entity identity, or unknown when it could not be read.
    pub entity: Option<u32>,
    /// Whether the unchanged native return byte was nonzero.
    pub accepted: bool,
    /// The unchanged native return byte, without assuming a Rust bool representation.
    pub native_al: u8,
    /// Whether the final input fields could be sampled.
    pub status: SampleStatus,
    /// Final native request fields, when their live pointer was confirmed.
    pub request: Option<Request>,
    /// Upward input at the first matching native accessor return.
    pub first_z_bits: Option<u32>,
    /// Matching accessor returns, saturated at the byte limit.
    pub accessor_calls: u8,
}

/// One original native dispatcher result, before native application of its calculated delta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalculatedDelta {
    /// The native entity identity, or unknown when it could not be read.
    pub entity: Option<u32>,
    /// The Jump type argument passed unchanged to the native dispatcher.
    pub jump_type: u8,
    /// Whether the dispatcher context supplied native input fields.
    pub status: SampleStatus,
    /// Native external request fields at dispatcher entry.
    pub request: Option<Request>,
    /// The native result's initialized error byte, or absent on a null/overflowed return.
    pub error: Option<u8>,
    /// The original calculated float3 bits, read only on native success.
    pub delta_bits: Option<[u32; 3]>,
}

/// An owned scalar observation. It contains no native pointer or borrowed storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    /// A native acceptance helper invocation.
    Accepted(Accepted),
    /// A native Jump dispatcher invocation.
    CalculatedDelta(CalculatedDelta),
}

/// One optional hook, already resolved by the whole-set startup preflight.
#[derive(Clone, Copy)]
pub struct Site {
    /// Its diagnostic name.
    pub name: &'static str,
    /// The verified native function entry.
    pub target: usize,
    /// The matching Rust detour entry.
    pub detour: usize,
    /// Startup publishes the trampoline here before enabling the queued hook.
    pub slot: &'static Slot,
}

/// Store the exact image's entity layout before any optional observer is enabled.
pub fn prepare(layout: Layout) {
    ENTITY_TABLE.store(layout.entity_table_offset(), Ordering::Release);
}

/// The complete optional set. Call only after preflight has resolved every target.
pub fn sites(targets: &Targets) -> [Site; 3] {
    [
        Site {
            name: "JUMP_ACCEPTED_OBSERVER",
            target: targets.accepted,
            detour: accepted_detour as *const () as usize,
            slot: &ACCEPTED,
        },
        Site {
            name: "JUMP_INPUT_OBSERVER",
            target: targets.accessor,
            detour: accessor_detour as *const () as usize,
            slot: &ACCESSOR,
        },
        Site {
            name: "JUMP_DELTA_OBSERVER",
            target: targets.dispatcher,
            detour: dispatcher_detour as *const () as usize,
            slot: &DISPATCHER,
        },
    ]
}

/// Enable collection only after startup has installed the complete optional observer set.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
}

/// Take one queued scalar record with one claim attempt. The existing log thread drains these.
pub fn take() -> Option<Record> {
    RECORDS.take()
}

/// Take and reset the diagnostic loss count with one atomic operation.
pub fn take_dropped() -> u64 {
    DROPPED.swap(0, Ordering::Relaxed)
}

/// Take the count of deliberately omitted checks with no acceptance or observed publication.
pub fn take_unpublished() -> u64 {
    UNPUBLISHED.swap(0, Ordering::Relaxed)
}

/// One successful nonblocking claim; its release also runs on a Rust unwind.
struct Claim<'a>(&'a AtomicBool);

impl<'a> Claim<'a> {
    /// Try once; another reader/writer never makes this caller wait.
    fn take(busy: &'a AtomicBool) -> Option<Self> {
        busy.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self(busy))
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Mutable scalar queue state, reached only by a successful claim.
struct Buffer {
    /// Initialized owned records; no slot contains native storage.
    entries: [Option<Record>; CAPACITY],
    /// The next record to drain.
    read: usize,
    /// The next available slot.
    write: usize,
    /// Retained records, bounded by capacity.
    len: usize,
}

/// A bounded multi-producer scalar queue, with one attempt per push or take.
struct Ring {
    /// Serializes access without spinning or waiting.
    busy: AtomicBool,
    /// Access is exclusive while the claim lives.
    buffer: UnsafeCell<Buffer>,
}

// SAFETY: every access to buffer takes the same acquire/release claim. Failed claims touch no
// buffer data. The buffer contains only owned Copy scalars, and no reference escapes a claim.
unsafe impl Sync for Ring {}

impl Ring {
    /// An empty queue with fixed storage.
    const fn new() -> Self {
        Self {
            busy: AtomicBool::new(false),
            buffer: UnsafeCell::new(Buffer {
                entries: [None; CAPACITY],
                read: 0,
                write: 0,
                len: 0,
            }),
        }
    }

    /// Store once, or drop the new observation without changing the retained records.
    fn push(&self, record: Record) -> bool {
        let Some(_claim) = Claim::take(&self.busy) else {
            return false;
        };
        // SAFETY: the claim exclusively owns buffer access until this function returns.
        let buffer = unsafe { &mut *self.buffer.get() };
        if buffer.len >= CAPACITY {
            return false;
        }
        let Some(slot) = buffer.entries.get_mut(buffer.write) else {
            return false;
        };
        *slot = Some(record);
        buffer.write = next_slot(buffer.write);
        buffer.len = buffer.len.saturating_add(1);
        true
    }

    /// Drain one oldest record, or return immediately when empty/busy.
    fn take(&self) -> Option<Record> {
        let _claim = Claim::take(&self.busy)?;
        // SAFETY: the claim exclusively owns buffer access until this function returns.
        let buffer = unsafe { &mut *self.buffer.get() };
        if buffer.len == 0 {
            return None;
        }
        let record = buffer.entries.get_mut(buffer.read)?.take();
        buffer.read = next_slot(buffer.read);
        buffer.len = buffer.len.saturating_sub(1);
        record
    }
}

/// Advance within fixed storage with checked arithmetic.
fn next_slot(slot: usize) -> usize {
    slot.checked_add(1)
        .filter(|next| *next < CAPACITY)
        .unwrap_or(0)
}

/// Queue only owned scalars. A dropped claim/full queue never affects native execution.
fn publish(record: Record) {
    if !RECORDS.push(record) {
        // Atomic addition has defined wrapping behavior. A diagnostic loss counter is not state
        // used for native input, deduplication, payment, or any other gameplay decision.
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Borrowed pointers for one lexical native owner invocation only.
#[derive(Clone, Copy)]
struct Frame {
    /// Exact address of the native view supplied by this accepted helper's iterator.
    expected_view: *const u8,
    /// Last pointer returned by the matching native-invoked accessor.
    last_input: *const u8,
    /// First observed processed upward input, copied immediately.
    first_z: Option<u32>,
    /// Native matching accessor calls, with saturation explicit in the record.
    calls: u8,
}

/// This thread's current lexical owner. Saved outer copies stay on the Rust call stack.
#[derive(Clone, Copy)]
struct State {
    /// Native observer nesting, bounded for collection rather than native execution.
    depth: u8,
    /// None shadows the outer frame when a nested invocation cannot be observed.
    frame: Option<Frame>,
}

thread_local! {
    /// Const-initialized scalar TLS has no allocation, lazy destructor or retained native owner.
    static CURRENT: Cell<State> = const { Cell::new(State { depth: 0, frame: None }) };
}

/// Restore the exact outer lexical state on every Rust return/unwind path.
struct Scope {
    /// Still-live outer native owner, or the empty state before an outermost call.
    previous: State,
    /// This invocation's nesting, used to reject an inconsistent current frame.
    depth: u8,
    /// A bounded/missing frame must not accidentally observe the outer owner.
    missing: Option<SampleStatus>,
}

impl Scope {
    /// Shadow the previous frame before invoking native code, without holding any Rust borrow.
    fn enter(view: Result<*const u8, SampleStatus>) -> Option<Self> {
        CURRENT
            .try_with(|current| {
                let previous = current.get();
                let depth = previous.depth.saturating_add(1);
                let missing = if previous.depth >= MAX_DEPTH {
                    Some(SampleStatus::NestedLimit)
                } else {
                    view.err()
                };
                let frame = if missing.is_none() {
                    view.ok().map(|expected_view| Frame {
                        expected_view,
                        last_input: std::ptr::null(),
                        first_z: None,
                        calls: 0,
                    })
                } else {
                    None
                };
                current.set(State { depth, frame });
                Self {
                    previous,
                    depth,
                    missing,
                }
            })
            .ok()
    }

    /// A copied lexical frame while its owner is suspended; it never enters the queue.
    fn frame(&self) -> Option<Frame> {
        CURRENT
            .try_with(|current| {
                let state = current.get();
                (state.depth == self.depth).then_some(state.frame).flatten()
            })
            .ok()
            .flatten()
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        let _ = CURRENT.try_with(|current| current.set(self.previous));
    }
}

/// A checked native address. The current native owner supplies the allocation/lifetime contract.
fn address(base: *const u8, offset: usize, length: usize) -> Result<*const u8, SampleStatus> {
    if base.is_null() {
        return Err(SampleStatus::NullPointer);
    }
    let at = base
        .addr()
        .checked_add(offset)
        .ok_or(SampleStatus::AddressOverflow)?;
    at.checked_add(length)
        .ok_or(SampleStatus::AddressOverflow)?;
    Ok(base.with_addr(at))
}

/// Copy an initialized native scalar without creating a Rust reference to native storage.
///
/// # Safety
///
/// The native owner keeps this range readable during the call. `T` is an integer or raw pointer
/// whose bits are initialized by that native type; this helper cannot validate an arbitrary pointer.
unsafe fn read_native<T: Copy>(base: *const u8, offset: usize) -> Result<T, SampleStatus> {
    let at = address(base, offset, std::mem::size_of::<T>())?;
    // SAFETY: the caller provides the live initialized typed range; alignment is not assumed.
    Ok(unsafe { std::ptr::read_unaligned(at.cast::<T>()) })
}

/// Copy the three initialized ActorInput fields within its reflected 0x1c8-byte allocation.
///
/// # Safety
///
/// `input` is a live ActorInput supplied by the current native owner/accessor, never a stale capture.
unsafe fn request(input: *const u8) -> Result<Request, SampleStatus> {
    // SAFETY: all three initialized fields lie inside the caller's live ActorInput.
    unsafe {
        Ok(Request {
            jump_type: read_native(input, REQUEST_TYPE)?,
            time: read_native(input, REQUEST_TIME)?,
            z_bits: read_native(input, LOCAL_Z)?,
        })
    }
}

/// Mirror the exact native EntityId leaf with checked address arithmetic and no new native call.
///
/// # Safety
///
/// `owner` supplies ProgramContext* at+0. Its native one-based index/table contract is live for
/// this owner invocation, and prepare selected the matching exact image's layout.
unsafe fn entity(owner: *const u8) -> Option<u32> {
    let table_offset = ENTITY_TABLE.load(Ordering::Acquire);
    if !matches!(table_offset, 0x828 | 0x830) {
        return None;
    }
    // SAFETY: the native owner supplies its live ProgramContext and native-valid table index.
    unsafe {
        let program: *const u8 = read_native(owner, 0).ok()?;
        let index: u32 = read_native(program, 8).ok()?;
        let index = index.checked_sub(1)?;
        let registry: *const u8 = read_native(program, 0).ok()?;
        let table: *const u8 = read_native(registry, table_offset).ok()?;
        let offset = usize::try_from(index).ok()?.checked_mul(16)?;
        let id: u32 = read_native(table, offset).ok()?;
        (id != 0).then_some(id)
    }
}

/// Observe the accepted helper while preserving its original return byte.
///
/// # Safety
///
/// iterator/view/component memory belongs to the current native owner until this call returns.
/// No foreign unwind crosses Rust. Native originals invoke the matching accessor themselves.
unsafe fn observe_accepted(iterator: *mut u8, original: impl FnOnce() -> u8) -> u8 {
    if !ENABLED.load(Ordering::Acquire) {
        return original();
    }
    let Some(scope) = Scope::enter(address(iterator, INPUT_VIEW, std::mem::size_of::<usize>()))
    else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return original();
    };
    let native_al = original();
    if !ENABLED.load(Ordering::Acquire) {
        return native_al;
    }
    let frame = scope.frame();
    if native_al == 0 && scope.missing.is_none() && frame.is_some_and(|frame| frame.calls == 0) {
        // No native publication was observed. Keep informative accepted/missing/error cases,
        // but do not fill the event queue or add native identity reads for these empty checks.
        // This does not infer whether the player was idle or why the native helper refused.
        drop(scope);
        UNPUBLISHED.fetch_add(1, Ordering::Relaxed);
        return native_al;
    }
    let mut status = scope.missing.unwrap_or(SampleStatus::NoAccessor);
    let mut copied = None;
    if let Some(frame) = frame.filter(|frame| frame.calls != 0) {
        // SAFETY: the caller is still suspended in this owner; this is its exact native view.
        let current: Result<*const u8, _> = unsafe { read_native(frame.expected_view, 0) };
        match current {
            Ok(input) if input != frame.last_input => status = SampleStatus::PointerChanged,
            Ok(input) => {
                // SAFETY: same live owner slot and last native accessor return, before advance.
                match unsafe { request(input) } {
                    Ok(value) => {
                        status = SampleStatus::Captured;
                        copied = Some(value);
                    }
                    Err(why) => status = why,
                }
            }
            Err(why) => status = why,
        }
    }
    // SAFETY: ProgramContext and its table still belong to this suspended native owner.
    let identity = unsafe { entity(iterator) };
    let record = Record::Accepted(Accepted {
        entity: identity,
        accepted: native_al != 0,
        native_al,
        status,
        request: copied,
        first_z_bits: frame.and_then(|frame| frame.first_z),
        accessor_calls: frame.map_or(0, |frame| frame.calls),
    });
    drop(scope);
    publish(record);
    native_al
}

/// Observe only a native-invoked accessor return inside the matching lexical native invocation.
///
/// # Safety
///
/// The native view and returned ActorInput belong to the current native owner. They are not
/// retained past that owner; this observer never invokes an additional accessor.
unsafe fn observe_accessor(view: *mut u8, original: impl FnOnce() -> *mut u8) -> *mut u8 {
    let input = original();
    if !ENABLED.load(Ordering::Acquire) {
        return input;
    }
    let _ = CURRENT.try_with(|current| {
        let mut state = current.get();
        let Some(mut frame) = state.frame.filter(|frame| frame.expected_view == view) else {
            return;
        };
        frame.calls = frame.calls.saturating_add(1);
        frame.last_input = input;
        if frame.calls == 1 {
            // SAFETY: exact matching owner view, and its native accessor just returned this
            // live ActorInput; native publication follows immediately after this observer.
            frame.first_z = unsafe { read_native(input, LOCAL_Z) }.ok();
        }
        state.frame = Some(frame);
        current.set(state);
    });
    input
}

/// Copy the input at dispatch entry and the original successful calculated delta at return.
///
/// # Safety
///
/// context+0x20 is the native owner's live ActorInput pointer. The original returns its live 20-byte
/// result; only ErrorId and the successful delta are read. Native application has not run yet.
unsafe fn observe_dispatcher(
    context: *mut u8,
    jump_type: u8,
    original: impl FnOnce() -> *mut u8,
) -> *mut u8 {
    if !ENABLED.load(Ordering::Acquire) {
        return original();
    }
    // SAFETY: the current dispatcher owner supplies the typed context and ActorInput.
    let sample = unsafe {
        read_native::<*const u8>(context, DISPATCH_INPUT).and_then(|input| request(input))
    };
    // SAFETY: this dispatcher context supplies the live native ProgramContext at+0.
    let identity = unsafe { entity(context) };
    let output = original();
    if !ENABLED.load(Ordering::Acquire) {
        return output;
    }
    // SAFETY: the original's returned pointer is its live native result buffer.
    let error = unsafe { read_native::<u8>(output, 0) }.ok();
    let delta_bits = if error == Some(0) {
        // SAFETY: only native success initializes these three float fields in out20.
        unsafe {
            read_native::<u32>(output, 8)
                .and_then(|x| read_native::<u32>(output, 12).map(|y| (x, y)))
                .and_then(|(x, y)| read_native::<u32>(output, 16).map(|z| [x, y, z]))
                .ok()
        }
    } else {
        None
    };
    let (status, copied) = match sample {
        Ok(value) => (SampleStatus::Captured, Some(value)),
        Err(why) => (why, None),
    };
    publish(Record::CalculatedDelta(CalculatedDelta {
        entity: identity,
        jump_type,
        status,
        request: copied,
        error,
        delta_bits,
    }));
    output
}

/// Guard every extern body, retaining a completed original result if observation panics.
fn guarded_original<T: Copy>(
    fallback: T,
    original: impl FnOnce() -> T,
    observe: impl FnOnce(&dyn Fn() -> T) -> T,
) -> T {
    let pending = Cell::new(Some(original));
    let answer = Cell::new(None);
    let run = || {
        let Some(original) = pending.take() else {
            return answer.get().unwrap_or(fallback);
        };
        let value = original();
        answer.set(Some(value));
        value
    };
    guard(Feature::Diag, run, || observe(&run))
}

/// The accepted helper's exact Windows x64 byte-return ABI.
type AcceptedFn = unsafe extern "system" fn(*mut u8) -> u8;
/// The native pointer-return ActorInput accessor ABI.
type AccessorFn = unsafe extern "system" fn(*mut u8) -> *mut u8;
/// The native pointer-return calculated Jump delta ABI.
type DispatcherFn = unsafe extern "system" fn(*mut u8, *mut u8, u8) -> *mut u8;
/// Published before the optional accepted hook is enabled.
static ACCEPTED: Slot = Slot::new();
/// Published before the optional accessor hook is enabled.
static ACCESSOR: Slot = Slot::new();
/// Published before the optional dispatcher hook is enabled.
static DISPATCHER: Slot = Slot::new();

/// The original accepted helper; a missing trampoline can occur only before hook enablement.
///
/// # Safety
///
/// Startup preflight and hook installation establish this exact original's ABI and live arguments.
unsafe fn original_accepted(iterator: *mut u8) -> u8 {
    let target = ACCEPTED.get();
    if target == 0 {
        return 0;
    }
    // SAFETY: startup stored this exact verified function's MinHook trampoline before enablement.
    let original: AcceptedFn = unsafe { std::mem::transmute(target) };
    // SAFETY: unchanged native arguments, with the native no-foreign-unwind contract.
    unsafe { original(iterator) }
}

/// The original native accessor, with all history bookkeeping preserved.
///
/// # Safety
///
/// Startup supplied the exact ABI/trampoline and the caller supplies its live native view.
unsafe fn original_accessor(view: *mut u8) -> *mut u8 {
    let target = ACCESSOR.get();
    if target == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the verified call-follow target has this exact ABI, published before enablement.
    let original: AccessorFn = unsafe { std::mem::transmute(target) };
    // SAFETY: unchanged native view; no extra accessor call is made.
    unsafe { original(view) }
}

/// The original native calculation, without changing any result bytes.
///
/// # Safety
///
/// Startup supplied the exact original ABI; the native caller owns out20/context for this call.
unsafe fn original_dispatcher(out: *mut u8, context: *mut u8, jump_type: u8) -> *mut u8 {
    let target = DISPATCHER.get();
    if target == 0 {
        return out;
    }
    // SAFETY: this exact verified native function has the declared ABI and a published trampoline.
    let original: DispatcherFn = unsafe { std::mem::transmute(target) };
    // SAFETY: all original native arguments are forwarded unchanged exactly once.
    unsafe { original(out, context, jump_type) }
}

/// Diagnostic post-hook on native request acceptance.
///
/// # Safety
///
/// The live native iterator and no-foreign-unwind contract are those documented in re-notes.
unsafe extern "system" fn accepted_detour(iterator: *mut u8) -> u8 {
    guarded_original(
        0,
        // SAFETY: the native caller supplies this original helper's unchanged iterator.
        || unsafe { original_accepted(iterator) },
        // SAFETY: the same owner remains suspended until this detour returns to it.
        |original| unsafe { observe_accepted(iterator, original) },
    )
}

/// Diagnostic observer on only the accessor returns native code itself invokes.
///
/// # Safety
///
/// The native caller supplies a live view and startup verified this exact accessor ABI.
unsafe extern "system" fn accessor_detour(view: *mut u8) -> *mut u8 {
    guarded_original(
        std::ptr::null_mut(),
        // SAFETY: original native arguments and verified trampoline.
        || unsafe { original_accessor(view) },
        // SAFETY: any sampling is restricted to the exact current lexical native owner/view.
        |original| unsafe { observe_accessor(view, original) },
    )
}

/// Diagnostic observer before native application of the calculated Jump delta.
///
/// # Safety
///
/// The native caller owns context/out20; this observer does not claim foreign-exception handling.
unsafe extern "system" fn dispatcher_detour(
    out: *mut u8,
    context: *mut u8,
    jump_type: u8,
) -> *mut u8 {
    guarded_original(
        out,
        // SAFETY: original native arguments, including its byte Jump type, are unchanged.
        || unsafe { original_dispatcher(out, context, jump_type) },
        // SAFETY: this native invocation owns the input and returned initialized result fields.
        |original| unsafe { observe_dispatcher(context, jump_type, original) },
    )
}

/// Test-only owned native seam fixtures. No game code, process, native FFI or hooks are invoked.
#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{Mutex, MutexGuard};

    /// Keep the module's test-only global configuration separate between tests.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Start with empty diagnostics and no native identity layout.
    fn reset() -> MutexGuard<'static, ()> {
        let serial = SERIAL.lock().unwrap();
        set_enabled(false);
        while take().is_some() {}
        take_dropped();
        take_unpublished();
        ENTITY_TABLE.store(0, Ordering::Release);
        CURRENT.with(|current| {
            assert_eq!(current.get().depth, 0);
            assert!(current.get().frame.is_none());
        });
        set_enabled(true);
        serial
    }

    /// Stable owned allocations for one iterator and ActorInput.
    struct Owner {
        /// Native input's full reflected storage.
        input: Box<[u8; 0x1c8]>,
        /// Only the initialized slots consumed by this seam.
        iterator: Box<[u8; 0x120]>,
    }

    impl Owner {
        /// A fixture whose view names its owned input, with no ProgramContext identity.
        fn new() -> Self {
            let mut owner = Self {
                input: Box::new([0; 0x1c8]),
                iterator: Box::new([0; 0x120]),
            };
            // SAFETY: both arrays own every written byte; boxed backing storage stays in place.
            unsafe {
                std::ptr::write_unaligned(
                    owner.iterator.as_mut_ptr().add(INPUT_VIEW).cast(),
                    owner.input.as_mut_ptr(),
                );
                std::ptr::write_unaligned(
                    owner.input.as_mut_ptr().add(LOCAL_Z).cast(),
                    1.0f32.to_bits(),
                );
            }
            owner
        }

        /// Raw pointers stay inside a test's synchronous owner invocation.
        fn pointers(&mut self) -> (*mut u8, *mut u8, *mut u8) {
            let iterator = self.iterator.as_mut_ptr();
            // SAFETY: INPUT_VIEW and its pointer-sized slot fit this owned array.
            let view = unsafe { iterator.add(INPUT_VIEW) };
            (iterator, view, self.input.as_mut_ptr())
        }
    }

    /// Require a copied accepted record rather than matching another record variant accidentally.
    fn accepted_record() -> Accepted {
        match take().expect("accepted record") {
            Record::Accepted(record) => record,
            Record::CalculatedDelta(_) => panic!("wrong observation kind"),
        }
    }

    /// A small distinct owned scalar record for queue ordering tests.
    fn record(id: u32) -> Record {
        Record::Accepted(Accepted {
            entity: Some(id),
            accepted: false,
            native_al: 0,
            status: SampleStatus::NoAccessor,
            request: None,
            first_z_bits: None,
            accessor_calls: 0,
        })
    }

    /// Acceptance must capture final publication after the native helper, with one original call.
    #[test]
    fn acceptance_copies_final_publication_after_one_native_call() {
        let _serial = reset();
        let mut owner = Owner::new();
        let (iterator, view, input) = owner.pointers();
        let iterator_before = owner.iterator.clone();
        let calls = Cell::new(0u8);
        // SAFETY: initialized owned buffers model the public native seam; no native execution.
        let accepted = unsafe {
            observe_accepted(iterator, || {
                calls.set(calls.get() + 1);
                assert_eq!(observe_accessor(view, || input), input);
                std::ptr::write_unaligned(input.add(REQUEST_TYPE), 13u8);
                std::ptr::write_unaligned(input.add(REQUEST_TIME).cast(), 91i64);
                1
            })
        };
        assert_eq!(accepted, 1);
        assert_eq!(calls.get(), 1);
        assert_eq!(owner.iterator, iterator_before);
        assert_eq!(
            accepted_record(),
            Accepted {
                entity: None,
                accepted: true,
                native_al: 1,
                status: SampleStatus::Captured,
                request: Some(Request {
                    jump_type: 13,
                    time: 91,
                    z_bits: 1.0f32.to_bits()
                }),
                first_z_bits: Some(1.0f32.to_bits()),
                accessor_calls: 1,
            }
        );
        assert_eq!(take(), None);
    }

    /// An unrelated accessor is ignored; a changed live slot is never dereferenced after return.
    #[test]
    fn unrelated_view_and_changed_pointer_do_not_publish_input_fields() {
        let _serial = reset();
        let mut owner = Owner::new();
        let mut other = Owner::new();
        let (iterator, view, input) = owner.pointers();
        let (_, other_view, _) = other.pointers();
        // SAFETY: the unrelated view is never sampled; the valid owner remains live throughout.
        unsafe {
            observe_accepted(iterator, || {
                observe_accessor(other_view, std::ptr::null_mut);
                0
            })
        };
        assert_eq!(take(), None);
        assert_eq!(
            take_unpublished(),
            1,
            "an unrelated accessor is not a publication"
        );
        // SAFETY: the same live owned fixture; accepted-but-missing data remains visible.
        unsafe {
            observe_accepted(iterator, || {
                observe_accessor(other_view, std::ptr::null_mut);
                1
            })
        };
        let ignored = accepted_record();
        assert_eq!(ignored.status, SampleStatus::NoAccessor);
        assert_eq!(ignored.request, None);
        assert_eq!(ignored.accessor_calls, 0);
        // SAFETY: the owner slot is valid; null models a changed current slot without a stale read.
        let returned = unsafe {
            observe_accepted(iterator, || {
                observe_accessor(view, || input);
                std::ptr::write_unaligned(view.cast(), std::ptr::null_mut::<u8>());
                0xe1
            })
        };
        assert_eq!(returned, 0xe1);
        let changed = accepted_record();
        assert_eq!(changed.native_al, 0xe1);
        assert!(changed.accepted);
        assert_eq!(changed.status, SampleStatus::PointerChanged);
        assert_eq!(changed.request, None);
        assert_eq!(changed.first_z_bits, Some(1.0f32.to_bits()));
    }

    /// Nested owners cannot lend accessors to each other, and the outer frame resumes afterward.
    #[test]
    fn nested_native_invocations_restore_their_exact_outer_frame() {
        let _serial = reset();
        let mut outer = Owner::new();
        let mut inner = Owner::new();
        let (outer_iterator, outer_view, outer_input) = outer.pointers();
        let (inner_iterator, inner_view, inner_input) = inner.pointers();
        // SAFETY: both owned iterators/components remain live for their nested lexical calls.
        let returned = unsafe {
            observe_accepted(outer_iterator, || {
                observe_accessor(outer_view, || outer_input);
                assert_eq!(
                    observe_accepted(inner_iterator, || {
                        observe_accessor(outer_view, std::ptr::null_mut);
                        observe_accessor(inner_view, || inner_input);
                        0
                    }),
                    0
                );
                observe_accessor(outer_view, || outer_input);
                0xff
            })
        };
        assert_eq!(returned, 0xff);
        let inner = accepted_record();
        assert!(!inner.accepted);
        assert_eq!(inner.status, SampleStatus::Captured);
        assert_eq!(inner.accessor_calls, 1);
        let outer = accepted_record();
        assert_eq!(outer.native_al, 0xff);
        assert_eq!(outer.status, SampleStatus::Captured);
        assert_eq!(outer.accessor_calls, 2);
        CURRENT.with(|current| assert!(current.get().frame.is_none()));
    }

    /// Exceeding the collection depth shadows outer pointers while native work still runs once.
    #[test]
    fn bounded_depth_drops_input_sampling_and_restores_empty_tls() {
        let _serial = reset();
        let mut owner = Owner::new();
        let (iterator, view, input) = owner.pointers();
        let mut scopes = Vec::new();
        for _ in 0..MAX_DEPTH {
            scopes.push(Scope::enter(Ok(view)).unwrap());
        }
        let calls = Cell::new(0);
        // SAFETY: the owned seam remains live; the exhausted lexical frame must ignore input.
        assert_eq!(
            unsafe {
                observe_accepted(iterator, || {
                    calls.set(calls.get() + 1);
                    observe_accessor(view, || input);
                    1
                })
            },
            1
        );
        assert_eq!(calls.get(), 1);
        let record = accepted_record();
        assert_eq!(record.status, SampleStatus::NestedLimit);
        assert_eq!(record.request, None);
        assert_eq!(record.accessor_calls, 0);
        while let Some(scope) = scopes.pop() {
            drop(scope);
        }
        CURRENT.with(|current| {
            assert_eq!(current.get().depth, 0);
            assert!(current.get().frame.is_none());
        });
    }

    /// A Rust unwind restores TLS before later unrelated accessors can be observed.
    #[test]
    fn rust_unwind_clears_the_lexical_native_borrow() {
        let _serial = reset();
        let mut owner = Owner::new();
        let (iterator, view, input) = owner.pointers();
        let calls = Cell::new(0);
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: owned initialized seam; the test unwinds through Rust, never native FFI.
            unsafe {
                observe_accepted(iterator, || {
                    calls.set(calls.get() + 1);
                    observe_accessor(view, || input);
                    panic!("owned seam unwind")
                })
            }
        }));
        assert!(result.is_err());
        assert_eq!(calls.get(), 1);
        assert_eq!(take(), None);
        CURRENT.with(|current| {
            assert_eq!(current.get().depth, 0);
            assert!(current.get().frame.is_none());
        });
        // SAFETY: outside an owner scope no component can be sampled, including this null return.
        unsafe { observe_accessor(view, std::ptr::null_mut) };
        assert_eq!(take(), None);
    }

    /// Disabled observers forward once without reading otherwise invalid arguments.
    #[test]
    fn disabled_observers_only_forward_the_original_result() {
        let _serial = reset();
        set_enabled(false);
        let calls = Cell::new(0);
        let pointer = std::ptr::dangling_mut::<u8>();
        // SAFETY: disabling observation prevents every native-memory read in these owned seams.
        unsafe {
            assert_eq!(
                observe_accepted(std::ptr::null_mut(), || {
                    calls.set(calls.get() + 1);
                    0xab
                }),
                0xab
            );
            assert_eq!(
                observe_accessor(std::ptr::null_mut(), || {
                    calls.set(calls.get() + 1);
                    pointer
                }),
                pointer
            );
            assert_eq!(
                observe_dispatcher(std::ptr::null_mut(), 13, || {
                    calls.set(calls.get() + 1);
                    pointer
                }),
                pointer
            );
        }
        assert_eq!(calls.get(), 3);
        assert_eq!(take(), None);
    }

    /// The fixed queue preserves FIFO across wrap and refuses full/busy operations immediately.
    #[test]
    fn bounded_ring_preserves_fifo_and_uses_one_nonblocking_claim() {
        let ring = Ring::new();
        for id in 0..CAPACITY {
            assert!(ring.push(record(id as u32)));
        }
        assert!(!ring.push(record(100)));
        let claim = Claim::take(&ring.busy).unwrap();
        assert_eq!(ring.take(), None);
        assert!(!ring.push(record(101)));
        drop(claim);
        for id in 0..8 {
            assert_eq!(ring.take(), Some(record(id)));
        }
        for id in 32..40 {
            assert!(ring.push(record(id)));
        }
        for id in 8..40 {
            assert_eq!(ring.take(), Some(record(id)));
        }
        assert_eq!(ring.take(), None);
    }

    /// Full/busy publication losses are explicit scalars and taking the counter resets it once.
    #[test]
    fn queue_loss_is_reported_separately_from_missing_native_fields() {
        let _serial = reset();
        for id in 0..CAPACITY {
            publish(record(id as u32));
        }
        publish(record(99));
        let claim = Claim::take(&RECORDS.busy).unwrap();
        publish(record(100));
        drop(claim);
        assert_eq!(take_dropped(), 2);
        assert_eq!(take_dropped(), 0);
        for id in 0..CAPACITY {
            assert_eq!(take(), Some(record(id as u32)));
        }
        assert_eq!(take(), None);
    }

    /// Dispatcher sampling copies only success delta bits and leaves all native bytes unchanged.
    #[test]
    fn dispatcher_reports_original_calculated_delta_and_never_changes_buffers() {
        let _serial = reset();
        let mut owner = Owner::new();
        let (_, _, input) = owner.pointers();
        let mut context = [0u8; 0x28];
        let mut output = [0xa5u8; 20];
        let delta = [
            1.25f32.to_bits(),
            f32::INFINITY.to_bits(),
            f32::NAN.to_bits(),
        ];
        // SAFETY: all initialized fields fit these owned arrays; no native execution occurs.
        unsafe {
            std::ptr::write_unaligned(context.as_mut_ptr().add(DISPATCH_INPUT).cast(), input);
            output[0] = 0;
            for (offset, value) in [8, 12, 16].into_iter().zip(delta) {
                std::ptr::write_unaligned(output.as_mut_ptr().add(offset).cast(), value);
            }
        }
        let context_before = context;
        let output_before = output;
        let input_before = owner.input.clone();
        let calls = Cell::new(0);
        let output_pointer = output.as_mut_ptr();
        // SAFETY: the owned typed context/input/result are live throughout the original seam.
        assert_eq!(
            unsafe {
                observe_dispatcher(context.as_mut_ptr(), 13, || {
                    calls.set(calls.get() + 1);
                    output_pointer
                })
            },
            output_pointer
        );
        assert_eq!(calls.get(), 1);
        assert_eq!(context, context_before);
        assert_eq!(output, output_before);
        assert_eq!(owner.input, input_before);
        let Record::CalculatedDelta(record) = take().unwrap() else {
            panic!("wrong kind")
        };
        assert_eq!(record.jump_type, 13);
        assert_eq!(record.status, SampleStatus::Captured);
        assert_eq!(record.error, Some(0));
        assert_eq!(record.delta_bits, Some(delta));
        assert_eq!(record.request.unwrap().z_bits, 1.0f32.to_bits());
    }

    /// An initialized native error byte never makes its unsuccessful payload a delta.
    #[test]
    fn dispatcher_error_does_not_publish_payload_or_invent_missing_input() {
        let _serial = reset();
        let mut error_result = [0xa5u8; 20];
        error_result[0] = 7;
        let before = error_result;
        let output = error_result.as_mut_ptr();
        // SAFETY: null context is refused arithmetically; the owned result has its error byte.
        assert_eq!(
            unsafe { observe_dispatcher(std::ptr::null_mut(), 12, || output) },
            output
        );
        let Record::CalculatedDelta(record) = take().unwrap() else {
            panic!("wrong kind")
        };
        assert_eq!(record.status, SampleStatus::NullPointer);
        assert_eq!(record.request, None);
        assert_eq!(record.error, Some(7));
        assert_eq!(record.delta_bits, None);
        assert_eq!(error_result, before);
    }

    /// Exact previous/current EntityId mirrors use the one-based native row and preserve unknown.
    #[test]
    fn entity_identity_uses_the_admitted_layout_and_checked_one_based_index() {
        let _serial = reset();
        let mut program = [0usize; 2];
        let mut registry = [0u8; 0x838];
        let mut table = [0u8; 32];
        let owner = [program.as_mut_ptr() as usize];
        program[0] = registry.as_mut_ptr() as usize;
        // SAFETY: the fixture owns all native-shaped initialized fields and both possible layouts.
        unsafe {
            std::ptr::write_unaligned(program.as_mut_ptr().cast::<u8>().add(8).cast(), 2u32);
            std::ptr::write_unaligned(table.as_mut_ptr().add(16).cast(), 42u32);
            for layout in [Layout::Previous, Layout::Current] {
                std::ptr::write_unaligned(
                    registry
                        .as_mut_ptr()
                        .add(layout.entity_table_offset())
                        .cast(),
                    table.as_mut_ptr(),
                );
                prepare(layout);
                assert_eq!(entity(owner.as_ptr().cast()), Some(42));
            }
            std::ptr::write_unaligned(program.as_mut_ptr().cast::<u8>().add(8).cast(), 0u32);
            assert_eq!(entity(owner.as_ptr().cast()), None);
        }
        assert_eq!(
            address(std::ptr::null(), 0, 1),
            Err(SampleStatus::NullPointer)
        );
        assert_eq!(
            address(std::ptr::without_provenance(usize::MAX), 1, 1),
            Err(SampleStatus::AddressOverflow)
        );
    }

    /// Repeated observation of an already completed original returns its cached result.
    #[test]
    fn guarded_original_cannot_invoke_native_twice() {
        let _serial = reset();
        let calls = Cell::new(0);
        let result = guarded_original(
            0u8,
            || {
                calls.set(calls.get() + 1);
                0xe1
            },
            |original| {
                assert_eq!(original(), 0xe1);
                assert_eq!(original(), 0xe1);
                0xe1
            },
        );
        assert_eq!(result, 0xe1);
        assert_eq!(calls.get(), 1);
    }

    /// Empty rejected checks must not consume the bounded queue before an actionable event.
    #[test]
    fn unpublished_checks_cannot_fill_the_jump_record_queue() {
        let _serial = reset();
        let mut owner = Owner::new();
        let (iterator, view, input) = owner.pointers();
        let before_input = owner.input.clone();
        let before_iterator = owner.iterator.clone();
        let calls = Cell::new(0);
        for _ in 0..128 {
            // SAFETY: this synchronous fixture owns the whole iterator/view/input storage.
            assert_eq!(
                unsafe {
                    observe_accepted(iterator, || {
                        calls.set(calls.get() + 1);
                        0
                    })
                },
                0
            );
        }
        assert_eq!(calls.get(), 128);
        assert_eq!(
            take(),
            None,
            "checks without publication must not flood the queue"
        );
        assert_eq!(
            take_dropped(),
            0,
            "intentional omissions are distinct from lost events"
        );
        assert_eq!(take_unpublished(), 128);
        assert_eq!(take_unpublished(), 0);
        assert_eq!(owner.input, before_input);
        assert_eq!(owner.iterator, before_iterator);
        // A rejected helper which did invoke its publication accessor is still informative.
        // SAFETY: matching initialized native-shaped owner; no real native function executes.
        let result = unsafe {
            observe_accepted(iterator, || {
                observe_accessor(view, || input);
                0
            })
        };
        assert_eq!(result, 0);
        let record = accepted_record();
        assert!(!record.accepted && record.accessor_calls == 1);
        assert_eq!(record.status, SampleStatus::Captured);
        assert_eq!(take_unpublished(), 0);
        // Accepted results without an accessor remain visible as missing-field observations.
        // SAFETY: the same owned iterator remains live until this call completes.
        assert_eq!(unsafe { observe_accepted(iterator, || 1) }, 1);
        assert_eq!(accepted_record().status, SampleStatus::NoAccessor);
        assert_eq!(take(), None);
    }

    /// Omission releases the inner lexical borrow before restoring an outer matching accessor.
    #[test]
    fn omitted_nested_check_restores_outer_publication_scope() {
        let _serial = reset();
        let mut outer = Owner::new();
        let mut inner = Owner::new();
        let (iterator, view, input) = outer.pointers();
        let (nested, _, _) = inner.pointers();
        // SAFETY: both fully initialized owned native-shaped owners remain alive and distinct.
        let result = unsafe {
            observe_accepted(iterator, || {
                observe_accessor(view, || input);
                assert_eq!(observe_accepted(nested, || 0), 0);
                std::ptr::write_unaligned(input.add(REQUEST_TYPE), 13_u8);
                std::ptr::write_unaligned(input.add(REQUEST_TIME).cast(), 77_i64);
                1
            })
        };
        assert_eq!(result, 1);
        let record = accepted_record();
        assert_eq!(record.status, SampleStatus::Captured);
        assert_eq!(record.accessor_calls, 1);
        assert_eq!(record.request.unwrap().time, 77);
        assert_eq!(take(), None);
        assert_eq!(take_unpublished(), 1);
    }
}
