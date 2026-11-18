#pragma once

// All that the scripts of one thread share.

#include "engine/api/Heap.h"

namespace JSC {

class CodeCache;
class ControlFlowProfiler;
class DecoderStringTable;
class DeferredWorkTimer;
class EntryFrame;
class FunctionHasExecutedCache;
class HeapProfiler;
class JSONCache;
class MicrotaskQueue;
class PersistentBytecodePayloads;
class QueuedTask;
class SamplingProfiler;
class SourceProviderCache;
class TerminationDeadline;
class TypedArrayController;
class UnlinkedProgramCodeBlock;

enum VMIdentifierType {};
using VMIdentifier = ObjectIdentifier<VMIdentifierType>;
using VMMalloc = FastMalloc;
using ErrorInfoFunction = Function<String(VM&, Vector<StackFrame>&, unsigned& line, unsigned& column, String& sourceURL)>;
using ErrorInfoFunctionJSValue = Function<JSValue(VM&, Vector<StackFrame>&, unsigned& line, unsigned& column, String& sourceURL, JSObject*)>;

class VMTraps {
public:
    using BitField = uint32_t;
    static constexpr BitField NeedTermination = Engine::Trap::TERMINATION;

    bool needHandling(BitField mask) const { return m_pending.load(std::memory_order_relaxed) & mask; }
    // Also true while an exception is pending.
    ALWAYS_INLINE bool maybeNeedHandling() const { return Engine::fieldAt<uintptr_t>(this, Engine::Layout::Vm_exception - Engine::Layout::Vm_traps) | m_pending.load(std::memory_order_relaxed); }
    bool isDeferringTermination() const { return Engine::vm_is_termination_deferred(Engine::fieldAt<VM>(this, -Engine::Layout::Vm_traps)); }

private:
    friend class DeferTraps;
    friend class VM;

    std::atomic<BitField> m_pending;
    std::atomic<bool> m_isTerminationRequested;
    bool m_areDeferred;
    Engine::Opaque<8> m_stackLimit;
    void* m_realStackLimit;
};

class SmallStrings {
public:
    JSString* emptyString() const { return m_empty; }
    JSString* singleCharacterString(unsigned char character) const { return ofStaticAtom(character); }
    JSString* defaultString() const { return ofStaticAtom(Engine::StaticAtom::default_); }
    JSString* falseString() const { return ofStaticAtom(Engine::StaticAtom::false_); }
    JSString* nullString() const { return ofStaticAtom(Engine::StaticAtom::null_); }
    JSString* objectObjectString() const { return ofStaticAtom(Engine::StaticAtom::objectObject_); }
    JSString* symbolString() const { return ofStaticAtom(Engine::StaticAtom::symbol_); }
    JSString* trueString() const { return ofStaticAtom(Engine::StaticAtom::true_); }
    JSString* undefinedString() const { return ofStaticAtom(Engine::StaticAtom::undefined_); }

private:
    friend class VM;

    JSString* ofStaticAtom(unsigned index) const;

    JSString* m_empty;
    JSString* m_ofStaticAtoms[Engine::StaticAtom::COUNT];
};

inline constexpr unsigned maxSingleCharacterString = 0xff;

// A VM belongs to the thread that made it, so there is nothing to wait for.
class JSLock {
public:
    void ref() const {}
    void deref() const {}
    void lock() {}
    void unlock() {}
};

class JSLockHolder {
public:
    JSLockHolder(VM&) {}
    JSLockHolder(VM*) {}
    JSLockHolder(JSGlobalObject*) {}
    ~JSLockHolder() {}
};

using ConcurrentJSLock = Lock;

class ConcurrentJSLocker {
public:
    ConcurrentJSLocker(NoLockingNecessaryTag) {}
};

class DateCache {
public:
    void clearForTimeZoneChange();
    double gregorianDateTimeToMS(int32_t year, int32_t month, int32_t day, int32_t hour, int32_t minute, int32_t second, double milliseconds, TimeType);
    PlainGregorianDateTime msToGregorianDateTime(double milliseconds, TimeType);
    double parseDate(JSGlobalObject*, VM&, const String&);

private:
    VM& vm();
};

class Interpreter {
public:
    using StackFrameFilter = ScopedLambda<bool(StackFrame&)>;

