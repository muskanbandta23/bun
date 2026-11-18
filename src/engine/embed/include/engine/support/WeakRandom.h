#pragma once

#include "engine/support/Noncopyable.h"

#include <cstdint>

namespace WTF {
class WeakRandom;
}

extern "C" uint32_t Support__WeakRandom__seed(const WTF::WeakRandom*);
extern "C" void Support__WeakRandom__setSeed(WTF::WeakRandom*, uint32_t);

namespace WTF {

// A `Cell<bun_engine_number::Random>`.
class WeakRandom final {
    WTF_MAKE_NONCOPYABLE(WeakRandom);

public:
    unsigned seed() const { return Support__WeakRandom__seed(this); }
    void setSeed(unsigned seed) { Support__WeakRandom__setSeed(this, seed); }
};

}

using WTF::WeakRandom;
