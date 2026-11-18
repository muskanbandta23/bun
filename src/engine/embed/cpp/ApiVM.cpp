#include "engine/Engine.h"

namespace JSC {

VM::VM()
    : deferredWorkTimer(DeferredWorkTimer::create())
    , m_ownerThread(currentThreadID())
    , m_identifier(VMIdentifier::generate())
    , m_hardStackLimit(Engine::vm_hard_stack_limit(*this))
{
}

VM::~VM()
{
    delete clientData;
}

RefPtr<VM> VM::tryCreate(HeapType heapType)
{
    void* memory = Engine::vm_create(heapType, Engine::StaticAtom::COUNT, Engine::RealmSlot::COUNT);
    return adoptRef(new (memory) VM);
}

extern "C" void engine_host_destroy_vm_members(VM* vm)
{
    vm->~VM();
}

void VM::whenIdle(Function<void()>&& task)
{
    auto run = [](void* context) { (*std::unique_ptr<Function<void()>>(static_cast<Function<void()>*>(context)))(); };
    Engine::vm_when_idle(*this, run, new Function<void()>(WTF::move(task)));
}

SymbolRegistry& VM::symbolRegistry()
{
    static SymbolRegistry registryOfCurrentThread;
    return registryOfCurrentThread;
}

bool VM::shrinkFootprintNow(OptionSet<ShrinkFootprint> options)
{
    if (isEntered())
        return false;
    if (options.contains(ShrinkFootprint::KeepCodeInUse))
        heap.deleteAllUnlinkedCodeBlocks(DeleteAllCodeIfNotCollecting);
    else
        deleteAllCode(DeleteAllCodeIfNotCollecting);
    if (!options.contains(ShrinkFootprint::LeaveCollectionToCaller))
        heap.collectNow(Sync, CollectionScope::Full);
    return true;
}

void VM::shrinkFootprintWhenIdle(OptionSet<ShrinkFootprint> options)
{
    whenIdle([this, options] { shrinkFootprintNow(options); });
}

#if ENABLE(EXCEPTION_SCOPE_VERIFICATION)

ExceptionScope::ExceptionScope(VM& vm, ExceptionEventLocation location)
    : m_vm(vm)
    , m_previous(std::exchange(vm.m_topExceptionScope, this))
    , m_location(location)
{
}

ExceptionScope::~ExceptionScope()
{
    RELEASE_ASSERT(m_vm.m_topExceptionScope);
    m_vm.m_topExceptionScope = m_previous;
}

static bool isEnabledByEnvironment(const char* name)
{
    const char* value = getenv(name);
    return value && (*value == '1' || *value == 't');
}

void ExceptionScope::verifyThatNoCheckIsOwed() const
{
    static const bool validates = isEnabledByEnvironment("BUN_JSC_validateExceptionChecks");
    if (!validates || !m_vm.m_needsExceptionCheck) [[likely]]
        return;
    auto& thrown = m_vm.m_simulatedThrowLocation;
    fprintf(stderr, "ERROR: Unchecked JS exception:\n    This scope can throw a JS exception: %s @ %s:%u\n    But the exception was unchecked as of this scope: %s @ %s:%u\n", thrown.functionName, thrown.file, thrown.line, m_location.functionName, m_location.file, m_location.line);
    WTFReportBacktrace();
    RELEASE_ASSERT_NOT_REACHED();
}

void ExceptionScope::simulateThrow() const
{
    m_vm.m_needsExceptionCheck = true;
    m_vm.m_simulatedThrowLocation = m_location;
}

ThrowScope::ThrowScope(VM& vm, ExceptionEventLocation location)
    : ExceptionScope(vm, location)
{
    verifyThatNoCheckIsOwed();
}

ThrowScope::~ThrowScope()
{
    if (m_isReleased)
        didCheck();
    else
        verifyThatNoCheckIsOwed();
    bool returnsToGeneratedCode = m_vm.topEntryFrame && (!m_previous || m_previous->m_location.stackPosition > m_vm.topEntryFrame);
    if (!returnsToGeneratedCode && !(m_previous && m_previous->m_isAroundCallIntoEngine))
        simulateThrow();
}

TopExceptionScope::TopExceptionScope(VM& vm, ExceptionEventLocation location)
    : ExceptionScope(vm, location)
{
    verifyThatNoCheckIsOwed();
}

TopExceptionScope::~TopExceptionScope()
{
    verifyThatNoCheckIsOwed();
}

#endif

} // namespace JSC

#if ENABLE(EXCEPTION_SCOPE_VERIFICATION)

namespace Engine {

MayThrow::MayThrow(const JSC::JSGlobalObject* globalObject, const char* function)
{
    static_assert(sizeof(m_scope) == sizeof(JSC::ThrowScope));
    auto* scope = new (&m_scope) JSC::ThrowScope(globalObject->vm(), { currentStackPointer(), function, "EngineExports.h", 0 });
    scope->m_isAroundCallIntoEngine = true;
    scope->release();
}

MayThrow::~MayThrow()
{
    reinterpret_cast<JSC::ThrowScope*>(&m_scope)->~ThrowScope();
}

} // namespace Engine

#endif
