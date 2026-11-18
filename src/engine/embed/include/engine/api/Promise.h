#pragma once

#include "engine/api/GlobalObject.h"

namespace JSC {

enum class QueuedTaskResult : uint8_t { Executed,
    Discard };

class JSPromise final : public JSNonFinalObject {
public:
    ENGINE_DECLARE_CLASS(Promise)
};
ENGINE_CLASS_HAS_TYPE(JSPromise, JSPromiseType);

class DeferredWorkTimer final : public ThreadSafeRefCounted<DeferredWorkTimer> {
public:
    enum class WorkType : uint8_t { AtSomePoint,
        ImminentlyScheduled };
    class Ticket : public ThreadSafeRefCounted<Ticket> {};
    using Task = Function<void(Ticket&)>;

    static Ref<DeferredWorkTimer> create();
};

} // namespace JSC
