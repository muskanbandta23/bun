#pragma once

#include "engine/support/TypeCasts.h"
#include "engine/text/StringImpl.h"

extern "C" {
WTF::AtomStringImpl* engine_text_atom_of_latin1(const WTF::Latin1Character*, size_t length) RETURNS_NONNULL;
WTF::AtomStringImpl* engine_text_atom_of_utf16(const char16_t*, size_t length) RETURNS_NONNULL;
WTF::AtomStringImpl* engine_text_atom_of_literal(const char*, size_t length) RETURNS_NONNULL;
WTF::AtomStringImpl* engine_text_atom_of_string(const WTF::StringImpl&) RETURNS_NONNULL;
WTF::AtomStringImpl* engine_text_find_atom_of_string(const WTF::StringImpl&);
size_t engine_text_length_of_longest_atom();
}

namespace WTF {

// An atom or a symbol: equal if and only if identical.
class UniquedStringImpl : public StringImpl {
};

// The one string of this thread with its characters.
class AtomStringImpl final : public UniquedStringImpl {
public:
    static RefPtr<AtomStringImpl> add(std::span<const Latin1Character> characters) { return characters.data() ? adoptRef(engine_text_atom_of_latin1(characters.data(), characters.size())) : nullptr; }
    static RefPtr<AtomStringImpl> add(std::span<const char16_t> characters) { return characters.data() ? adoptRef(engine_text_atom_of_utf16(characters.data(), characters.size())) : nullptr; }
    static Ref<AtomStringImpl> add(ASCIILiteral literal) { return adoptRef(*engine_text_atom_of_literal(literal.characters(), literal.length())); }

    static RefPtr<AtomStringImpl> add(StringImpl* string)
    {
        if (!string || string->isAtom())
            return static_cast<AtomStringImpl*>(string);
        return adoptRef(engine_text_atom_of_string(*string));
    }

    static RefPtr<AtomStringImpl> add(RefPtr<StringImpl>&& string)
    {
        if (!string || string->isAtom())
            return adoptRef(static_cast<AtomStringImpl*>(string.leakRef()));
        return adoptRef(engine_text_atom_of_string(*string));
    }

    static RefPtr<AtomStringImpl> lookUp(StringImpl* string)
    {
        if (!string || string->isAtom())
            return static_cast<AtomStringImpl*>(string);
        return adoptRef(engine_text_find_atom_of_string(*string));
    }

    static size_t lengthOfLongest() { return engine_text_length_of_longest_atom(); }
};

}

SPECIALIZE_TYPE_TRAITS_BEGIN(WTF::AtomStringImpl)
static bool isType(const WTF::StringImpl& string) { return string.isAtom(); }
SPECIALIZE_TYPE_TRAITS_END()
