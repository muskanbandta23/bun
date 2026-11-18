#pragma once

#include "engine/support/Atomics.h"
#include "engine/support/FastMalloc.h"
#include "engine/support/Forward.h"

#include <cstring>

namespace WTF {

// Counts the CheckedPtrs to the object. The class needs WTF_OVERRIDE_DELETE_FOR_CHECKED_PTR.
template<typename T>
class CanMakeThreadSafeCheckedPtr {
public:
    CanMakeThreadSafeCheckedPtr() = default;
    CanMakeThreadSafeCheckedPtr(const CanMakeThreadSafeCheckedPtr&) {}
    CanMakeThreadSafeCheckedPtr& operator=(const CanMakeThreadSafeCheckedPtr&) { return *this; }
    ~CanMakeThreadSafeCheckedPtr() { static_assert(requires { typename T::WTFDidOverrideDeleteForCheckedPtr; }); }

    uint32_t checkedPtrCount() const { return m_count.load(std::memory_order_acquire); }
    void incrementCheckedPtrCount() const { m_count.fetch_add(1, std::memory_order_relaxed); }
    // The count of a deleted object is zero.
    void decrementCheckedPtrCount() const { RELEASE_ASSERT(m_count.fetch_sub(1, std::memory_order_acq_rel)); }

private:
    mutable std::atomic<uint32_t> m_count { 0 };
};

class AbstractCanMakeCheckedPtr {
public:
    virtual uint32_t checkedPtrCount() const = 0;
    virtual void incrementCheckedPtrCount() const = 0;
    virtual void decrementCheckedPtrCount() const = 0;

protected:
    virtual ~AbstractCanMakeCheckedPtr() = default;
};

// Does not own the object, and crashes if it outlives it.
template<typename T>
class CheckedPtr {
    WTF_MAKE_TZONE_ALLOCATED(CheckedPtr);

public:
    CheckedPtr() = default;
    CheckedPtr(std::nullptr_t) {}

    CheckedPtr(T* object)
        : m_ptr(object)
    {
        if (object)
            object->incrementCheckedPtrCount();
    }

    CheckedPtr(const CheckedPtr& other)
        : CheckedPtr(other.m_ptr)
    {
    }

    CheckedPtr(CheckedPtr&& other)
        : m_ptr(std::exchange(other.m_ptr, nullptr))
    {
    }

    ~CheckedPtr()
    {
        if (m_ptr)
            m_ptr->decrementCheckedPtrCount();
    }

    CheckedPtr& operator=(CheckedPtr other)
    {
        std::swap(m_ptr, other.m_ptr);
        return *this;
    }

    T* get() const LIFETIME_BOUND { return m_ptr; }
    T* operator->() const LIFETIME_BOUND
    {
        RELEASE_ASSERT(m_ptr);
        return m_ptr;
    }
    T& operator*() const LIFETIME_BOUND { return *operator->(); }

    explicit operator bool() const { return m_ptr; }
    bool operator!() const { return !m_ptr; }

private:
    T* m_ptr { nullptr };
};

}

// The memory of an object that is still pointed to is zeroed and never used again.
#define WTF_OVERRIDE_DELETE_FOR_CHECKED_PTR(Class)                        \
public:                                                                   \
    void operator delete(Class* object, std::destroying_delete_t)         \
    {                                                                     \
        object->Class::~Class();                                          \
        WTF::compilerFence();                                             \
        if (object->Class::checkedPtrCount()) [[unlikely]] {              \
            memset(static_cast<void*>(object), 0, sizeof(Class));         \
            return;                                                       \
        }                                                                 \
        Class::operator delete(static_cast<void*>(object));               \
    }                                                                     \
    using WTFDidOverrideDeleteForCheckedPtr = int;                        \
                                                                          \
private:                                                                  \
    WTF_REQUIRE_SEMICOLON

#define OVERRIDE_ABSTRACT_CAN_MAKE_CHECKEDPTR(Base)                                    \
    uint32_t checkedPtrCount() const final { return Base::checkedPtrCount(); }         \
    void incrementCheckedPtrCount() const final { Base::incrementCheckedPtrCount(); } \
    void decrementCheckedPtrCount() const final { Base::decrementCheckedPtrCount(); } \
    WTF_REQUIRE_SEMICOLON

using WTF::AbstractCanMakeCheckedPtr;
using WTF::CanMakeThreadSafeCheckedPtr;
using WTF::CheckedPtr;
