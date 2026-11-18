#include "engine/Engine.h"

namespace JSC {

RefPtr<CachedBytecode> SourceProvider::cachedBytecode() const
{
    return nullptr;
}

RefPtr<CachedBytecode> generateBytecode(VM& vm, const SourceCode& source, SourceCodeType type, unsigned depth)
{
    RefPtr<CachedBytecode> bytecode;
    Engine::bytecode_generate(vm, source, type == SourceCodeType::ModuleType, depth, &bytecode, [](void* bytecode, const uint8_t* bytes, size_t size) {
        *static_cast<RefPtr<CachedBytecode>*>(bytecode) = CachedBytecode::createByCopying({ bytes, size });
    });
    return bytecode;
}

JSValue evaluate(JSGlobalObject* globalObject, const SourceCode& source, UnlinkedProgramCodeBlock* code, JSValue thisValue, NakedPtr<Exception>& returnedException)
{
    if (!code)
        return evaluate(globalObject, source, thisValue, returnedException);
    auto scope = DECLARE_TOP_EXCEPTION_SCOPE(globalObject->vm());
    JSValue result = Engine::compiled_script_evaluate(*code, globalObject, thisValue);
    if (auto* exception = scope.exception()) [[unlikely]] {
        returnedException = exception;
        scope.clearException();
        return jsUndefined();
    }
    return result;
}

} // namespace JSC
