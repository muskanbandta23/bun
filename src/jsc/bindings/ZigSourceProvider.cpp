#include "root.h"

#include "helpers.h"

#include "ZigSourceProvider.h"
#include "MimallocWTFMalloc.h"

#include "ZigGlobalObject.h"
#include "wtf/Assertions.h"

#include <JavaScriptCore/Completion.h>
#include <wtf/Scope.h>
#include <wtf/text/StringHash.h>
#include <sys/stat.h>
#include <mimalloc.h>
#include <JavaScriptCore/CodeCache.h>
#include "BunBuiltinNames.h"

RefPtr<JSC::CachedBytecode> ResolvedSourceBytecode::take()
{
    static_assert(offsetof(ResolvedSourceBytecode, m_owned) == 16 && offsetof(ResolvedSourceBytecode, m_entryOffset) == 20, "Bytecode layout is mirrored in src/jsc/ResolvedSource.rs");
    if (!m_bytes)
        return nullptr;
    std::span<const uint8_t> bytes { std::exchange(m_bytes, nullptr), m_size };
    if (!m_owned)
        return JSC::CachedBytecode::create(Bun::BytecodeLinkEncoder::entry(bytes, m_entryOffset), nullptr);
    return JSC::CachedBytecode::create(bytes, [](const void* ptr) {
        ResolvedSource__freeBytecode(static_cast<uint8_t*>(const_cast<void*>(ptr)));
    });
}

