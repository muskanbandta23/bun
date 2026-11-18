#pragma once

#include "engine/text/AtomStringImpl.h"

namespace WTF {
class PrivateSymbolImpl;
}

extern "C" {
WTF::SymbolImpl* engine_text_symbol(const WTF::StringImpl& description) RETURNS_NONNULL;
WTF::SymbolImpl* engine_text_symbol_without_description() RETURNS_NONNULL;
WTF::PrivateSymbolImpl* engine_text_private_symbol(const WTF::StringImpl& description) RETURNS_NONNULL;
WTF::RegisteredSymbolImpl* engine_text_registered_symbol(const WTF::StringImpl& key) RETURNS_NONNULL;
}

namespace WTF {

// The identity of a symbol. Its characters are the description.
class SymbolImpl : public UniquedStringImpl {
public:
    using Flags = unsigned;
    static constexpr Flags s_flagDefault = 0;
    static constexpr Flags s_flagIsNullSymbol = 1 << 0;
    static constexpr Flags s_flagIsRegistered = 1 << 1;
    static constexpr Flags s_flagIsPrivate = 1 << 2;
    static constexpr Flags s_flagPassesThroughProxies = 1 << 3;

    static Ref<SymbolImpl> create(StringImpl& description) { return adoptRef(*engine_text_symbol(description)); }
    static Ref<SymbolImpl> createNullSymbol() { return adoptRef(*engine_text_symbol_without_description()); }

    unsigned hashForSymbol() const { return m_hash; }
    bool isNullSymbol() const { return m_flags & s_flagIsNullSymbol; }
    bool isRegistered() const { return m_flags & s_flagIsRegistered; }
    bool isPrivate() const { return m_flags & s_flagIsPrivate; }

    // `bun_engine_text::StaticSymbol`.
    class StaticSymbolImpl final {
        WTF_MAKE_NONCOPYABLE(StaticSymbolImpl);

    public:
        consteval StaticSymbolImpl(ASCIILiteral description, Flags flags = s_flagDefault)
            : m_string(staticDescription(description))
            , m_hash(StringHasher::computeLiteralHash(description.span()))
            , m_flags(flags)
        {
        }

        operator SymbolImpl&() const { return *reinterpret_cast<SymbolImpl*>(const_cast<StaticSymbolImpl*>(this)); }

    private:
        StaticStringImpl m_string;
        [[maybe_unused]] StringImpl* m_owner { nullptr };
        [[maybe_unused]] unsigned m_hash;
        [[maybe_unused]] Flags m_flags;
    };

private:
    static consteval StaticStringImpl staticDescription(ASCIILiteral description) { return { description, flagIsSymbol() }; }

    [[maybe_unused]] StringImpl* m_owner;
    unsigned m_hash;
    Flags m_flags;
};

static_assert(sizeof(SymbolImpl) == 40 && sizeof(SymbolImpl::StaticSymbolImpl) == sizeof(SymbolImpl));

class PrivateSymbolImpl final : public SymbolImpl {
public:
    static Ref<PrivateSymbolImpl> create(StringImpl& description) { return adoptRef(*engine_text_private_symbol(description)); }
};

class RegisteredSymbolImpl final : public SymbolImpl {
};

inline unsigned StringImpl::existingSymbolAwareHash() const
{
    return isSymbol() ? static_cast<const SymbolImpl*>(this)->hashForSymbol() : existingHash();
}

}

SPECIALIZE_TYPE_TRAITS_BEGIN(WTF::SymbolImpl)
static bool isType(const WTF::StringImpl& string) { return string.isSymbol(); }
SPECIALIZE_TYPE_TRAITS_END()

using WTF::PrivateSymbolImpl;
