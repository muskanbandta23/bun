#include "headers.h"
#include "root.h"

#pragma once

namespace JSC {
class Structure;
class Identifier;
class SourceCode;
class SourceProvider;
} // namespace JSC

#include <JavaScriptCore/CachedBytecode.h>
#include <JavaScriptCore/JSGlobalObject.h>
#include <JavaScriptCore/JSTypeInfo.h>
#include <JavaScriptCore/SourceProvider.h>
#include <JavaScriptCore/Structure.h>

namespace Zig {

class GlobalObject;

JSC::SourceOrigin toSourceOrigin(const String& sourceURL, bool isBuiltin);
class SourceProvider final : public JSC::SourceProvider {
    WTF_DEPRECATED_MAKE_FAST_ALLOCATED(SourceProvider);
    using Base = JSC::SourceProvider;
    using SourceOrigin = JSC::SourceOrigin;

public:
    static Ref<SourceProvider> create(
        Zig::GlobalObject*,
        ResolvedSource& resolvedSource,
        JSC::SourceProviderSourceType sourceType = JSC::SourceProviderSourceType::Module,
        bool isBuiltIn = false);
    ~SourceProvider();

    RefPtr<JSC::CachedBytecode> cachedBytecode() const final
    {
        return m_cachedBytecode.copyRef();
    };

    std::unique_ptr<bun_ModuleInfoDeserialized> m_moduleInfo;
    uint32_t m_tag { 0 };
    bool m_alreadyBundled { false };
    bool m_dependsOnMoreThanSource { false };
    // Set by IsolatedModuleCache::insert() and compared by lookup().
    uint64_t m_madeFromHash { 0 };

private:
    SourceProvider(void* bunVM, ResolvedSource& resolvedSource, const WTF::String& source,
        const SourceOrigin& sourceOrigin, WTF::String&& sourceURL,
        const TextPosition& startPosition, JSC::SourceProviderSourceType sourceType)
        : Base(source, sourceOrigin, WTF::move(sourceURL), startPosition, sourceType, resolvedSource.is_static ? JSC::SourceProvenance::Static : JSC::SourceProvenance::GeneratedAtRuntime)
        , m_moduleInfo(WTF::move(resolvedSource.module_info))
        , m_tag(resolvedSource.tag)
        , m_alreadyBundled(resolvedSource.already_bundled)
        , m_dependsOnMoreThanSource(resolvedSource.depends_on_more_than_source)
        , m_bunVM(bunVM)
        , m_cachedBytecode(resolvedSource.bytecode_cache.take())
    {
        if (m_cachedBytecode)
            setHash(resolvedSource.source_code_hash);
    }

    // Stored directly (not via the creating global) so the destructor stays
    // valid when the provider outlives its global under --isolate caching.
    void* m_bunVM;
    RefPtr<JSC::CachedBytecode> m_cachedBytecode;
};

} // namespace Zig

namespace Bun {

// bun_bundler::bytecode_order::CodeNamesRef
struct BytecodeOrderNamesRef {
    struct Function {
        uint32_t start;
        uint32_t kind;
        uint64_t name;
    };

    uint64_t module;
    const Function* functions;
    size_t functionCount;
};

// The bytecode of all modules of an executable in one payload (--bytecode-order), in the order in which they are added.
// Each entry follows its size.
class BytecodeLinkEncoder {
    WTF_DEPRECATED_MAKE_FAST_ALLOCATED(BytecodeLinkEncoder);

public:
    // False if there is none.
    bool add(RefPtr<JSC::CachedBytecode>&&, const BytecodeOrderNamesRef&);
    // `entryOffset` is zero for bytecode that is on its own.
    static std::span<const uint8_t> entry(std::span<const uint8_t> payload, uint32_t entryOffset);

    Vector<uint64_t> sortedHotFunctions;
    uint32_t namedHotFunctions { 0 };
    Vector<uint32_t> entryOffsets;
    Vector<uint8_t> payload;
};

JSC_DECLARE_HOST_FUNCTION(jsSourceHasLineStarts);

} // namespace Bun
