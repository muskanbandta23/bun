#pragma once

// Compiled code that is kept: in memory for the next run, and as bytes for the next process.

#include "engine/api/Code.h"

namespace JSC {

enum class SourceCodeType : bool { ProgramType,
    ModuleType };

class CachedBytecode : public ThreadSafeRefCounted<CachedBytecode> {
public:
    using Destructor = Function<void(const void*)>;

    // Without a destructor the bytes outlive this.
    static Ref<CachedBytecode> create(std::span<const uint8_t> bytes, Destructor&& destructor) { return adoptRef(*new CachedBytecode(bytes, WTF::move(destructor))); }
    static Ref<CachedBytecode> createByCopying(std::span<const uint8_t> bytes)
    {
        auto* copy = static_cast<uint8_t*>(fastMalloc(bytes.size()));
        memcpySpan(std::span { copy, bytes.size() }, bytes);
        return create({ copy, bytes.size() }, [](const void* bytes) { fastFree(const_cast<void*>(bytes)); });
    }

    ~CachedBytecode()
    {
        if (m_destructor)
            m_destructor(m_bytes.data());
    }

    std::span<const uint8_t> span() const { return m_bytes; }
    size_t size() const { return m_bytes.size(); }

private:
    CachedBytecode(std::span<const uint8_t> bytes, Destructor&& destructor)
        : m_bytes(bytes)
        , m_destructor(WTF::move(destructor))
    {
    }

    std::span<const uint8_t> m_bytes;
    Destructor m_destructor;
};

// Tells this build from others.
inline void setBytecodeCacheVersion(uint32_t version) { Engine::bytecode_set_version(version); }
// With the functions that are nested up to `depth` levels deep. Null if the text has an error or the code cannot be stored.
RefPtr<CachedBytecode> generateBytecode(VM&, const SourceCode&, SourceCodeType, unsigned depth = std::numeric_limits<unsigned>::max());
// Whether this build has generated the bytes from that text.
inline bool isUsableBytecode(VM& vm, const SourceCode& source, SourceCodeType type, std::span<const uint8_t> bytes) { return Engine::bytecode_is_usable(vm, source, type == SourceCodeType::ModuleType, bytes); }

// Has the address of the VM.
class CodeCache {
public:
    size_t workingSetMaxEntries() const { return Engine::code_cache_max_entries(vm()); }
    void setWorkingSetMaxEntries(size_t count) { Engine::code_cache_set_max_entries(vm(), count); }

private:
    const VM& vm() const { return Engine::fieldAt<VM>(this, 0); }
};

// A script that can run any number of times, in any realm of the VM.
class UnlinkedProgramCodeBlock {
    WTF_MAKE_NONCOPYABLE(UnlinkedProgramCodeBlock);

public:
    // For the global scope, unless there is another. Null if there is an error.
    static RefPtr<UnlinkedProgramCodeBlock> compile(JSGlobalObject*, const SourceCode&, ParserError&, JSScope* = nullptr);

    void ref() const { Engine::compiled_script_retain(this); }
    void deref() const { Engine::compiled_script_release(this); }

    // Not if a debugger or a profiler has come or gone.
    bool isUsableIn(JSGlobalObject* globalObject) const { return Engine::compiled_script_is_usable_in(*this, globalObject); }
    // Without running the script. Null if it has no function.
    JSFunction* createFirstFunction(JSGlobalObject* globalObject, JSScope* scope) const { return Engine::compiled_script_create_first_function(*this, globalObject, scope); }
};

// Compiles the source if `code` is null.
JSValue evaluate(JSGlobalObject*, const SourceCode&, UnlinkedProgramCodeBlock* code, JSValue thisValue, NakedPtr<Exception>& returnedException);

} // namespace JSC
