#include "engine/Engine.h"

namespace JSC {

Engine::Options Options::s_values;
bool Options::s_areFrozen;

bool Options::setOption(const char* assignment)
{
#if ASSERT_ENABLED
    constexpr auto allocationLimit = "maxSingleAllocationSize="_s;
    if (StringView::fromLatin1(assignment).startsWithIgnoringASCIICase(allocationLimit)) {
        size_t limit;
        if (sscanf(assignment + allocationLimit.length(), "%zu", &limit) != 1)
            return false;
        FastMalloc::maxSingleAllocationSize = limit ? limit : std::numeric_limits<size_t>::max();
        return true;
    }
#endif
    return Engine::options_set(s_values, byteCast<uint8_t>(unsafeSpan(assignment)));
}

void Options::freeze()
{
    static std::once_flag once;
    std::call_once(once, [] {
        Engine::options_freeze(s_values);
        s_areFrozen = true;
    });
}

void initialize(const ScopedLambda<void()>& customizeOptions)
{
    auto& options = Options::s_values;
    Engine::options_initialize(&options, sizeof(options));
    customizeOptions();
    if (unsigned level = options.dumpOptions) [[unlikely]] {
        dataLogLn(level == 1 ? "Modified JSC options:"_s : "All JSC options:"_s);
        Engine::options_dump(options, level == 1);
    }
}

} // namespace JSC
