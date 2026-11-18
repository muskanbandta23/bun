#pragma once

// Frames, lists of arguments and calls.

#include "engine/api/Primitives.h"

namespace JSC {

class NativeCallee;

class CalleeBits {
public:
    explicit CalleeBits(uint64_t bits)
        : m_bits(bits)
    {
    }

    bool isNativeCallee() const { return (m_bits & Engine::Layout::nativeCalleeTag) == Engine::Layout::nativeCalleeTag; }
    bool isCell() const { return !isNativeCallee(); }
    JSCell* asCell() const { return reinterpret_cast<JSCell*>(m_bits); }
    NativeCallee* asNativeCallee() const { return reinterpret_cast<NativeCallee*>(m_bits & ~Engine::Layout::nativeCalleeTag); }
    void* rawPtr() const { return reinterpret_cast<void*>(m_bits); }

private:
    uint64_t m_bits;
};

enum class CallFrameSlot : int {
    argumentCountIncludingThis = Engine::Layout::CallFrame_argument_count / sizeof(JSValue),
    firstArgument = Engine::Layout::CallFrame_arguments / sizeof(JSValue),
};

class CallFrame {
    WTF_MAKE_NONCOPYABLE(CallFrame);

public:
    CalleeBits callee() const { return CalleeBits(m_callee); }
    JSObject* jsCallee() const { return reinterpret_cast<JSObject*>(m_callee); }
    void setCallee(JSObject* callee) { m_callee = reinterpret_cast<uint64_t>(callee); }

    ALWAYS_INLINE size_t argumentCountIncludingThis() const { return m_argumentCountIncludingThis; }
    ALWAYS_INLINE size_t argumentCount() const { return argumentCountIncludingThis() - 1; }
    ALWAYS_INLINE JSValue* addressOfArgumentsStart() const { return &Engine::fieldAt<JSValue>(this, Engine::Layout::CallFrame_arguments); }
    ALWAYS_INLINE JSValue uncheckedArgument(size_t index) const
    {
        ASSERT(index < argumentCount());
        return addressOfArgumentsStart()[index];
    }
    ALWAYS_INLINE JSValue argument(size_t index) const { return index < argumentCount() ? uncheckedArgument(index) : jsUndefined(); }
    ALWAYS_INLINE JSValue thisValue() const { return m_thisValue; }
    void setThisValue(JSValue value) { m_thisValue = value; }
    JSValue newTarget() const { return m_thisValue; }

    SourceOrigin callerSourceOrigin(VM&);

private:
    static void checkLayout();

    void* m_caller;
    void* m_returnAddress;
    void* m_codeBlock;
    uint64_t m_callee;
    uint32_t m_argumentCountIncludingThis;
    uint32_t m_callSite;
    JSValue m_thisValue;
};

inline void CallFrame::checkLayout()
{
    ENGINE_CHECK_OFFSET(CallFrame, m_codeBlock, CallFrame_code_block);
    ENGINE_CHECK_OFFSET(CallFrame, m_callee, CallFrame_callee);
    ENGINE_CHECK_OFFSET(CallFrame, m_argumentCountIncludingThis, CallFrame_argument_count);
    ENGINE_CHECK_OFFSET(CallFrame, m_thisValue, CallFrame_this);
    ENGINE_CHECK_SIZE(CallFrame, CallFrame_arguments);
}

// What native code leaves below its frame pointer when it calls into generated code.
class VMEntryRecord {
public:
    EntryFrame* prevTopEntryFrame() const { return m_previousTopEntryFrame; }
    CallFrame* prevTopCallFrame() const { return m_previousTopCallFrame; }

    // The async function or async generator that the call resumes.
    JSCell* m_context;

private:
    EntryFrame* m_previousTopEntryFrame;
    CallFrame* m_previousTopCallFrame;
};
ENGINE_CHECK_SIZE(VMEntryRecord, sizeOfEntryRecord);

inline VMEntryRecord* vmEntryRecord(EntryFrame* frame) { return reinterpret_cast<VMEntryRecord*>(frame) - 1; }

// A list for a local variable, which keeps the cells in it alive.
template<typename T, size_t inlineCapacity = 8, typename OverflowHandler = CrashOnOverflow>
class MarkedVector : public OverflowHandler {
    WTF_MAKE_NONCOPYABLE(MarkedVector);
    WTF_MAKE_NONMOVABLE(MarkedVector);
    WTF_FORBID_HEAP_ALLOCATION;
    static_assert(sizeof(T) == sizeof(EncodedJSValue) && std::is_trivially_copyable_v<T>);

public:
    ALWAYS_INLINE MarkedVector()
        : m_buffer(reinterpret_cast<T*>(m_inlineBuffer))
    {
    }
    ALWAYS_INLINE ~MarkedVector()
    {
        if (m_capacity > inlineCapacity) [[unlikely]]
            Engine::root_free(m_buffer, m_capacity);
    }

    size_t size() const { return m_size; }
    bool isEmpty() const { return !m_size; }
    T& at(size_t index)
    {
        RELEASE_ASSERT(index < m_size);
        return m_buffer[index];
    }
    const T& at(size_t index) const { return const_cast<MarkedVector*>(this)->at(index); }
    T last() const { return at(m_size - 1); }
    T* begin() const { return m_buffer; }
    T* end() const { return m_buffer + m_size; }

