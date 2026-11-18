#pragma once

// What names a property: an atom or a symbol.

#include "engine/api/Value.h"

extern "C" const Engine::Opaque<Engine::Layout::sizeOfStaticString> engine_static_atoms[];
extern "C" const Engine::Opaque<Engine::Layout::sizeOfStaticString> engine_empty_string;
extern "C" const Engine::Opaque<Engine::Layout::sizeOfStaticSymbol> engine_symbols[];
extern "C" const Engine::Opaque<Engine::Layout::sizeOfStaticSymbol> engine_private_names[];

namespace JSC {

template<typename Character> std::optional<uint32_t> parseIndex(std::span<const Character> characters)
{
    if (characters.empty() || characters.size() > 10 || (characters[0] == '0' && characters.size() > 1))
        return std::nullopt;
    uint64_t value = 0;
    for (auto character : characters) {
        if (!isASCIIDigit(character))
            return std::nullopt;
        value = value * 10 + (character - '0');
    }
    if (value > MAX_ARRAY_INDEX)
        return std::nullopt;
    return static_cast<uint32_t>(value);
}

inline std::optional<uint32_t> parseIndex(const StringImpl& string)
{
    return string.is8Bit() ? parseIndex(string.span8()) : parseIndex(string.span16());
}

class PrivateName {
public:
    enum PrivateSymbolTag { PrivateSymbol };

    PrivateName(PrivateSymbolTag, const String& description)
        : m_uid(PrivateSymbolImpl::create(*description.impl()))
    {
    }

    SymbolImpl& uid() const { return m_uid.get(); }

private:
    Ref<SymbolImpl> m_uid;
};

class Identifier {
public:
    Identifier() = default;
    Identifier(WTF::HashTableDeletedValueType)
        : m_string(WTF::HashTableDeletedValue)
    {
    }

    static Identifier adopt(UniquedStringImpl* uid) { return Identifier(AtomString(adoptRef(static_cast<AtomStringImpl*>(uid)))); }

    static Identifier fromString(VM&, ASCIILiteral);
    static Identifier fromString(VM&, std::span<const Latin1Character> characters) { return Identifier(AtomString(characters)); }
    static Identifier fromString(VM&, std::span<const char16_t> characters) { return Identifier(AtomString(characters)); }
    static Identifier fromString(VM&, const String& string) { return Identifier(AtomString(string)); }
    static Identifier fromString(VM&, const AtomString& atom) { return Identifier(AtomString(atom)); }
    static Identifier fromString(VM&, AtomStringImpl* atom) { return Identifier(AtomString(atom)); }
    static Identifier fromString(VM&, Ref<AtomStringImpl>&& atom) { return Identifier(AtomString(WTF::move(atom))); }
    static Identifier fromUid(VM&, UniquedStringImpl* uid) { return Identifier(AtomString(static_cast<AtomStringImpl*>(uid))); }
    static Identifier fromUid(SymbolImpl& symbol) { return Identifier(AtomString(static_cast<AtomStringImpl*>(static_cast<UniquedStringImpl*>(&symbol)))); }
    static Identifier fromUid(const PrivateName& name) { return fromUid(name.uid()); }
    static Identifier from(VM&, unsigned);
    static Identifier from(VM&, double);

    // Of a symbol, the description.
    const AtomString& string() const { return m_string; }
    UniquedStringImpl* impl() const { return m_string.impl(); }
    RefPtr<AtomStringImpl> releaseImpl() { return m_string.releaseImpl(); }
    int length() const { return m_string.length(); }
    auto utf8() const { return m_string.string().utf8(); }

    bool isNull() const { return m_string.isNull(); }
    bool isEmpty() const { return m_string.isEmpty(); }
    bool isSymbol() const { return !isNull() && impl()->isSymbol(); }
    bool isPrivateName() const { return isSymbol() && static_cast<const SymbolImpl*>(impl())->isPrivate(); }
    bool isHashTableDeletedValue() const { return m_string.isHashTableDeletedValue(); }

    friend bool operator==(const Identifier& a, const Identifier& b) { return a.impl() == b.impl(); }
    friend bool operator==(const Identifier& a, ASCIILiteral b) { return !a.isSymbol() && WTF::equal(a.impl(), b); }

private:
    explicit Identifier(AtomString&& string)
        : m_string(WTF::move(string))
    {
    }

    AtomString m_string;
};

class PropertyName {
public:
    PropertyName(UniquedStringImpl* uid)
        : m_uid(uid)
    {
    }
    PropertyName(const Identifier& identifier)
        : m_uid(identifier.impl())
    {
    }
    PropertyName(const PrivateName& name)
        : m_uid(&name.uid())
    {
    }

