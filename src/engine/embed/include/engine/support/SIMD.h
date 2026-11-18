#pragma once

#include "engine/support/ByteOrder.h"
#include "engine/support/Platform.h"

#include <cstddef>
#include <cstdint>
#include <optional>

// Vectors of 128 bits, which every processor has. A comparison sets all bits of a lane or none.
namespace WTF::SIMD {

using Vector8 = uint8_t __attribute__((vector_size(16)));
using Vector16 = uint16_t __attribute__((vector_size(16)));

template<typename Lane> using Vector = std::conditional_t<sizeof(Lane) == 1, Vector8, Vector16>;
template<typename Lane> inline constexpr size_t stride = 16 / sizeof(Lane);

template<typename T>
concept AnyVector = std::same_as<T, Vector8> || std::same_as<T, Vector16>;

template<typename Lane>
ALWAYS_INLINE constexpr Vector<Lane> splat(Lane value)
{
    return Vector<Lane> {} | value;
}

ALWAYS_INLINE Vector8 load(const uint8_t* address) { return unalignedLoad<Vector8>(address); }
ALWAYS_INLINE Vector16 load(const uint16_t* address) { return unalignedLoad<Vector16>(address); }

template<AnyVector V> ALWAYS_INLINE V sub(V a, V b) { return a - b; }
template<AnyVector V> ALWAYS_INLINE V bitAnd(V a, V b) { return a & b; }
template<AnyVector V> ALWAYS_INLINE V bitOr(V first, std::same_as<V> auto... others) { return (first | ... | others); }

template<AnyVector V> ALWAYS_INLINE V equal(V a, V b) { return std::bit_cast<V>(a == b); }
template<AnyVector V> ALWAYS_INLINE V greaterThan(V a, V b) { return std::bit_cast<V>(a > b); }
template<AnyVector V> ALWAYS_INLINE V lessThanOrEqual(V a, V b) { return std::bit_cast<V>(a <= b); }

// To any of the values.
template<auto... values> ALWAYS_INLINE Vector8 equal(Vector8 input) { return bitOr(equal(input, splat<uint8_t>(values))...); }
template<auto... values> ALWAYS_INLINE Vector16 equal(Vector16 input) { return bitOr(equal(input, splat<uint16_t>(values))...); }

namespace Detail {

// The same number of bits from each byte of the result of a comparison.
#if CPU(X86_64)
inline constexpr unsigned maskBitsPerByte = 1;

ALWAYS_INLINE unsigned mask(AnyVector auto comparison)
{
    using Chars = char __attribute__((vector_size(16)));
    return __builtin_ia32_pmovmskb128(std::bit_cast<Chars>(comparison));
}
#else
inline constexpr unsigned maskBitsPerByte = 4;

ALWAYS_INLINE uint64_t mask(AnyVector auto comparison)
{
    using HalfVector = uint8_t __attribute__((vector_size(8)));
    return std::bit_cast<uint64_t>(__builtin_convertvector(std::bit_cast<Vector16>(comparison) >> 4, HalfVector));
}
#endif

}

ALWAYS_INLINE bool isNonZero(AnyVector auto comparison) { return Detail::mask(comparison); }

ALWAYS_INLINE std::optional<uint8_t> findFirstNonZeroIndex(AnyVector auto comparison)
{
    auto mask = Detail::mask(comparison);
    if (!mask)
        return std::nullopt;
    return std::countr_zero(mask) / (Detail::maskBitsPerByte * sizeof(comparison[0]));
}

}

namespace SIMD = WTF::SIMD;
