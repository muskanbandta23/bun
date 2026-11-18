#pragma once

#include "engine/support/Assertions.h"
#include "engine/support/Forward.h"

namespace WTF {

// A number other than zero that cannot be taken for the identifier of another kind of object.
template<typename Kind>
class ObjectIdentifier {
public:
    explicit constexpr ObjectIdentifier(uint64_t identifier)
        : m_identifier(identifier)
    {
        RELEASE_ASSERT(identifier);
    }

    constexpr uint64_t toUInt64() const { return m_identifier; }

    friend constexpr bool operator==(ObjectIdentifier, ObjectIdentifier) = default;

private:
    uint64_t m_identifier;
};

}

using WTF::ObjectIdentifier;