    void getStackTrace(JSCell* owner, Vector<StackFrame>& results, size_t framesToSkip = 0, size_t maxStackSize = std::numeric_limits<size_t>::max(), JSCell* caller = nullptr, JSCell* ownerOfCallLinkInfo = nullptr, CallLinkInfo* = nullptr, const StackFrameFilter* = nullptr);
    JSValue executeProgram(const SourceCode&, JSGlobalObject*, JSObject* thisObject, UnlinkedProgramCodeBlock* = nullptr);

private:
    VM& vm();
};

struct ExceptionEventLocation {
    void* stackPosition { nullptr };
    const char* functionName { nullptr };
    const char* file { nullptr };
    unsigned line { 0 };
};

// Rust allocates it. The constructor and the destructor see to the members that only C++ has.
class VM {
    WTF_MAKE_NONCOPYABLE(VM);

public:
    struct ClientData {
        virtual ~ClientData() = default;
        virtual bool isWebCoreJSClientData() const { return false; }
        virtual DecoderStringTable* decoderStringTable() { return nullptr; }
        virtual void reconcileWeakReferencesAtGCEnd(VM&, CollectionScope) {}
    };

    enum class DeletePropertyMode : bool { Default,
        IgnoreConfigurable };

    class DeletePropertyModeScope {
    public:
        DeletePropertyModeScope(VM& vm, DeletePropertyMode mode)
            : m_vm(vm)
            , m_previous(std::exchange(vm.m_deletePropertyMode, mode))
        {
        }
        ~DeletePropertyModeScope() { m_vm.m_deletePropertyMode = m_previous; }

    private:
        VM& m_vm;
        DeletePropertyMode m_previous;
    };

    enum class ShrinkFootprint : uint8_t {
        LeaveCollectionToCaller = 1 << 0,
        KeepCodeInUse = 1 << 1,
    };

    struct SynchronousModuleQueue;
    class SynchronousModuleQueueScope;

    static RefPtr<VM> tryCreate(HeapType = HeapType::Small, RunLoop* = nullptr);
    void ref() const { m_refCount.fetch_add(1, std::memory_order_relaxed); }
    void deref() const { Engine::vm_release(this); }
    void refSuppressingSaferCPPChecking() const { ref(); }
    void derefSuppressingSaferCPPChecking() const { deref(); }

    VMIdentifier identifier() const { return m_identifier; }
    RunLoop& runLoop() const { return m_runLoop.get(); }
    JSLock& apiLock() { return m_apiLock; }
    bool currentThreadIsHoldingAPILock() const { return m_ownerThread == currentThreadID(); }
    bool isCollectorBusyOnCurrentThread() const { return !currentThreadIsHoldingAPILock() || heap.mutatorState() != MutatorState::Running; }
    constexpr bool hasCompiledCode() const { return false; }

    bool isEntered() const { return entryScope; }
    void whenIdle(Function<void()>&&);
    void finalizeSynchronousJSExecution() { Engine::vm_clear_kept_objects(*this); }
    void* softStackLimit() const { return m_traps.m_realStackLimit; }
    bool isSafeToRecurseSoft() const { return currentStackPointer() >= softStackLimit(); }
    bool isSafeToRecurse() const { return currentStackPointer() >= m_hardStackLimit; }

    Exception* exceptionForInspection() const { return m_exception; }
    Exception* lastException() const { return m_lastException; }
    void clearLastException() { m_lastException = nullptr; }
    bool hasExceptionsAfterHandlingTraps() { return Engine::vm_has_exception_after_handling_traps(*this); }

