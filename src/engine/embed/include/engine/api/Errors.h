#pragma once

#include "engine/api/GlobalObject.h"

namespace JSC {

class Exception final : public JSCell {
public:
    ENGINE_DECLARE_CLASS(Exception)

    JSValue value() const { return Engine::fieldAt<JSValue>(this, Engine::Layout::Exception_value); }
};
ENGINE_CLASS_HAS_TYPE(Exception, ExceptionType);

} // namespace JSC
