#pragma once

#include "engine/support/Clocks.h"
#include "engine/support/Ref.h"
#include "engine/support/ThreadSafeRefCounted.h"

#include <optional>

namespace WTF {

class Stopwatch final : public ThreadSafeRefCounted<Stopwatch> {
public:
    static Ref<Stopwatch> create() { return adoptRef(*new Stopwatch); }

    bool isActive() const { return m_startTime.has_value(); }

    void start()
    {
        ASSERT(!isActive());
        m_startTime = MonotonicTime::now();
    }

    void stop()
    {
        ASSERT(isActive());
        m_timeOfEarlierRuns = elapsedTime();
        m_startTime.reset();
    }

    void reset()
    {
        m_timeOfEarlierRuns = {};
        m_startTime.reset();
    }

    Seconds elapsedTime() const { return elapsedTimeSince(MonotonicTime::now()); }

    // What it shows at that moment of the current run.
    Seconds elapsedTimeSince(MonotonicTime time) const { return isActive() ? m_timeOfEarlierRuns + (time - *m_startTime) : m_timeOfEarlierRuns; }

private:
    Stopwatch() = default;

    Seconds m_timeOfEarlierRuns;
    std::optional<MonotonicTime> m_startTime;
};

}

using WTF::Stopwatch;
