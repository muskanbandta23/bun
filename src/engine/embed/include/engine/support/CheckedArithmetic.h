#pragma once

#include "engine/support/OverflowHandler.h"

#include <concepts>
#include <cstdint>

namespace WTF {

// An integer that tells its handler when a result does not fit.
template<std::integral T, typename OverflowHandler = CrashOnOverflow>
class Checked : public OverflowHandler {
public:
    constexpr Checked() = default;

    template<std::integral U>
    constexpr Checked(U value)
    {
        *this += value;
    }

    template<typename U, typename OtherHandler>
    constexpr Checked(const Checked<U, OtherHandler>& other)
    {
        *this += other;
    }

    template<std::integral U = T>
    constexpr U value() const
    {
        U result;
        if (this->hasOverflowed() || __builtin_add_overflow(m_value, 0, &result)) [[unlikely]]
            CRASH();
        return result;
    }

    constexpr operator T() const { return value(); }

    constexpr Checked& operator+=(auto other) { return check(__builtin_add_overflow(m_value, operand(other), &m_value)); }
    constexpr Checked& operator-=(auto other) { return check(__builtin_sub_overflow(m_value, operand(other), &m_value)); }
    constexpr Checked& operator*=(auto other) { return check(__builtin_mul_overflow(m_value, operand(other), &m_value)); }

    friend constexpr Checked operator+(Checked a, std::integral auto b) { return a += b; }
    friend constexpr Checked operator-(Checked a, std::integral auto b) { return a -= b; }
    friend constexpr Checked operator*(Checked a, std::integral auto b) { return a *= b; }
    friend constexpr Checked operator+(std::integral auto a, Checked b) { return b += a; }
    friend constexpr Checked operator*(std::integral auto a, Checked b) { return b *= a; }
    friend constexpr Checked operator+(Checked a, Checked b) { return a += b; }
    friend constexpr Checked operator-(Checked a, Checked b) { return a -= b; }
    friend constexpr Checked operator*(Checked a, Checked b) { return a *= b; }

private:
    template<std::integral, typename> friend class Checked;

    template<std::integral U>
    constexpr U operand(U value) { return value; }

    template<typename U, typename OtherHandler>
    constexpr U operand(const Checked<U, OtherHandler>& other)
    {
        check(other.hasOverflowed());
        return other.m_value;
    }

    constexpr Checked& check(bool didOverflow)
    {
        if (didOverflow) [[unlikely]]
            this->overflowed();
        return *this;
    }

    T m_value { 0 };
};

using CheckedInt32 = Checked<int32_t, RecordOverflow>;
using CheckedUint32 = Checked<uint32_t, RecordOverflow>;
using CheckedSize = Checked<size_t, RecordOverflow>;

}

using WTF::Checked;
using WTF::CheckedInt32;
using WTF::CheckedSize;
using WTF::CheckedUint32;