    VMTraps& traps() { return m_traps; }
    const VMTraps& traps() const { return m_traps; }
    void notifyNeedTermination() { Engine::vm_fire_trap(*this, Engine::Trap::TERMINATION); }
    void notifyNeedDebuggerBreak() { Engine::vm_fire_trap(*this, Engine::Trap::DEBUGGER_BREAK); }
    void notifyNeedShellTimeoutCheck() { Engine::vm_fire_trap(*this, Engine::Trap::SHELL_TIMEOUT_CHECK); }
    Exception* ensureTerminationException() { return Engine::vm_termination_exception(*this); }
    bool isTerminationException(Exception* exception) const { return exception == m_terminationException; }
    bool hasPendingTerminationException() const { return m_exception && isTerminationException(m_exception); }
    bool hasTerminationRequest() const { return m_traps.m_isTerminationRequested.load(std::memory_order_relaxed); }
    void setHasTerminationRequest() { m_traps.m_isTerminationRequested.store(true); }
    void clearHasTerminationRequest() { m_traps.m_isTerminationRequested.store(false); }
    bool hasPendingTermination() const { return m_traps.needHandling(VMTraps::NeedTermination) || hasTerminationRequest() || hasPendingTerminationException(); }
    bool cancelTermination() { return Engine::vm_cancel_termination(*this); }
    void throwTerminationException() { Engine::vm_throw_termination(*this); }
    Ref<TerminationDeadline> addTerminationDeadline(MonotonicTime);
    bool executionForbidden() const { return m_isExecutionForbidden; }
    void setExecutionForbidden() { m_isExecutionForbidden = true; }

    ALWAYS_INLINE void writeBarrier(const JSCell* from)
    {
        if (from->cellState() <= heap.barrierThreshold()) [[unlikely]]
            Engine::write_barrier_slow_path(from);
    }
    ALWAYS_INLINE void writeBarrier(const JSCell* from, JSCell* to)
    {
        if (to)
            writeBarrier(from);
    }
    ALWAYS_INLINE void writeBarrier(const JSCell* from, JSValue to)
    {
        if (to.isCell() && to)
            writeBarrier(from);
    }
    ALWAYS_INLINE void mutatorFence()
    {
#if CPU(X86_64)
        std::atomic_signal_fence(std::memory_order_release);
#else
        if (heap.mutatorShouldBeFenced()) [[unlikely]]
            std::atomic_thread_fence(std::memory_order_release);
#endif
    }

    IsoSubspace& plainObjectSpace() { return heap.cellSpace().allocatorFor<Engine::Layout::sizeOfJsObject>(); }
    IsoSubspace& internalFunctionSpace() { return heap.cellSpace().allocatorFor<Engine::Layout::sizeOfInternalFunction>(); }
    IsoSubspace& ropeStringSpace() { return heap.ropeStringSpace(); }
    CompleteSubspace& destructibleObjectSpace() { return heap.destructibleObjectSpace(); }

    NativeExecutable* getHostFunction(NativeFunction function, ImplementationVisibility visibility, NativeFunction constructor, unsigned length, const String& name) { return getHostFunction(function, visibility, NoIntrinsic, constructor, nullptr, length, name); }
    NativeExecutable* getHostFunction(NativeFunction function, ImplementationVisibility visibility, Intrinsic intrinsic, NativeFunction constructor, const DOMJIT::Signature*, unsigned length, const String& name) { return Engine::native_executable_shared(*this, length, name, function, constructor, intrinsic, visibility); }
    ExecutableBase* internalFunctionExecutable() const { return m_internalFunctionExecutable; }
    // Makes `@name` in the source of a built-in stand for the symbol.
    void registerPrivateName(const Identifier& name, const Identifier& symbol) { Engine::vm_register_private_name(*this, name, symbol); }
    SymbolRegistry& symbolRegistry();

    MicrotaskQueue& defaultMicrotaskQueue();
    void queueMicrotask(QueuedTask&&);
    void drainMicrotasks();
    void setOnEachMicrotaskTick(Function<void(VM&)>&&);
    bool isAsyncContextTrackingEnabled() const { return m_isAsyncContextTracked; }
    SynchronousModuleQueue* synchronousModuleQueue() const { return m_synchronousModuleQueue; }

