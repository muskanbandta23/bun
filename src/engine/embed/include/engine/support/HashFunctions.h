#pragma once

#include "engine/support/Forward.h"
#include "engine/support/GetPtr.h"
#include "engine/support/Int128.h"
#include "engine/support/StdLibExtras.h"

#include <bit>

namespace WTF {

// The mixing step of rapidhash.
constexpr unsigned intHash(uint64_t key)
{
    UInt128 product = static_cast<UInt128>(key ^ 0x2d358dccaa6c78a5) * (key ^ 0x8bb84b93962eacc9);
    return static_cast<unsigned>(static_cast<uint64_t>(product) ^ static_cast<uint64_t>(product >> 64));
}

constexpr unsigned pairIntHash(unsigned first, unsigned second)
{
    return intHash(static_cast<uint64_t>(first) << 32 | second);
}

template<typename T> struct IntHash {
    static unsigned hash(T key) { return intHash(static_cast<typename SizedUnsignedTrait<sizeof(T)>::Type>(key)); }
    static bool equal(T a, T b) { return a == b; }
};

template<typename T> struct FloatHash {
    using Bits = typename SizedUnsignedTrait<sizeof(T)>::Type;
    static unsigned hash(T key) { return intHash(std::bit_cast<Bits>(key)); }
    static bool equal(T a, T b) { return std::bit_cast<Bits>(a) == std::bit_cast<Bits>(b); }
};

template<typename T> struct PtrHash {
    static unsigned hash(T key) { return intHash(reinterpret_cast<uintptr_t>(key)); }
    static bool equal(T a, T b) { return a == b; }
};

// Also takes the raw pointer as the key to look for.
template<SmartPtr T> struct PtrHash<T> {
    using RawPointer = const typename GetPtrHelper<T>::UnderlyingType*;
    static unsigned hash(RawPointer key) { return PtrHash<RawPointer>::hash(key); }
    static unsigned hash(const std::same_as<T> auto& key) { return hash(getPtr(key)); }
    static bool equal(const T& a, RawPointer b) { return getPtr(a) == b; }
    static bool equal(const T& a, const std::same_as<T> auto& b) { return getPtr(a) == getPtr(b); }
};

template<typename First, typename Second> struct PairHash {
    static unsigned hash(const std::pair<First, Second>& key) { return pairIntHash(DefaultHash<First>::hash(key.first), DefaultHash<Second>::hash(key.second)); }
    static bool equal(const std::pair<First, Second>& a, const std::pair<First, Second>& b) { return DefaultHash<First>::equal(a.first, b.first) && DefaultHash<Second>::equal(a.second, b.second); }
};

template<typename T>
concept HashableWithMemberFunction = std::equality_comparable<T> && requires(const T& value) {
    { value.hash() } -> std::same_as<unsigned>;
};

template<typename T>
    requires std::integral<T> || std::is_enum_v<T>
struct DefaultHash<T> : IntHash<T> {};
template<std::floating_point T> struct DefaultHash<T> : FloatHash<T> {};
template<typename T> struct DefaultHash<T*> : PtrHash<T*> {};
template<typename T> struct DefaultHash<Ref<T>> : PtrHash<Ref<T>> {};
template<typename T> struct DefaultHash<RefPtr<T>> : PtrHash<RefPtr<T>> {};
template<typename T, typename Deleter> struct DefaultHash<std::unique_ptr<T, Deleter>> : PtrHash<std::unique_ptr<T, Deleter>> {};
template<typename First, typename Second> struct DefaultHash<std::pair<First, Second>> : PairHash<First, Second> {};

template<HashableWithMemberFunction T> struct DefaultHash<T> {
    static unsigned hash(const T& key) { return key.hash(); }
    static bool equal(const T& a, const T& b) { return a == b; }
};

}

using WTF::DefaultHash;
using WTF::IntHash;
using WTF::intHash;
using WTF::pairIntHash;
using WTF::PtrHash;
