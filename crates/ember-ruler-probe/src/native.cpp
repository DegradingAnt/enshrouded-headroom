// Read-only ABI adapted from MIT EMF08d6001 common/world_view.h/.cpp and core/core.cpp.
// See LICENSE-EMF. No framework startup or mutation services are linked.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <atomic>
#include "native.hpp"
/// Rust catches all of its own panics before returning to this native frame.
extern "C" void ember_ruler_observe(void* storage);
namespace {
/// Published before activation and kept for the pinned module's lifetime.
std::atomic<ruler::Step> original_step{nullptr};
/// Game-facing native detour; no Rust frame surrounds the original engine call.
void step(void* storage, char ticked) {
    const auto original=original_step.load(std::memory_order_acquire);
    if (original != nullptr) ruler::dispatch(original, &ember_ruler_observe, storage, ticked);
}
}
void ruler::dispatch(Step original, Observer observer, void* storage, char ticked) {
    original(storage,ticked);
    observer(storage);
}
extern "C" std::uint8_t ruler_set_original(std::uintptr_t original) noexcept {
    if (original==0) return 0;
    ruler::Step empty=nullptr;
    return original_step.compare_exchange_strong(empty,reinterpret_cast<ruler::Step>(original),std::memory_order_release) ? 1 : 0;
}
extern "C" std::uintptr_t ruler_hook_address() noexcept { return reinterpret_cast<std::uintptr_t>(&step); }
extern "C" std::uint8_t ruler_component(std::uintptr_t target,void* world,std::uint32_t player,std::uint64_t* out) noexcept {
    if (target==0 || world==nullptr || out==nullptr) return 0;
    out[0]=0;out[1]=0;
    // All locals are POD. This leaf cannot throw a C++ exception or a Rust panic.
    __try {
        using Get=void* (*)(void*,void*,std::uint32_t,std::uint32_t);
        reinterpret_cast<Get>(target)(out,world,0x027ac564u,player);
        return 1;
    } __except(EXCEPTION_EXECUTE_HANDLER) { out[0]=0;out[1]=0;return 0; }
}