    Function<void(VM&, SourceProvider*, LineColumn&, String&)>& computeLineColumnWithSourcemap() { return m_computeLineColumnWithSourcemap; }
    void setComputeLineColumnWithSourcemap(Function<void(VM&, SourceProvider*, LineColumn&, String&)>&& function) { m_computeLineColumnWithSourcemap = WTF::move(function); }
    const ErrorInfoFunction& onComputeErrorInfo() const { return m_onComputeErrorInfo; }
    void setOnComputeErrorInfo(ErrorInfoFunction&&);
    const ErrorInfoFunctionJSValue& onComputeErrorInfoJSValue() const { return m_onComputeErrorInfoJSValue; }
    void setOnComputeErrorInfoJSValue(ErrorInfoFunctionJSValue&&);

    void deleteAllCode(DeleteAllCodeEffort) { Engine::vm_delete_all_code(*this); }
    bool shrinkFootprintNow(OptionSet<ShrinkFootprint> = {});
    void shrinkFootprintWhenIdle(OptionSet<ShrinkFootprint> = {});
    void setStartupJITDeferralScale(double);
    CodeCache* codeCache();
    SourceProviderCache* addSourceProviderCache(SourceProvider*);
    PersistentBytecodePayloads& persistentBytecodePayloads();
    PersistentBytecodePayloads* persistentBytecodePayloadsIfExists();
    JSONCache& jsonCache();

    bool enableControlFlowProfiler();
    ControlFlowProfiler* controlFlowProfiler();
    FunctionHasExecutedCache* functionHasExecutedCache();
    HeapProfiler& ensureHeapProfiler();
    HeapProfiler* heapProfiler();
    SamplingProfiler& ensureSamplingProfiler(Ref<Stopwatch>&&);
    SamplingProfiler* samplingProfiler();

    static constexpr Engine::CommonNamesPointer propertyNames {};

    ~VM();

private:
    friend class ExceptionScope;
    friend class JSNativeStdFunction;
    friend class SuspendExceptionScope;
    friend class ThrowScope;
    friend class TopExceptionScope;
    friend class Engine::MayThrow;

    explicit VM(RunLoop&);
    static void checkLayout();