    ALWAYS_INLINE void append(T value)
    {
        if (m_size == m_capacity && !grow(m_size + 1)) [[unlikely]]
            return this->overflowed();
        m_buffer[m_size++] = value;
    }
    void appendWithCrashOnOverflow(T value)
    {
        RELEASE_ASSERT(m_size < m_capacity || grow(m_size + 1));
        m_buffer[m_size++] = value;
    }
    void ensureCapacity(size_t capacity)
    {
        if (capacity > m_capacity && !grow(capacity))
            this->overflowed();
    }
    template<typename Functor> void fill(VM&, size_t count, const Functor& functor)
    {
        ASSERT(!m_size);
        if (count > m_capacity && !grow(count))
            return this->overflowed();
        m_size = count;
        functor(m_buffer);
    }
    void removeLast()
    {
        ASSERT(m_size);
        m_size--;
    }
    T takeLast()
    {
        T value = last();
        removeLast();
        return value;
    }
    void clear()
    {
        this->clearOverflow();
        m_size = 0;
    }
    void overflowCheckNotNeeded() {}

private:
    NEVER_INLINE bool grow(size_t needed)
    {
        size_t capacity = std::max<size_t>(needed, static_cast<size_t>(m_capacity) * 2);
        if (capacity > static_cast<size_t>(std::numeric_limits<int32_t>::max()))
            return false;
        auto* buffer = static_cast<T*>(Engine::root_try_allocate(capacity));
        if (!buffer)
            return false;
        memcpy(static_cast<void*>(buffer), m_buffer, m_size * sizeof(T));
        if (m_capacity > inlineCapacity)
            Engine::root_free(m_buffer, m_capacity);
        m_buffer = buffer;
        m_capacity = capacity;
        return true;
    }

    unsigned m_size { 0 };
    unsigned m_capacity { inlineCapacity };
    T* m_buffer;
    EncodedJSValue m_inlineBuffer[inlineCapacity];
};

template<size_t inlineCapacity = 8>
class MarkedArgumentBufferWithSize : public MarkedVector<JSValue, inlineCapacity, RecordOverflow> {
public:
    EncodedJSValue* data() const { return reinterpret_cast<EncodedJSValue*>(this->begin()); }
};

using MarkedArgumentBuffer = MarkedArgumentBufferWithSize<>;

class ArgList {
public:
    ArgList() = default;
    ArgList(CallFrame* callFrame)
        : ArgList(reinterpret_cast<EncodedJSValue*>(callFrame->addressOfArgumentsStart()), callFrame->argumentCount())
    {
    }
    ArgList(CallFrame* callFrame, int first) { ArgList(callFrame).getSlice(first, *this); }
    ArgList(EncodedJSValue* arguments, unsigned count)
        : m_arguments(arguments)
        , m_count(count)
    {
    }
    template<size_t inlineCapacity>
    ArgList(const MarkedVector<JSValue, inlineCapacity, RecordOverflow>& arguments)
        : ArgList(reinterpret_cast<EncodedJSValue*>(arguments.begin()), arguments.size())
    {
    }

    JSValue at(unsigned index) const { return index < m_count ? JSValue::decode(m_arguments[index]) : jsUndefined(); }
    bool isEmpty() const { return !m_count; }
    size_t size() const { return m_count; }
    EncodedJSValue* data() const { return m_arguments; }
    std::span<const EncodedJSValue> span() const { return { m_arguments, m_count }; }

    void getSlice(int first, ArgList& result) const
    {
        unsigned skipped = std::min<unsigned>(first, m_count);
        result = ArgList(m_arguments + skipped, m_count - skipped);
    }

private:
    EncodedJSValue* m_arguments { nullptr };
    unsigned m_count { 0 };
};

inline CallData getCallData(JSValue value)
{
    CallData data;
    Engine::call_data_for_call(value, data);
    return data;
}
inline CallData getCallData(JSCell* cell) { return getCallData(JSValue(cell)); }

inline CallData getConstructData(JSValue value)
{
    CallData data;
    Engine::call_data_for_construct(value, data);
    return data;
}

inline JSValue call(JSGlobalObject* globalObject, JSValue function, const CallData&, JSValue thisValue, const ArgList& arguments) { return Engine::call(globalObject, function, thisValue, arguments.span()); }
// Takes the exception, and returns undefined then.
JSValue call(JSGlobalObject*, JSValue function, const CallData&, JSValue thisValue, const ArgList&, NakedPtr<Exception>& returnedException);
JSValue call(JSGlobalObject*, JSValue function, JSValue thisValue, const ArgList&, ASCIILiteral errorMessage);
inline JSValue call(JSGlobalObject* globalObject, JSValue function, const ArgList& arguments, ASCIILiteral errorMessage) { return call(globalObject, function, function, arguments, errorMessage); }

inline JSObject* construct(JSGlobalObject* globalObject, const JSValue& constructor, const CallData&, const ArgList& arguments, const JSValue& newTarget) { return Engine::construct(globalObject, constructor, arguments.span(), newTarget); }
inline JSObject* construct(JSGlobalObject* globalObject, const JSValue& constructor, const CallData& constructData, const ArgList& arguments) { return construct(globalObject, constructor, constructData, arguments, constructor); }
JSObject* construct(JSGlobalObject*, const JSValue& constructor, const JSValue& newTarget, const ArgList&, ASCIILiteral errorMessage);
inline JSObject* construct(JSGlobalObject* globalObject, const JSValue& constructor, const ArgList& arguments, ASCIILiteral errorMessage) { return construct(globalObject, constructor, constructor, arguments, errorMessage); }

} // namespace JSC
