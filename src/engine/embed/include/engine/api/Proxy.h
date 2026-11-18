#pragma once

#include "engine/api/GlobalObject.h"

namespace JSC {

class ProxyObject final : public JSInternalFieldObjectImpl<2> {
public:
    ENGINE_DECLARE_CLASS(ProxyObject)

    enum class Field : uint32_t {
        Target = Engine::Layout::proxyTargetField,
        Handler = Engine::Layout::proxyHandlerField,
    };

    static ProxyObject* create(JSGlobalObject* globalObject, JSValue target, JSValue handler) { return Engine::proxy_create(globalObject, target, handler); }
    // Null if the handler has none.
    static JSObject* getHandlerTrap(JSGlobalObject* globalObject, JSObject* handler, PropertyName trap) { return Engine::proxy_find_trap(globalObject, handler, trap); }
    static void validateGetTrapResult(JSGlobalObject* globalObject, JSValue trapResult, JSObject* target, PropertyName name) { Engine::proxy_validate_get_result(globalObject, trapResult, target, name); }

    const WriteBarrier<Unknown>& internalField(Field field) const { return JSInternalFieldObjectImpl::internalField(static_cast<uint32_t>(field)); }
    JSObject* target() const { return asObject(internalField(Field::Target).get()); }
    JSValue handler() const { return internalField(Field::Handler).get(); }
    bool isRevoked() const { return handler().isNull(); }
};
ENGINE_CLASS_HAS_TYPE(ProxyObject, ProxyObjectType);

} // namespace JSC
