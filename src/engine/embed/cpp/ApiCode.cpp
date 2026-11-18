#include "engine/Engine.h"

namespace JSC {

JSObject* ParserError::toErrorObject(JSGlobalObject* globalObject, const SourceCode& source, int overrideLineNumber) const
{
    if (!isValid())
        return nullptr;
    // A script may run, and destroy what owns the source.
    Ref provider = *source.provider();
    return Engine::parser_error_to_object(*this, globalObject, provider.get(), overrideLineNumber == -1 ? m_line : overrideLineNumber);
}

JSValue evaluate(JSGlobalObject* globalObject, const SourceCode& source, JSValue thisValue, NakedPtr<Exception>& returnedException)
{
    auto scope = DECLARE_TOP_EXCEPTION_SCOPE(globalObject->vm());
    JSValue result = Engine::evaluate_script(globalObject, source, thisValue);
    if (auto* exception = scope.exception()) [[unlikely]] {
        returnedException = exception;
        scope.clearException();
        return jsUndefined();
    }
    return result;
}

FunctionExecutable* getExecutableForFunction(JSValue value)
{
    auto* function = dynamicDowncast<JSFunction>(value);
    return function && !function->isHostFunction() ? function->jsExecutable() : nullptr;
}

CodeBlock* getSomeBaselineCodeBlockForFunction(JSValue value)
{
    auto* executable = getExecutableForFunction(value);
    auto* codeBlock = executable ? executable->eitherCodeBlock() : nullptr;
    return codeBlock ? codeBlock->baselineAlternative() : nullptr;
}

JSValue numberOfDFGCompiles(JSGlobalObject*, CallFrame* callFrame)
{
    if (!callFrame->argumentCount())
        return jsUndefined();
    auto* codeBlock = getSomeBaselineCodeBlockForFunction(callFrame->uncheckedArgument(0));
    return jsNumber(codeBlock ? codeBlock->numberOfOptimizingCompiles() : 0);
}

JSValue optimizeNextInvocation(JSValue function)
{
    if (auto* codeBlock = getSomeBaselineCodeBlockForFunction(function))
        codeBlock->optimizeNextInvocation();
    return jsUndefined();
}

JSValue setNeverInline(JSGlobalObject*, CallFrame* callFrame)
{
    if (auto* executable = getExecutableForFunction(callFrame->argument(0)))
        executable->setNeverInline(true);
    return jsUndefined();
}

} // namespace JSC
