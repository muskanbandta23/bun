#pragma once

#include "engine/api/GlobalObject.h"

using JSTypedArrayBytesDeallocator = void (*)(void* bytes, void* context);

#define MAX_ARRAY_BUFFER_SIZE (1ull << 32)

namespace JSC {

class JSArrayBufferView : public JSNonFinalObject {
};

struct Uint8Adaptor;
template<typename> class JSGenericTypedArrayView;
using JSUint8Array = JSGenericTypedArrayView<Uint8Adaptor>;

class TypedArrayController : public RefCounted<TypedArrayController> {
public:
    virtual ~TypedArrayController() = default;
};

} // namespace JSC
