#pragma once

#include "engine/text/AtomString.h"
#include "engine/text/NumberConversion.h"

// What makeString() and StringBuilder::append() take. The adapter of a type has `unsigned length()`, `bool is8Bit()`
// and `void writeTo(std::span<T>)`, which writes `length()` characters: Latin-1 if `is8Bit()`, or else UTF-16.

extern "C" {
size_t engine_text_valid_utf8_prefix(const char8_t*, size_t length, size_t& lengthInUTF16);
void engine_text_decode_valid_utf8(const char8_t*, size_t length, char16_t* destination, size_t lengthInUTF16);
}

namespace WTF {

template<> class StringTypeAdapter<char> {
public:
    StringTypeAdapter(char character)
        : m_character(character)
    {
    }

    static constexpr unsigned length() { return 1; }
    static constexpr bool is8Bit() { return true; }
    template<typename T> void writeTo(std::span<T> destination) const { destination[0] = m_character; }

private:
    Latin1Character m_character;
};

template<> class StringTypeAdapter<char16_t> {
public:
    StringTypeAdapter(char16_t character)
        : m_character(character)
    {
    }

    static constexpr unsigned length() { return 1; }
    bool is8Bit() const { return isLatin1(m_character); }
    template<typename T> void writeTo(std::span<T> destination) const { destination[0] = m_character; }

private:
    char16_t m_character;
};

template<> class StringTypeAdapter<char32_t> {
public:
    StringTypeAdapter(char32_t codePoint)
        : m_codePoint(codePoint)
    {
    }

    unsigned length() const { return U16_LENGTH(m_codePoint); }
    bool is8Bit() const { return isLatin1(m_codePoint); }
    template<typename T> void writeTo(std::span<T> destination) const
    {
        if (sizeof(T) == 1 || U_IS_BMP(m_codePoint))
            destination[0] = m_codePoint;
        else {
            destination[0] = U16_LEAD(m_codePoint);
            destination[1] = U16_TRAIL(m_codePoint);
        }
    }

private:
    char32_t m_codePoint;
};

template<std::integral Integer> class StringTypeAdapter<Integer> {
public:
    StringTypeAdapter(Integer number)
        : m_magnitude(number < 0 ? -static_cast<Magnitude>(number) : static_cast<Magnitude>(number))
        , m_isNegative(number < 0)
        , m_length(Detail::decimalLength(m_magnitude) + m_isNegative)
    {
    }

    unsigned length() const { return m_length; }
    static constexpr bool is8Bit() { return true; }
    template<typename T> void writeTo(std::span<T> destination) const
    {
        destination[0] = '-';
        auto* cursor = destination.data() + m_length;
        Magnitude rest = m_magnitude;
        do {
            *--cursor = static_cast<T>('0' + rest % 10);
            rest /= 10;
        } while (rest);
    }

private:
    using Magnitude = std::conditional_t<sizeof(Integer) <= 4, uint32_t, uint64_t>;

    Magnitude m_magnitude;
    bool m_isNegative;
    unsigned m_length;
};

template<typename Enum>
    requires std::is_enum_v<Enum>
class StringTypeAdapter<Enum> : public StringTypeAdapter<long long> {
public:
    StringTypeAdapter(Enum value)
        : StringTypeAdapter<long long>(std::to_underlying(value))
    {
    }
};

template<> class StringTypeAdapter<double> {
public:
    StringTypeAdapter(double number)
        : m_digits(number)
    {
    }

    unsigned length() const { return m_digits.length; }
    static constexpr bool is8Bit() { return true; }
    template<typename T> void writeTo(std::span<T> destination) const { StringImpl::copyCharacters(destination, m_digits.span()); }

private:
    Detail::ShortestDigits m_digits;
};

// Latin-1 or UTF-16, as the size of the elements says.
template<typename Element, size_t extent> class StringTypeAdapter<std::span<Element, extent>> {
public:
    ALWAYS_INLINE StringTypeAdapter(std::span<Element, extent> characters)
        : m_characters(spanReinterpretCast<const Character>(characters))
    {
        RELEASE_ASSERT(characters.size() <= String::MaxLength);
    }

    unsigned length() const { return m_characters.size(); }
    static constexpr bool is8Bit() { return sizeof(Element) == 1; }
    template<typename T> ALWAYS_INLINE void writeTo(std::span<T> destination) const { StringImpl::copyCharacters(destination, m_characters); }

private:
    using Character = std::conditional_t<sizeof(Element) == 1, Latin1Character, char16_t>;

    std::span<const Character> m_characters;
};

// Ends where the bytes cease to be UTF-8.
template<size_t extent> class StringTypeAdapter<std::span<const char8_t, extent>> {
public:
    StringTypeAdapter(std::span<const char8_t, extent> bytes)
        : m_bytes(bytes.first(engine_text_valid_utf8_prefix(bytes.data(), bytes.size(), m_length)))
    {
        RELEASE_ASSERT(m_length <= String::MaxLength);
    }

    unsigned length() const { return m_length; }
    bool is8Bit() const { return m_length == m_bytes.size(); }
    void writeTo(std::span<Latin1Character> destination) const { StringImpl::copyCharacters(destination, byteCast<Latin1Character>(m_bytes)); }
    void writeTo(std::span<char16_t> destination) const { engine_text_decode_valid_utf8(m_bytes.data(), m_bytes.size(), destination.data(), m_length); }

private:
    size_t m_length;
    std::span<const char8_t> m_bytes;
};

template<typename T>
    requires requires(const T& value) { typename decltype(value.span())::element_type; }
class StringTypeAdapter<T> : public StringTypeAdapter<decltype(std::declval<const T&>().span())> {
public:
    StringTypeAdapter(const T& value)
        : StringTypeAdapter<decltype(value.span())>(value.span())
    {
    }
};

template<> class StringTypeAdapter<ASCIILiteral> : public StringTypeAdapter<std::span<const Latin1Character>> {
public:
    ALWAYS_INLINE StringTypeAdapter(ASCIILiteral literal)
        : StringTypeAdapter<std::span<const Latin1Character>>(literal.span8())
    {
    }
};

template<> class StringTypeAdapter<StringView> {
public:
    StringTypeAdapter(StringView string)
        : m_string(string)
    {
    }

    unsigned length() const { return m_string.length(); }
    bool is8Bit() const { return m_string.is8Bit(); }
    ALWAYS_INLINE void writeTo(std::span<Latin1Character> destination) const { StringImpl::copyCharacters(destination, m_string.span8()); }
    void writeTo(std::span<char16_t> destination) const { m_string.getCharacters(destination); }

private:
    StringView m_string;
};

namespace Detail {

// Types that share an adapter share the code that concatenates them.
template<typename T> struct Adapted {
    using Type = T;
};
template<> struct Adapted<String> {
    using Type = StringView;
};
template<> struct Adapted<AtomString> {
    using Type = StringView;
};
template<std::derived_from<StringImpl> T> struct Adapted<T*> {
    using Type = StringView;
};

template<typename T> using AdapterFor = StringTypeAdapter<typename Adapted<T>::Type>;

// More than `String::MaxLength` if it is too much.
ALWAYS_INLINE size_t totalLength(const auto&... adapters) { return (size_t { 0 } + ... + adapters.length()); }
ALWAYS_INLINE bool are8Bit(const auto&... adapters) { return (... && adapters.is8Bit()); }

template<typename T> ALWAYS_INLINE void writeAll(T* destination, auto&... adapters)
{
    (..., (adapters.writeTo(std::span<T> { destination, adapters.length() }), destination += adapters.length()));
}

}

}
