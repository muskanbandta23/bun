#include "ApiFixtures.h"

namespace Test {

using namespace JSC;

class GlobalObject final : public JSGlobalObject {
public:
    using Base = JSGlobalObject;

    DECLARE_INFO;

    static GlobalObject* create(VM& vm, IsoSubspace& space)
    {
        auto* structure = Structure::create(vm, nullptr, jsNull(), TypeInfo(GlobalObjectType, StructureFlags), info());
        auto* global = new (NotNull, space.allocate(vm.heap, sizeof(GlobalObject))) GlobalObject(vm, structure);
        global->finishCreation(vm);
        return global;
    }

private:
    GlobalObject(VM& vm, Structure* structure)
        : Base(vm, structure, &methods)
    {
    }

    static void trackRejection(JSGlobalObject*, JSPromise*, JSPromiseRejectionOperation operation) { Realm::log().append(operation == JSPromiseRejectionOperation::Reject ? "reject"_s : "handle"_s); }
    static void reportException(JSGlobalObject* global, Exception* exception) { Realm::log().append(makeString("uncaught "_s, exception->value().toWTFString(global))); }

    static constexpr GlobalObjectMethodTable methods {
        .promiseRejectionTracker = trackRejection,
        .reportUncaughtExceptionAtEventLoop = reportException,
        .currentScriptExecutionOwner = currentScriptExecutionOwner,
        .scriptExecutionStatus = scriptExecutionStatus,
    };
};

const ClassInfo GlobalObject::s_info = { "GlobalObject"_s, &Base::s_info, nullptr, nullptr, CREATE_METHOD_TABLE(GlobalObject) };

struct ClientData final : VM::ClientData {
    explicit ClientData(VM& vm)
        : globalObjects(vm.heap.createIsoSubspace(sizeof(GlobalObject), GlobalObject::destroy, false))
    {
    }

    void reportErrorOfMicrotask(JSGlobalObject* global, Exception* exception) final { Realm::log().append(makeString("error of microtask "_s, exception->value().toWTFString(global))); }

    IsoSubspace& globalObjects;
};

static VM& createVM()
{
    static std::once_flag once;
    std::call_once(once, [] { initialize([] {}); });
    VM& vm = *VM::tryCreate().leakRef();
    vm.clientData = new ClientData(vm);
    return vm;
}

Realm::Realm()
    : vm(createVM())
    , global(createGlobalObject())
{
    gcProtect(global);
    log().clear();
}

Realm::~Realm()
{
    EXPECT(!vm.exceptionForInspection());
    gcUnprotect(global);
    vm.deref();
}

JSGlobalObject* Realm::createGlobalObject()
{
    return GlobalObject::create(vm, static_cast<ClientData*>(vm.clientData)->globalObjects);
}

JSValue Realm::run(ASCIILiteral source)
{
    NakedPtr<Exception> exception;
    JSValue result = evaluate(global, makeSource(source, {}, SourceTaintedOrigin::Untainted), {}, exception);
    if (exception)
        fail(__FILE__, __LINE__, makeString(source, " not to throw "_s, text(exception->value())).utf8().data());
    return result;
}

String Realm::text(JSValue value)
{
    auto scope = DECLARE_TOP_EXCEPTION_SCOPE(vm);
    String result = value.toWTFString(global);
    scope.releaseAssertNoException();
    return result;
}

String Realm::takeException()
{
    auto scope = DECLARE_TOP_EXCEPTION_SCOPE(vm);
    Exception* exception = scope.exception();
    if (!exception)
        return "nothing"_s;
    scope.clearException();
    return text(exception->value());
}

Vector<String>& Realm::log()
{
    static NeverDestroyed<Vector<String>> log;
    return log;
}

}