    Exception* m_exception;
    VMTraps m_traps;

public:
    CallFrame* topCallFrame;
    EntryFrame* topEntryFrame;
    ClientData* clientData;
    // The realm through which the outermost script was entered.
    JSGlobalObject* entryScope;

private:
    Exception* m_terminationException;
    Exception* m_lastException;
    ExecutableBase* m_internalFunctionExecutable;
    mutable std::atomic<uint32_t> m_refCount;

public:
    bool didEnterVM;

private:
    bool m_isExecutionForbidden;
    DeletePropertyMode m_deletePropertyMode;
    bool m_isAsyncContextTracked;
    Engine::Opaque<160> m_forGeneratedCode;

public:
    // From here to the heap, what only C++ has.
    const Ref<DeferredWorkTimer> deferredWorkTimer;
    RefPtr<TypedArrayController> m_typedArrayController;
    SynchronousModuleQueue* m_synchronousModuleQueue { nullptr };
    DateCache dateCache;
    Interpreter interpreter;

private:
    JSLock m_apiLock;
    bool m_needsExceptionCheck { false };
    const uint32_t m_ownerThread;
    const VMIdentifier m_identifier;
    const Ref<RunLoop> m_runLoop;
    void* const m_hardStackLimit;
    Function<void(VM&, SourceProvider*, LineColumn&, String&)> m_computeLineColumnWithSourcemap;
    ErrorInfoFunction m_onComputeErrorInfo;
    ErrorInfoFunctionJSValue m_onComputeErrorInfoJSValue;
    Function<void(VM&)> m_onEachMicrotaskTick;
    ExceptionScope* m_topExceptionScope { nullptr };
    ExceptionEventLocation m_simulatedThrowLocation;
    IsoSubspace* m_nativeStdFunctionSpace { nullptr };
    Engine::Opaque<120> m_unused;

public:
    Heap heap;
    SmallStrings smallStrings;
};

inline void VM::checkLayout()
{
    ENGINE_CHECK_SIZE(VMTraps, sizeOfTraps);
    ENGINE_CHECK_OFFSET(VMTraps, m_pending, Traps_pending);
    ENGINE_CHECK_OFFSET(VMTraps, m_isTerminationRequested, Traps_is_termination_requested);
    ENGINE_CHECK_OFFSET(VMTraps, m_areDeferred, Traps_are_deferred);
    ENGINE_CHECK_OFFSET(VMTraps, m_realStackLimit, Traps_real_stack_limit);
    ENGINE_CHECK_OFFSET(SmallStrings, m_empty, SmallStrings_empty);
    ENGINE_CHECK_OFFSET(SmallStrings, m_ofStaticAtoms, SmallStrings_static_atoms);
    ENGINE_CHECK_OFFSET(VM, m_exception, Vm_exception);
    ENGINE_CHECK_OFFSET(VM, m_traps, Vm_traps);
    ENGINE_CHECK_OFFSET(VM, topCallFrame, Vm_top_call_frame);
    ENGINE_CHECK_OFFSET(VM, topEntryFrame, Vm_top_entry_frame);
    ENGINE_CHECK_OFFSET(VM, clientData, Vm_client_data);
    ENGINE_CHECK_OFFSET(VM, entryScope, Vm_entry_global);
    ENGINE_CHECK_OFFSET(VM, m_terminationException, Vm_termination_exception);
    ENGINE_CHECK_OFFSET(VM, m_lastException, Vm_last_exception);
    ENGINE_CHECK_OFFSET(VM, m_internalFunctionExecutable, Vm_internal_function_executable);
    ENGINE_CHECK_OFFSET(VM, m_refCount, Vm_ref_count);
    ENGINE_CHECK_OFFSET(VM, didEnterVM, Vm_did_enter);
    ENGINE_CHECK_OFFSET(VM, m_isExecutionForbidden, Vm_is_execution_forbidden);
    ENGINE_CHECK_OFFSET(VM, m_deletePropertyMode, Vm_deletes_unconfigurable_properties);
    ENGINE_CHECK_OFFSET(VM, m_isAsyncContextTracked, Vm_is_async_context_tracked);
    ENGINE_CHECK_OFFSET(VM, deferredWorkTimer, Vm_embedder_members);
    ENGINE_CHECK_OFFSET(VM, heap, Vm_heap);
    ENGINE_CHECK_OFFSET(VM, smallStrings, Vm_small_strings);
}

inline VM& DateCache::vm() { return Engine::fieldAt<VM>(this, -OBJECT_OFFSETOF(VM, dateCache)); }
inline VM& Interpreter::vm() { return Engine::fieldAt<VM>(this, -OBJECT_OFFSETOF(VM, interpreter)); }

ALWAYS_INLINE JSString* SmallStrings::ofStaticAtom(unsigned index) const
{
    if (JSString* string = m_ofStaticAtoms[index]) [[likely]]
        return string;
    return Engine::string_of_static_atom(Engine::fieldAt<VM>(this, -Engine::Layout::Vm_small_strings), index);
}

ALWAYS_INLINE void JSCell::finishCreation(VM& vm) { vm.mutatorFence(); }

template<typename T> ALWAYS_INLINE void* allocateCell(VM& vm, size_t size = sizeof(T))
{
    return T::template subspaceFor<T, SubspaceAccess::OnMainThread>(vm)->allocate(vm.heap, size);
}

// A collection that has become due runs at the end of the scope.
class DeferGC {
    WTF_MAKE_NONCOPYABLE(DeferGC);

public:
    DeferGC(VM& vm)
        : m_heap(vm.heap)
    {
        m_heap.m_deferralDepth++;
    }
    ~DeferGC()
    {
        if (!--m_heap.m_deferralDepth && m_heap.m_didDeferCollection) [[unlikely]]
            Engine::heap_collect_if_deferred(m_heap);
    }

private:
    Heap& m_heap;
};

class DeferGCForAWhile {
    WTF_MAKE_NONCOPYABLE(DeferGCForAWhile);

public:
    DeferGCForAWhile(VM& vm)
        : m_heap(vm.heap)
    {
        m_heap.m_deferralDepth++;
    }
    ~DeferGCForAWhile() { m_heap.m_deferralDepth--; }

private:
    Heap& m_heap;
};

class GCDeferralContext {
public:
    GCDeferralContext(VM& vm)
        : m_deferral(vm)
    {
    }

private:
    DeferGC m_deferral;
};

} // namespace JSC
