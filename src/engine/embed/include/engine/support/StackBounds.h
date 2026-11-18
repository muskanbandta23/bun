#pragma once

#include "engine/support/Assertions.h"

extern "C" void Support__currentStackBounds(void** origin, void** end);

namespace WTF {

ALWAYS_INLINE void* currentStackPointer()
{
    void* result;
#if CPU(X86_64)
    asm volatile("movq %%rsp, %0" : "=r"(result));
#else
    asm volatile("mov %0, sp" : "=r"(result));
#endif
    return result;
}

// The stack grows from the origin down to the end.
class StackBounds {
public:
    static constexpr StackBounds emptyBounds() { return {}; }

    static StackBounds currentThreadStackBoundsForEmbedder()
    {
        StackBounds result;
        Support__currentStackBounds(&result.m_origin, &result.m_end);
        return result;
    }

    bool isEmpty() const { return !m_origin; }
    void* origin() const { return m_origin; }
    void* end() const { return m_end; }

private:
    void* m_origin { nullptr };
    void* m_end { nullptr };
};

}

using WTF::currentStackPointer;
using WTF::StackBounds;