namespace Zig {

using SourceOrigin = JSC::SourceOrigin;
using String = WTF::String;
using SourceProviderSourceType = JSC::SourceProviderSourceType;

SourceOrigin toSourceOrigin(const String& sourceURL, bool isBuiltin)
{

    ASSERT_WITH_MESSAGE(!sourceURL.startsWith("file://"_s), "specifier should not already be a file URL");

    if (isBuiltin) {
        if (sourceURL.startsWith("node:"_s)) {
            return SourceOrigin(WTF::URL(makeString("builtin://node/"_s, sourceURL.substring(5))));
        } else if (sourceURL.startsWith("bun:"_s)) {
            return SourceOrigin(WTF::URL(makeString("builtin://bun/"_s, sourceURL.substring(4))));
        } else {
            return SourceOrigin(WTF::URL(makeString("builtin://"_s, sourceURL)));
        }
    }
    return SourceOrigin(WTF::URL::fileURLWithFileSystemPath(sourceURL));
}

extern "C" void ByteRangeMapping__generate(const BunString* sourceURL, const BunString* code, int sourceID, bool alreadyBundled);

extern "C" bool BunTest__shouldGenerateCodeCoverage(const BunString* sourceURL);
extern "C" void Bun__addSourceProviderSourceMap(void* bun_vm, SourceProvider* opaque_source_provider, const BunString* specifier);
extern "C" void Bun__removeSourceProviderSourceMap(void* bun_vm, SourceProvider* opaque_source_provider, const BunString* specifier);

Ref<SourceProvider> SourceProvider::create(
    Zig::GlobalObject* globalObject,
    ResolvedSource& resolvedSource,
    JSC::SourceProviderSourceType sourceType,
    bool isBuiltin)
{
    // Use BunTranspiledModule when module_info is present.
    // This allows JSC to skip parsing during the analyze phase (uses pre-computed imports/exports).
    // Bytecode cache (if present) is used separately during the evaluate phase.
    if (resolvedSource.module_info != nullptr) {
        ASSERT(!resolvedSource.isCommonJSModule);
        sourceType = JSC::SourceProviderSourceType::BunTranspiledModule;
    }

    auto string = resolvedSource.source_code.transferToWTFString();
    auto sourceURLString = resolvedSource.source_url.transferToWTFString();

    bool isCodeCoverageEnabled = !!globalObject->vm().controlFlowProfiler();

    bool shouldGenerateCodeCoverage = false;
    if (isCodeCoverageEnabled && !isBuiltin) {
        BunString sourceURLBunString = Bun::toString(sourceURLString);
        shouldGenerateCodeCoverage = BunTest__shouldGenerateCodeCoverage(&sourceURLBunString);
    }

    const auto getSourceOrigin = [&]() -> SourceOrigin {
        auto originPath = resolvedSource.origin_path.transferToWTFString();
        if (!originPath.isEmpty())
            return SourceOrigin(WTF::URL::fileURLWithFileSystemPath(originPath));
        return toSourceOrigin(sourceURLString, isBuiltin);
    };

    auto origin = getSourceOrigin();
    auto provider = adoptRef(*new SourceProvider(globalObject->bunVM(), resolvedSource, string, origin, WTF::move(sourceURLString), TextPosition(), sourceType));

    if (shouldGenerateCodeCoverage) {
        BunString providerURL = Bun::toString(provider->sourceURL());
        WTF::String providerSourceString = provider->source().toStringWithoutCopying();
        BunString providerSource = Bun::toString(providerSourceString);
        ByteRangeMapping__generate(&providerURL, &providerSource, provider->asID(), provider->m_alreadyBundled);
    }

    if (provider->m_alreadyBundled) {
        BunString str = Bun::toString(provider->sourceURL());
        Bun__addSourceProviderSourceMap(globalObject->bunVM(), provider.ptr(), &str);
    }

    return provider;
}

SourceProvider::~SourceProvider()
{
    if (m_alreadyBundled) {
        BunString str = Bun::toString(sourceURL());
        Bun__removeSourceProviderSourceMap(m_bunVM, this, &str);
    }
}

extern "C" void CachedBytecode__deref(JSC::CachedBytecode* cachedBytecode)
{
    cachedBytecode->deref();
}

JSC::VM& vmForBytecodeCache();
static thread_local JSC::VM* s_vmForBytecodeCache = nullptr;
// The builtins parse with private @names; this VM has no JSVMClientData to register Bun's, so it owns them directly.
static thread_local std::unique_ptr<WebCore::BunBuiltinNames> s_builtinNamesForBytecodeCache;

JSC::VM& vmForBytecodeCache()
{
    if (!s_vmForBytecodeCache) {
        const auto heapSize = JSC::HeapType::Small;
        auto vmPtr = JSC::VM::tryCreate(heapSize);
        vmPtr->refSuppressingSaferCPPChecking();
        s_vmForBytecodeCache = vmPtr.get();
        vmPtr->heap.acquireAccess();
    }
    return *s_vmForBytecodeCache;
}

void ensureBuiltinNamesForBytecodeCache(JSC::VM& vm)
{
    ASSERT(&vm == s_vmForBytecodeCache);
    if (!s_builtinNamesForBytecodeCache)
        s_builtinNamesForBytecodeCache = WebCore::BunBuiltinNames::createStandalone(vm);
}

extern "C" void Bun__destroyBytecodeCacheVM()
{
    JSC::VM* vm = std::exchange(s_vmForBytecodeCache, nullptr);
    if (!vm)
        return;
    s_builtinNamesForBytecodeCache = nullptr;
    vm->derefSuppressingSaferCPPChecking();
}

// The names in the module_info of an executable's modules. The slot of one is where its header is: the length, and
// whether it is 8-bit in the lowest bit. The characters follow, up to the next multiple of 4.
using EncoderStringTable = Vector<uint8_t>;

extern "C" EncoderStringTable* Bun__EncoderStringTable__create()
{
    return new EncoderStringTable();
}

extern "C" void Bun__EncoderStringTable__destroy(EncoderStringTable* table)
{
    delete table;
}

extern "C" void Bun__EncoderStringTable__serialize(EncoderStringTable* table, const uint64_t*, size_t, void* ctx, void (*append)(void* ctx, const uint8_t* bytes, size_t len))
{
    append(ctx, table->span().data(), table->size());
}

template<typename Character> static uint32_t appendToStringTable(EncoderStringTable& table, std::span<const Character> characters)
{
    uint32_t slot = table.size();
    uint32_t header = characters.size() << 1 | (sizeof(Character) == 1);
    table.append(asByteSpan(header));
    table.append(asByteSpan(characters));
    while (table.size() % sizeof(header))
        table.append(0);
    return slot;
}

extern "C" uint32_t Bun__EncoderStringTable__slotForLatin1(EncoderStringTable* table, const Latin1Character* chars, size_t length)
{
    return appendToStringTable(*table, std::span { chars, length });
}

extern "C" uint32_t Bun__EncoderStringTable__slotForUTF16(EncoderStringTable* table, const char16_t* chars, size_t length)
{
    return appendToStringTable(*table, std::span { chars, length });
}

extern "C" void Bun__DecoderStringTable__install(JSC::VM* vm, const uint8_t* bytes, size_t len)
{
    WebCore::clientData(*vm)->moduleInfoStrings = { bytes, len };
}

extern "C" bool JSC__IdentifierArray__setFromSlot(Identifier* identifierArray, size_t n, VM& vm, uint32_t slot)
{
    auto table = WebCore::clientData(vm)->moduleInfoStrings;
    uint32_t header;
    if (slot % sizeof(header) || static_cast<size_t>(slot) + sizeof(header) > table.size())
        return false;
    memcpy(&header, table.data() + slot, sizeof(header));
    auto rest = table.subspan(slot + sizeof(header));
    size_t length = header >> 1;
    bool is8Bit = header & 1;
    if ((is8Bit ? length : length * sizeof(char16_t)) > rest.size())
        return false;
    identifierArray[n] = is8Bit
        ? Identifier::fromString(vm, std::span { reinterpret_cast<const Latin1Character*>(rest.data()), length })
        : Identifier::fromString(vm, std::span { reinterpret_cast<const char16_t*>(rest.data()), length });
    return true;
}

extern "C" void JSC__Heap__setInitialAllocationBudget(JSC::VM* vm, size_t bytes)
{
    vm->heap.setInitialAllocationBudget(bytes);
}

static RefPtr<JSC::CachedBytecode> generateBytecode(const BunString* sourceProviderURL, const BunString* inputSourceCode, bool isModule, uint32_t depth)
{
    auto sourceCode = JSC::makeSource(inputSourceCode->toWTFString(), toSourceOrigin(sourceProviderURL->toWTFString(), false), JSC::SourceTaintedOrigin::Untainted);
    return JSC::generateBytecode(vmForBytecodeCache(), sourceCode, isModule ? JSC::SourceCodeType::ModuleType : JSC::SourceCodeType::ProgramType, depth);
}

static bool handOverBytecode(RefPtr<JSC::CachedBytecode>&& cachedBytecode, const uint8_t** outputByteCode, size_t* outputByteCodeSize, JSC::CachedBytecode** cachedBytecodePtr)
{
    if (!cachedBytecode)
        return false;
    *outputByteCode = cachedBytecode->span().data();
    *outputByteCodeSize = cachedBytecode->size();
    *cachedBytecodePtr = cachedBytecode.leakRef();
    return true;
}

extern "C" bool generateCachedModuleByteCodeFromSourceCode(const BunString* sourceProviderURL, const BunString* inputSourceCode, uint32_t depth, bool, const uint8_t** outputByteCode, size_t* outputByteCodeSize, JSC::CachedBytecode** cachedBytecodePtr, EncoderStringTable*)
{
    return handOverBytecode(generateBytecode(sourceProviderURL, inputSourceCode, true, depth), outputByteCode, outputByteCodeSize, cachedBytecodePtr);
}

extern "C" bool generateCachedCommonJSProgramByteCodeFromSourceCode(const BunString* sourceProviderURL, const BunString* inputSourceCode, uint32_t depth, bool, const uint8_t** outputByteCode, size_t* outputByteCodeSize, JSC::CachedBytecode** cachedBytecodePtr, EncoderStringTable*)
{
    return handOverBytecode(generateBytecode(sourceProviderURL, inputSourceCode, false, depth), outputByteCode, outputByteCodeSize, cachedBytecodePtr);
}

extern "C" Bun::BytecodeLinkEncoder* Bun__BytecodeLinkEncoder__create(EncoderStringTable*, const uint64_t* hotFunctions, size_t hotFunctionCount, const uint64_t*, size_t, const uint64_t*, size_t, const uint64_t*, size_t)
{
    auto* encoder = new Bun::BytecodeLinkEncoder;
    encoder->sortedHotFunctions.append(std::span { hotFunctions, hotFunctionCount });
    std::ranges::sort(encoder->sortedHotFunctions);
    return encoder;
}

extern "C" void Bun__BytecodeLinkEncoder__destroy(Bun::BytecodeLinkEncoder* encoder)
{
    delete encoder;
}

extern "C" bool Bun__BytecodeLinkEncoder__addModule(Bun::BytecodeLinkEncoder* encoder, const BunString* sourceProviderURL, const BunString* inputSourceCode, bool isModule, uint32_t depth, bool, const Bun::BytecodeOrderNamesRef& names)
{
    return encoder->add(generateBytecode(sourceProviderURL, inputSourceCode, isModule, depth), names);
}

// `entryOffsets` has room for one number per successful add* call. All is in the first region.
extern "C" bool Bun__BytecodeLinkEncoder__finish(Bun::BytecodeLinkEncoder* encoder, const uint8_t** outputByteCode, size_t* outputByteCodeSize, JSC::CachedBytecode** cachedBytecodePtr, uint32_t* entryOffsets, size_t entryOffsetCount, uint32_t* regionEnds, uint32_t* namedHotFunctions, uint32_t* placedHotFunctions, uint32_t* functionsWithoutName)
{
    constexpr size_t regionCount = 6; // bun_resolver::LINKED_BYTECODE_REGION_COUNT
    if (encoder->entryOffsets.size() != entryOffsetCount)
        return false;
    memcpySpan(std::span { entryOffsets, entryOffsetCount }, encoder->entryOffsets.span());
    std::fill_n(regionEnds, regionCount, encoder->payload.size());
    *namedHotFunctions = encoder->namedHotFunctions;
    *placedHotFunctions = 0;
    *functionsWithoutName = 0;
    return handOverBytecode(JSC::CachedBytecode::createByCopying(encoder->payload.span()), outputByteCode, outputByteCodeSize, cachedBytecodePtr);
}

// Nothing is recorded of what a process reads out of its bytecode (BUN_BYTECODE_ORDER_OUT, BUN_BYTECODE_DIGEST_OUT).
extern "C" void Bun__BytecodeOrder__enableRecording(JSC::VM*)
{
}

extern "C" void Bun__BytecodeOrder__setLinkedPayload(JSC::VM*, const uint8_t*, size_t, const uint32_t*, size_t)
{
}

extern "C" void Bun__BytecodeOrder__takeRecording(void* ctx, void (*take)(void* ctx, const void* sources, size_t sourceCount, const void* functions, size_t functionCount, const unsigned* evaluatedSources, size_t evaluatedSourceCount, const unsigned* rejectedSources, size_t rejectedSourceCount, const uint64_t* strings, size_t stringCount))
{
    take(ctx, nullptr, 0, nullptr, 0, nullptr, 0, nullptr, 0, nullptr, 0);
}

extern "C" bool Bun__BytecodeOrder__digestModule(JSC::VM*, const BunString*, const BunString*, bool, uint8_t*, size_t, uint32_t, uint64_t*, uint32_t*)
{
    return false;
}

extern "C" bool Bun__BytecodeOrder__digestInternalModule(JSC::VM*, uint32_t, uint8_t*, size_t, uint32_t, uint64_t*, uint32_t*)
{
    return false;
}

extern "C" BunString ZigSourceProvider__getSourceSlice(SourceProvider* provider)
{
    return Bun::toStringView(provider->source());
}

}; // namespace Zig

