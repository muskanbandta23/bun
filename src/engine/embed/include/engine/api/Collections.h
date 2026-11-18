#pragma once

#include "engine/api/GlobalObject.h"

namespace JSC {

class JSMap final : public JSNonFinalObject {
public:
    ENGINE_DECLARE_CLASS(Map)
};
ENGINE_CLASS_HAS_TYPE(JSMap, JSMapType);

class JSSet final : public JSNonFinalObject {
public:
    ENGINE_DECLARE_CLASS(Set)
};
ENGINE_CLASS_HAS_TYPE(JSSet, JSSetType);

class JSWeakMap final : public JSNonFinalObject {
public:
    ENGINE_DECLARE_CLASS(WeakMap)
};
ENGINE_CLASS_HAS_TYPE(JSWeakMap, JSWeakMapType);

} // namespace JSC
