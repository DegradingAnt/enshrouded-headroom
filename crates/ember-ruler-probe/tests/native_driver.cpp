// Inert native fixtures, in their own archive object. Not referenced by a release DLL.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include "../src/native.hpp"
namespace {
/// One test call owns these counters; production code never refers to this object.
unsigned order=0, observations=0, originals=0;
/// Check native argument forwarding and finish before the observer.
void normal(void* storage,char ticked){if(storage!=reinterpret_cast<void*>(0x1234) || ticked!=7)throw 1;++originals;order=1;}
/// A foreign exception must stay entirely in C++ and skip observation.
void throwing(void*,char){++originals;order=1;throw 2;}
/// Refuse the wrong sequence, rather than just counting that both callbacks ran.
void observed(void*){if(order!=1)throw 3;++observations;order=2;}
/// A valid callable fixture raises a structured fault inside the getter boundary.
void* fault(void*,void*,std::uint32_t,std::uint32_t){RaiseException(EXCEPTION_ACCESS_VIOLATION,0,0,nullptr);return nullptr;}
/// A valid getter returns the same two-word descriptor as the source ABI.
void* getter(void* out,void*,std::uint32_t hash,std::uint32_t player){
    auto* words=static_cast<std::uint64_t*>(out);words[0]=hash==0x027ac564u && player==42 ? 0x7000 : 0;words[1]=56;return out;
}
}
/// Never permits a foreign exception to cross this test's C boundary.
extern "C" std::uint32_t ruler_cpp_checks() noexcept {
    try {
        order=0;observations=0;originals=0;
        ruler::dispatch(normal,observed,reinterpret_cast<void*>(0x1234),7);
        if(order!=2 || observations!=1 || originals!=1)return 1;
        order=0;observations=0;originals=0;
        try {ruler::dispatch(throwing,observed,nullptr,0);return 2;}catch(int value){if(value!=2)return 3;}
        if(order!=1 || observations!=0 || originals!=1)return 4;
        std::uint64_t out[2]={99,99};
        if(ruler_component(reinterpret_cast<std::uintptr_t>(&getter),reinterpret_cast<void*>(0x1234),42,out)!=1 || out[0]!=0x7000 || out[1]!=56)return 5;
        if(ruler_component(reinterpret_cast<std::uintptr_t>(&fault),reinterpret_cast<void*>(0x1234),42,out)!=0 || out[0]!=0 || out[1]!=0)return 6;
        return 0;
    }catch(...){return 7;}
}