namespace Bun {

bool BytecodeLinkEncoder::add(RefPtr<JSC::CachedBytecode>&& bytecode, const BytecodeOrderNamesRef& names)
{
    if (!bytecode || payload.size() + sizeof(uint32_t) + bytecode->size() > std::numeric_limits<int32_t>::max())
        return false;
    uint32_t size = bytecode->size();
    payload.append(asByteSpan(size));
    entryOffsets.append(payload.size());
    payload.append(bytecode->span());
    for (auto& function : std::span { names.functions, names.functionCount })
        namedHotFunctions += std::ranges::binary_search(sortedHotFunctions, function.name);
    return true;
}

std::span<const uint8_t> BytecodeLinkEncoder::entry(std::span<const uint8_t> payload, uint32_t entryOffset)
{
    uint32_t size;
    if (entryOffset < sizeof(size) || entryOffset > payload.size())
        return payload;
    memcpy(&size, payload.data() + entryOffset - sizeof(size), sizeof(size));
    auto rest = payload.subspan(entryOffset);
    return rest.first(std::min<size_t>(size, rest.size()));
}

// bun:internal-for-testing: whether the source of a function knows where its lines start. Before anything asked for a
// position in it, that means they came with its code: from the parse, or out of the bytecode.
JSC_DEFINE_HOST_FUNCTION(jsSourceHasLineStarts, (JSC::JSGlobalObject*, JSC::CallFrame* callFrame))
{
    auto* function = dynamicDowncast<JSC::JSFunction>(callFrame->argument(0));
    if (!function || function->isHostFunction())
        return JSValue::encode(jsUndefined());
    return JSValue::encode(jsBoolean(function->jsExecutable()->sourceProvider()->lineStartTableIsBuilt()));
}

} // namespace Bun

// What StringImpl::hash() returns for an 8-bit string with these bytes; `bun build --compile` records it per module.
extern "C" uint32_t Bun__WTFStringHashLatin1(const Latin1Character* characters, size_t length)
{
    return StringHasher::computeHashAndMaskTop8Bits(std::span { characters, length });
}

extern "C" uint32_t Bun__WTFStringHashUTF16(const char16_t* characters, size_t length)
{
    return StringHasher::computeHashAndMaskTop8Bits(std::span { characters, length });
}
