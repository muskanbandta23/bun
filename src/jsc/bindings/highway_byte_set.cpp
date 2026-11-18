// The kernels of `ByteSet` in src/highway/lib.rs and of `PlainBytes` in src/highway/url.rs.

// Few runs are long, and 512-bit vectors slow down the code around them. The list is that of highway_png.cpp.
#undef HWY_DISABLED_TARGETS
#define HWY_DISABLED_TARGETS (HWY_ALL_SVE | (HWY_AVX2 - 1))

#undef HWY_TARGET_INCLUDE
#define HWY_TARGET_INCLUDE "highway_byte_set.cpp"
#include <hwy/foreach_target.h>
#include <hwy/highway.h>
#include "highway_dispatch.h"

#include <stddef.h>
#include <stdint.h>

namespace bun {
static size_t LongByteSetPrefixLength(const uint8_t* text, size_t len, const uint8_t* set);
}

HWY_BEFORE_NAMESPACE();
namespace bun {
namespace HWY_NAMESPACE {

namespace hn = hwy::HWY_NAMESPACE;

// How far the target that the build is compiled for looks, without a call through the table.
static constexpr size_t kShort = 64;

template<class D>
static HWY_INLINE hn::Mask<D> IsOutside(D d, const uint8_t* at, hn::Vec<D> rows, hn::Vec<D> columns)
{
    const auto bytes = hn::LoadU(d, at);
    const auto row = hn::TableLookupBytes(rows, hn::ShiftRight<4>(bytes));
    const auto column = hn::TableLookupBytes(columns, hn::And(bytes, hn::Set(d, uint8_t { 15 })));
    return hn::Eq(hn::And(row, column), hn::Zero(d));
}

template<bool is_short, class D>
static HWY_INLINE size_t PrefixLength(D d, const uint8_t* text, size_t len, const uint8_t* set)
{
    const size_t N = hn::Lanes(d);
    HWY_DASSERT(len >= N);
    const auto rows = hn::LoadDup128(d, set);
    const auto columns = hn::LoadDup128(d, set + 16);
    for (size_t at = is_short ? 0 : kShort; at + N <= len; at += N) {
        if (is_short && at == kShort) return LongByteSetPrefixLength(text, len, set);
        const auto outside = IsOutside(d, text + at, rows, columns);
        if (!hn::AllFalse(d, outside)) return at + hn::FindKnownFirstTrue(d, outside);
    }
    // It ends with the text, so it overlaps the one before it.
    const intptr_t found = hn::FindFirstTrue(d, IsOutside(d, text + len - N, rows, columns));
    return found < 0 ? len : len - N + static_cast<size_t>(found);
}

size_t LongByteSetPrefixLengthImpl(const uint8_t* text, size_t len, const uint8_t* set)
{
    return PrefixLength<false>(hn::CappedTag<uint8_t, 32> {}, text, len, set);
}

#if HWY_TARGET == HWY_STATIC_TARGET
static HWY_INLINE size_t ByteSetPrefixLengthImpl(const uint8_t* text, size_t len, const uint8_t* set)
{
    return PrefixLength<true>(hn::CappedTag<uint8_t, 16> {}, text, len, set);
}

using D16 = hn::Full128<uint8_t>;

// A byte is plain if its row and its column share a bit. The lowest bit does not count behind a '.' or a '/'.
static HWY_INLINE uint64_t UrlStops(hn::Vec<D16> bytes, hn::Vec<D16> before, const uint8_t* set)
{
    const D16 d;
    const hn::RebindToSigned<D16> di;
    const auto row = hn::TableLookupBytes(hn::LoadU(d, set), hn::ShiftRight<4>(bytes));
    const auto column = hn::TableLookupBytes(hn::LoadU(d, set + 16), hn::And(bytes, hn::Set(d, uint8_t { 15 })));
    // '.' is '/' without its lowest bit, and a mask is -1.
    const auto is_leading = hn::Eq(hn::Or(before, hn::Set(d, uint8_t { 1 })), hn::Set(d, uint8_t { '/' }));
    const auto shared = hn::Add(hn::And(row, column), hn::VecFromMask(d, is_leading));
    return hn::BitsFromMask(di, hn::Lt(hn::BitCast(di, shared), hn::Set(di, int8_t { 1 })));
}

// How long a text is decides no branch: the vectors that would pass its end stop there and overlap.
template<size_t count>
static HWY_INLINE uint64_t UrlStopsAt(const uint8_t* text, size_t len, size_t at, const uint8_t* set)
{
    const D16 d;
    const size_t last = len - 16;
    if (HWY_UNLIKELY(at > last)) return UrlStops(hn::LoadU(d, text + last), hn::LoadU(d, text + last - 1), set) >> (at - last);
    const auto first = hn::LoadU(d, text + at);
    uint64_t stops = UrlStops(first, at == 0 ? hn::ShiftLeftBytes<1>(d, first) : hn::LoadU(d, text + at - 1), set);
    HWY_UNROLL(4)
    for (size_t step = 16; step < count; step += 16) {
        const size_t offset = HWY_MIN(step, last - at);
        // If there is nothing to add, `at` is `last`, which is not 0.
        stops |= UrlStops(hn::LoadU(d, text + at + offset), hn::LoadU(d, text + at + offset - 1), set) << offset;
    }
    return stops;
}

static HWY_INLINE uint64_t UrlStopsOfWords(uint64_t low, uint64_t high, const uint8_t* set)
{
    const D16 d;
    const auto bytes = hn::BitCast(d, hn::Dup128VecFromValues(hn::Full128<uint64_t> {}, low, high));
    return UrlStops(bytes, hn::ShiftLeftBytes<1>(d, bytes), set);
}
#endif

} // namespace HWY_NAMESPACE
} // namespace bun
HWY_AFTER_NAMESPACE();

#if HWY_ONCE
namespace bun {

HWY_EXPORT(LongByteSetPrefixLengthImpl);

static size_t LongByteSetPrefixLength(const uint8_t* text, size_t len, const uint8_t* set)
{
    return BUN_HWY_DISPATCH(LongByteSetPrefixLengthImpl)(text, len, set);
}

// `len` is at least 16 and `set` has 32 bytes. LTO has to inline this: a call takes as long as a short run.
extern "C" __attribute__((always_inline)) size_t highway_byte_set_prefix_length(const uint8_t* text, size_t len, const uint8_t* set)
{
    return HWY_STATIC_DISPATCH(ByteSetPrefixLengthImpl)(text, len, set);
}

// For these two, `len` is at least 17 and above `at`.
extern "C" __attribute__((always_inline)) uint64_t highway_url_stops_32(const uint8_t* text, size_t len, size_t at, const uint8_t* set)
{
    return HWY_STATIC_DISPATCH(UrlStopsAt)<32>(text, len, at, set);
}

extern "C" __attribute__((always_inline)) uint64_t highway_url_stops_64(const uint8_t* text, size_t len, size_t at, const uint8_t* set)
{
    return HWY_STATIC_DISPATCH(UrlStopsAt)<64>(text, len, at, set);
}

extern "C" __attribute__((always_inline)) uint64_t highway_url_stops_of_words(uint64_t low, uint64_t high, const uint8_t* set)
{
    return HWY_STATIC_DISPATCH(UrlStopsOfWords)(low, high, set);
}

} // namespace bun
#endif
