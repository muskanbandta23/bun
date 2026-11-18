#pragma once

// The functions of embed/api/*.rs.

#include "engine/api/Identifier.h"

namespace Engine {

ALWAYS_INLINE WTF::StringImpl* implOrEmpty(const WTF::String& string) { return string.impl() ? string.impl() : WTF::StringImpl::empty(); }
ALWAYS_INLINE WTF::StringImpl* leak(WTF::String&& string) { return string.releaseImpl().leakRef(); }
ALWAYS_INLINE WTF::StringImpl* leakOrEmpty(WTF::String&& string) { return string.impl() ? leak(WTF::move(string)) : WTF::StringImpl::empty(); }

// Around a call that may leave an exception, which the caller has to look for.
class MayThrow {
public:
#if ENABLE(EXCEPTION_SCOPE_VERIFICATION)
    explicit MayThrow(const JSC::JSGlobalObject*, const char* function = __builtin_FUNCTION());
    ~MayThrow();

private:
    Opaque<56> m_scope;
#else
    ALWAYS_INLINE explicit MayThrow(const JSC::JSGlobalObject*)
    {
    }
#endif
};

} // namespace Engine

#include "EngineExports.h"
