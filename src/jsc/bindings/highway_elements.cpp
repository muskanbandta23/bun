// The kernels of src/highway/elements.rs.

// 512-bit vectors slow down the code around them. The list is that of highway_png.cpp.
#undef HWY_DISABLED_TARGETS
#define HWY_DISABLED_TARGETS (HWY_ALL_SVE | (HWY_AVX2 - 1))

#undef HWY_TARGET_INCLUDE
#define HWY_TARGET_INCLUDE "highway_elements.cpp"
#include <hwy/foreach_target.h>
#include <hwy/highway.h>
#include "highway_dispatch.h"

#include <stddef.h>
#include <stdint.h>

HWY_BEFORE_NAMESPACE();
namespace bun {
namespace HWY_NAMESPACE {

namespace hn = hwy::HWY_NAMESPACE;

// For these two, `len` is at least the number of lanes, and is the result if nothing matches.
template<class T, class Matches>
static HWY_INLINE size_t First(const T* elements, size_t len, Matches matches)
{
    const hn::ScalableTag<T> d;
    const size_t N = hn::Lanes(d);
    HWY_DASSERT(len >= N);
    const auto at = [&](size_t index) HWY_ATTR { return matches(hn::LoadU(d, elements + index)); };
    size_t index = 0;
    for (; index + 4 * N <= len; index += 4 * N) {
        const auto any = hn::Or(hn::Or(at(index), at(index + N)), hn::Or(at(index + 2 * N), at(index + 3 * N)));
        if (HWY_UNLIKELY(!hn::AllFalse(d, any))) break;
    }
    for (; index + N <= len; index += N) {
        const intptr_t found = hn::FindFirstTrue(d, at(index));
        if (found >= 0) return index + static_cast<size_t>(found);
    }
    const intptr_t found = hn::FindFirstTrue(d, at(len - N));
    return found < 0 ? len : len - N + static_cast<size_t>(found);
}

template<class T, class Matches>
static HWY_INLINE size_t Last(const T* elements, size_t len, Matches matches)
{
    const hn::ScalableTag<T> d;
    const size_t N = hn::Lanes(d);
    HWY_DASSERT(len >= N);
    const auto at = [&](size_t index) HWY_ATTR { return matches(hn::LoadU(d, elements + index)); };
    size_t end = len;
    for (; end >= 4 * N; end -= 4 * N) {
        const auto any = hn::Or(hn::Or(at(end - N), at(end - 2 * N)), hn::Or(at(end - 3 * N), at(end - 4 * N)));
        if (HWY_UNLIKELY(!hn::AllFalse(d, any))) break;
    }
    for (; end >= N; end -= N) {
        const intptr_t found = hn::FindLastTrue(d, at(end - N));
        if (found >= 0) return end - N + static_cast<size_t>(found);
    }
    const intptr_t found = hn::FindLastTrue(d, at(0));
    return found < 0 ? len : static_cast<size_t>(found);
}

template<class T>
static HWY_INLINE auto Equals(T needle)
{
    return [needles = hn::Set(hn::ScalableTag<T> {}, needle)](auto lanes) HWY_ATTR { return hn::Eq(lanes, needles); };
}

size_t IndexOfU64Impl(const uint64_t* elements, size_t len, uint64_t needle)
{
    return First(elements, len, Equals(needle));
}

size_t LastIndexOfU64Impl(const uint64_t* elements, size_t len, uint64_t needle)
{
    return Last(elements, len, Equals(needle));
}

size_t IndexOfF64Impl(const double* elements, size_t len, double needle)
{
    return First(elements, len, Equals(needle));
}

size_t LastIndexOfF64Impl(const double* elements, size_t len, double needle)
{
    return Last(elements, len, Equals(needle));
}

size_t IndexOfNaNImpl(const double* elements, size_t len)
{
    return First(elements, len, [](auto lanes) HWY_ATTR { return hn::IsNaN(lanes); });
}

size_t IndexOfEitherU64Impl(const uint64_t* elements, size_t len, uint64_t first, uint64_t second)
{
    return First(elements, len, [a = Equals(first), b = Equals(second)](auto lanes) HWY_ATTR { return hn::Or(a(lanes), b(lanes)); });
}

size_t LastIndexOfEitherU64Impl(const uint64_t* elements, size_t len, uint64_t first, uint64_t second)
{
    return Last(elements, len, [a = Equals(first), b = Equals(second)](auto lanes) HWY_ATTR { return hn::Or(a(lanes), b(lanes)); });
}

} // namespace HWY_NAMESPACE
} // namespace bun
HWY_AFTER_NAMESPACE();

#if HWY_ONCE
namespace bun {

HWY_EXPORT(IndexOfU64Impl);
HWY_EXPORT(LastIndexOfU64Impl);
HWY_EXPORT(IndexOfF64Impl);
HWY_EXPORT(LastIndexOfF64Impl);
HWY_EXPORT(IndexOfNaNImpl);
HWY_EXPORT(IndexOfEitherU64Impl);
HWY_EXPORT(LastIndexOfEitherU64Impl);

extern "C" size_t highway_index_of_u64(const uint64_t* elements, size_t len, uint64_t needle)
{
    return BUN_HWY_DISPATCH(IndexOfU64Impl)(elements, len, needle);
}

extern "C" size_t highway_last_index_of_u64(const uint64_t* elements, size_t len, uint64_t needle)
{
    return BUN_HWY_DISPATCH(LastIndexOfU64Impl)(elements, len, needle);
}

extern "C" size_t highway_index_of_f64(const double* elements, size_t len, double needle)
{
    return BUN_HWY_DISPATCH(IndexOfF64Impl)(elements, len, needle);
}

extern "C" size_t highway_last_index_of_f64(const double* elements, size_t len, double needle)
{
    return BUN_HWY_DISPATCH(LastIndexOfF64Impl)(elements, len, needle);
}

extern "C" size_t highway_index_of_nan(const double* elements, size_t len)
{
    return BUN_HWY_DISPATCH(IndexOfNaNImpl)(elements, len);
}

extern "C" size_t highway_index_of_either_u64(const uint64_t* elements, size_t len, uint64_t first, uint64_t second)
{
    return BUN_HWY_DISPATCH(IndexOfEitherU64Impl)(elements, len, first, second);
}

extern "C" size_t highway_last_index_of_either_u64(const uint64_t* elements, size_t len, uint64_t first, uint64_t second)
{
    return BUN_HWY_DISPATCH(LastIndexOfEitherU64Impl)(elements, len, first, second);
}

} // namespace bun
#endif
