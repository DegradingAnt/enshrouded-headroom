#pragma once
#include <cstdint>
namespace ruler {
/// Exact pinned source ABI of the game's simulation step.
using Step = void (*)(void*, char);
/// Our guarded Rust observation callback, invoked only after a normal return.
using Observer = void (*)(void*);
/// Call order is tested in the inert C++ driver, including native exception propagation.
void dispatch(Step original, Observer observer, void* storage, char ticked);
}
/// Set the trampoline before any hook is enabled. No pointer is ever reset.
extern "C" std::uint8_t ruler_set_original(std::uintptr_t original) noexcept;
/// Own detour address, with the exact Step signature.
extern "C" std::uintptr_t ruler_hook_address() noexcept;
/// Fault-guard only our read-only component lookup, never the game's simulation call.
/// target has the verified GetComponent ABI; world is live on its callback owner thread.
/// out points at two owned writable u64 words for the duration of this call.
extern "C" std::uint8_t ruler_component(std::uintptr_t target, void* world, std::uint32_t player, std::uint64_t* out) noexcept;