    UniquedStringImpl* uid() const { return m_uid; }
    AtomStringImpl* publicName() const { return isSymbol() ? nullptr : static_cast<AtomStringImpl*>(m_uid); }
    bool isNull() const { return !m_uid; }
    bool isSymbol() const { return m_uid && m_uid->isSymbol(); }
    bool isPrivateName() const { return isSymbol() && static_cast<const SymbolImpl*>(m_uid)->isPrivate(); }

private:
    UniquedStringImpl* m_uid;
};

inline bool operator==(PropertyName a, PropertyName b) { return a.uid() == b.uid(); }
inline bool operator==(PropertyName a, const Identifier& b) { return a.uid() == b.impl(); }
inline bool operator==(PropertyName a, const char* b) { return !a.isSymbol() && WTF::equal(a.uid(), WTF::unsafeSpan8(b)); }

ALWAYS_INLINE std::optional<uint32_t> parseIndex(PropertyName name)
{
#if ENGINE_CHECK_WITH_REFERENCE_SUPPORT
    return parseIndex(*name.uid());
#else
    if (!name.uid() || !name.uid()->isArrayIndexAtom()) [[likely]]
        return std::nullopt;
    return parseIndex(*name.uid());
#endif
}

ALWAYS_INLINE std::optional<uint32_t> parseIndex(const Identifier& identifier) { return parseIndex(PropertyName(identifier)); }

bool isCanonicalNumericIndexString(UniquedStringImpl*, std::optional<uint64_t>* index = nullptr);
JSValue identifierToJSValue(VM&, const Identifier&);
// A private name is given as the string of its description.
JSValue identifierToSafePublicJSValue(VM&, const Identifier&);

struct IdentifierRepHash : PtrHash<RefPtr<UniquedStringImpl>> {
    static unsigned hash(const RefPtr<UniquedStringImpl>& key) { return key->existingSymbolAwareHash(); }
    static unsigned hash(UniquedStringImpl* key) { return key->existingSymbolAwareHash(); }
};

using IdentifierSet = UncheckedKeyHashSet<RefPtr<UniquedStringImpl>, IdentifierRepHash>;

#define ENGINE_FOR_EACH_COMMON_NAME(name, keyword, symbol, privateName)                                                          \
    name(Loader) name(__esModule) name(arguments) name(byteLength) name(byteOffset) name(caller) name(cause) name(column)        \
    name(constructor) name(detail) name(done) name(entries) name(error) name(errors) name(from) name(get) name(id) name(input)   \
    name(length) name(line) name(message) name(name) name(next) name(origin) name(parse) name(prototype) name(resolve) name(set) \
    name(size) name(sourceURL) name(stack) name(stackTraceLimit) name(target) name(then) name(toJSON) name(toString) name(type)  \
    name(value) name(values)                                                                                                     \
    keyword(default) keyword(delete) keyword(private) keyword(public) keyword(return) keyword(throw) keyword(undefined)          \
    symbol(asyncDispose) symbol(asyncIterator) symbol(dispose) symbol(iterator) symbol(species) symbol(toPrimitive)              \
    symbol(toStringTag)                                                                                                          \
    privateName(ArrayBuffer) privateName(meta) privateName(moduleLoader) privateName(starDefault) privateName(starNamespace)     \
    privateName(then)

class BuiltinNames;

// Never constructed: a view of Engine::commonNames.
class CommonIdentifiers {
public:
    CommonIdentifiers() = delete;

    const BuiltinNames& builtinNames() const { return *reinterpret_cast<const BuiltinNames*>(this); }

    const Identifier emptyIdentifier;
    const Identifier underscoreProto;
#define ENGINE_NAME(text) const Identifier text;
#define ENGINE_KEYWORD(text) const Identifier text##Keyword;
#define ENGINE_SYMBOL_NAME(text) const Identifier text##Symbol;
#define ENGINE_PRIVATE_NAME(text) const Identifier text##PrivateName;
    ENGINE_FOR_EACH_COMMON_NAME(ENGINE_NAME, ENGINE_KEYWORD, ENGINE_SYMBOL_NAME, ENGINE_PRIVATE_NAME)
#undef ENGINE_NAME
#undef ENGINE_KEYWORD
#undef ENGINE_SYMBOL_NAME
#undef ENGINE_PRIVATE_NAME
};

class BuiltinNames {
public:
    const Identifier& entriesPublicName() const { return m_names.entries; }
    const Identifier& valuesPublicName() const { return m_names.values; }
    const Identifier& ArrayBufferPrivateName() const { return m_names.ArrayBufferPrivateName; }
    const Identifier& metaPrivateName() const { return m_names.metaPrivateName; }
    const Identifier& moduleLoaderPrivateName() const { return m_names.moduleLoaderPrivateName; }
    const Identifier& thenPrivateName() const { return m_names.thenPrivateName; }

private:
    CommonIdentifiers m_names;
};

} // namespace JSC

namespace Engine {

inline constexpr const void* commonNames[] = {
    &engine_empty_string,
    &engine_static_atoms[StaticAtom::__proto___],
#define ENGINE_ATOM(text) &engine_static_atoms[StaticAtom::text##_],
#define ENGINE_SYMBOL_NAME(text) &engine_symbols[WellKnownSymbol::text##_],
#define ENGINE_PRIVATE_NAME(text) &engine_private_names[PrivateName::text##_],
    ENGINE_FOR_EACH_COMMON_NAME(ENGINE_ATOM, ENGINE_ATOM, ENGINE_SYMBOL_NAME, ENGINE_PRIVATE_NAME)
#undef ENGINE_ATOM
#undef ENGINE_SYMBOL_NAME
#undef ENGINE_PRIVATE_NAME
};
static_assert(sizeof(commonNames) == sizeof(JSC::CommonIdentifiers));

// The same for every VM, and known when linking.
struct CommonNamesPointer {
    ALWAYS_INLINE const JSC::CommonIdentifiers* operator->() const { return reinterpret_cast<const JSC::CommonIdentifiers*>(commonNames); }
    ALWAYS_INLINE const JSC::CommonIdentifiers& operator*() const { return *operator->(); }
    ALWAYS_INLINE operator const JSC::CommonIdentifiers*() const { return operator->(); }
};

} // namespace Engine

namespace WTF {

template<> struct VectorTraits<JSC::Identifier> : SimpleClassVectorTraits {};

} // namespace WTF
