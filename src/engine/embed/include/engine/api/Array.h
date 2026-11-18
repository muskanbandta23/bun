#pragma once

// Arrays.

#include "engine/api/Call.h"

namespace JSC {

class JSArray : public JSNonFinalObject {
public:
    ENGINE_DECLARE_CLASS(Array)

    static JSArray* tryCreate(VM& vm, Structure* structure, unsigned length = 0) { return Engine::array_try_create(vm, structure, length); }
    static JSArray* create(VM& vm, Structure* structure, unsigned length = 0)
    {
        JSArray* array = tryCreate(vm, structure, length);
        RELEASE_ASSERT(array);
        return array;
    }
    // Has room for all elements, which the caller stores before a script sees the array.
    static JSArray* tryCreateUninitializedRestricted(ObjectInitializationScope& scope, Structure* structure, unsigned length) { return Engine::array_try_create_dense(scope.vm(), structure, length); }
    static JSArray* tryCreateUninitializedRestricted(ObjectInitializationScope& scope, GCDeferralContext*, Structure* structure, unsigned length) { return tryCreateUninitializedRestricted(scope, structure, length); }

    unsigned length() const { return getArrayLength(); }
    bool setLength(JSGlobalObject* globalObject, unsigned length, bool shouldThrow = false) { return Engine::array_set_length(this, globalObject, length, shouldThrow); }
    void push(JSGlobalObject* globalObject, JSValue value) { Engine::array_push(this, globalObject, value); }
    // Reading an element or a hole runs no code.
    bool canDoFastIndexedAccess() const { return !hasAnyArrayStorage(indexingType()) && !Engine::object_holes_must_forward_to_prototype(this); }
    bool isIteratorProtocolFastAndNonObservable() const { return Engine::array_is_iteration_pristine(this); }
};
ENGINE_CLASS_HAS_TYPES(JSArray, ArrayType, DerivedArrayType);

inline bool isJSArray(JSCell* cell) { return cell->type() == ArrayType; }
inline bool isJSArray(JSValue value) { return value.isCell() && isJSArray(value.asCell()); }
inline JSArray* asArray(JSCell* cell) { return uncheckedDowncast<JSArray>(cell); }
inline JSArray* asArray(JSValue value) { return asArray(value.asCell()); }

inline bool isArray(JSGlobalObject* globalObject, JSValue value)
{
    if (!value.isCell())
        return false;
    if (value.asCell()->type() == ProxyObjectType) [[unlikely]]
        return Engine::proxy_is_array(globalObject, value);
    return value.asCell()->inherits<JSArray>();
}

inline bool containsHole(const double* elements, unsigned length) { return std::any_of(elements, elements + length, [](double element) { return element != element; }); }
inline bool containsHole(const WriteBarrier<Unknown>* elements, unsigned length) { return std::any_of(elements, elements + length, [](auto& element) { return !element; }); }

inline JSArray* constructArray(JSGlobalObject* globalObject, Structure* structure, const ArgList& values) { return Engine::array_from_values_with_shape(globalObject, structure, values.span()); }
inline JSArray* constructArray(JSGlobalObject* globalObject, ArrayAllocationProfile*, const ArgList& values) { return Engine::array_from_values(globalObject, values.span()); }
inline JSArray* constructArray(JSGlobalObject* globalObject, ArrayAllocationProfile*, const JSValue* values, unsigned length) { return Engine::array_from_values(globalObject, { reinterpret_cast<const EncodedJSValue*>(values), length }); }
inline JSArray* constructEmptyArray(JSGlobalObject* globalObject, ArrayAllocationProfile*, unsigned length = 0) { return Engine::array_with_length(globalObject, length); }
inline JSArray* constructArrayPair(JSGlobalObject* globalObject, JSValue first, JSValue second)
{
    const JSValue values[] = { first, second };
    return constructArray(globalObject, nullptr, values, 2);
}

// Until the functor returns false or something throws.
template<typename Functor> void forEachInArrayLike(JSGlobalObject* globalObject, JSObject* object, Functor functor)
{
    auto scope = DECLARE_THROW_SCOPE(getVM(globalObject));
    uint64_t length = toLength(globalObject, object);
    RETURN_IF_EXCEPTION(scope, void());
    for (uint64_t index = 0; index < length; index++) {
        JSValue value = object->getIndex(globalObject, index);
        RETURN_IF_EXCEPTION(scope, void());
        bool shouldContinue = functor(value);
        RETURN_IF_EXCEPTION(scope, void());
        if (!shouldContinue)
            return;
    }
}

// A list without holes that never changes.
class JSCellButterfly final : public JSCell {
public:
    ENGINE_DECLARE_CLASS(ImmutableArray)

    static JSCellButterfly* tryCreateFromArgList(VM& vm, ArgList values) { return Engine::immutable_array_try_from_values(vm, values.span()); }

    unsigned length() const { return Engine::fieldAt<uint32_t>(this, Engine::Layout::ImmutableArray_length); }
    JSValue get(unsigned index) const
    {
        if (elementKind() == DoubleShape)
            return jsDoubleNumber((&Engine::fieldAt<double>(this, Engine::Layout::ImmutableArray_elements))[index]);
        return (&Engine::fieldAt<JSValue>(this, Engine::Layout::ImmutableArray_elements))[index];
    }
};
ENGINE_CLASS_HAS_TYPE(JSCellButterfly, JSCellButterflyType);

} // namespace JSC
