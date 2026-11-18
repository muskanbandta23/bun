#pragma once

#include "engine/support/Vector.h"

#include <bit>

namespace WTF {

namespace Detail {

template<typename T, size_t inlineCapacity>
struct DequeStorage {
    T* inlineBuffer() { return reinterpret_cast<T*>(m_inlineBuffer); }

    alignas(T) std::byte m_inlineBuffer[inlineCapacity * sizeof(T)];
};

template<typename T>
struct DequeStorage<T, 0> {
    T* inlineBuffer() { return nullptr; }
};

template<typename T>
class DequeIterator {
public:
    using iterator_category = std::forward_iterator_tag;
    using value_type = std::remove_const_t<T>;
    using difference_type = ptrdiff_t;
    using pointer = T*;
    using reference = T&;

    DequeIterator() = default;
    DequeIterator(T* buffer, unsigned mask, unsigned position)
        : m_buffer(buffer)
        , m_mask(mask)
        , m_position(position)
    {
    }

    T& operator*() const { return m_buffer[m_position & m_mask]; }
    T* operator->() const { return std::addressof(**this); }

    DequeIterator& operator++()
    {
        ++m_position;
        return *this;
    }

    DequeIterator operator++(int) { return { m_buffer, m_mask, m_position++ }; }

    friend bool operator==(const DequeIterator& a, const DequeIterator& b) { return a.m_position == b.m_position; }

private:
    T* m_buffer { nullptr };
    unsigned m_mask { 0 };
    unsigned m_position { 0 };
};

}

// A ring whose capacity is a power of two. Positions count on past the capacity, and their low bits are the slot.
template<typename T, size_t inlineCapacity>
class Deque final : private Detail::DequeStorage<T, inlineCapacity> {
    WTF_MAKE_TZONE_ALLOCATED(Deque);

public:
    using iterator = Detail::DequeIterator<T>;
    using const_iterator = Detail::DequeIterator<const T>;

    Deque() {}
    Deque(Deque&& other) { adopt(other); }

    ~Deque()
    {
        destroyElements();
        freeBuffer();
    }

    Deque& operator=(Deque&& other)
    {
        if (this != &other) {
            Deque old(WTF::move(*this));
            adopt(other);
        }
        return *this;
    }

    size_t size() const { return m_end - m_start; }
    bool isEmpty() const { return m_start == m_end; }

    iterator begin() LIFETIME_BOUND { return { m_buffer, m_capacity - 1, m_start }; }
    iterator end() LIFETIME_BOUND { return { m_buffer, m_capacity - 1, m_end }; }
    const_iterator begin() const LIFETIME_BOUND { return { m_buffer, m_capacity - 1, m_start }; }
    const_iterator end() const LIFETIME_BOUND { return { m_buffer, m_capacity - 1, m_end }; }

    T& first() LIFETIME_BOUND { return *firstSlot(); }
    const T& first() const LIFETIME_BOUND { return *firstSlot(); }
    T& last() LIFETIME_BOUND { return *lastSlot(); }
    const T& last() const LIFETIME_BOUND { return *lastSlot(); }

    void append(T&& value) { append<T>(WTF::move(value)); }

    template<typename U>
    void append(U&& value)
    {
        if (size() == m_capacity) [[unlikely]]
            return expandAndAdd(size(), std::forward<U>(value));
        new (NotNull, slot(m_end)) T(std::forward<U>(value));
        ++m_end;
    }

    template<typename U>
    void prepend(U&& value)
    {
        if (size() == m_capacity) [[unlikely]]
            return expandAndAdd(-1, std::forward<U>(value));
        new (NotNull, slot(m_start - 1)) T(std::forward<U>(value));
        --m_start;
    }

    void removeFirst()
    {
        T* element = firstSlot();
        ++m_start;
        Detail::RemovedElement<T> removed { element };
    }

    void removeLast()
    {
        T* element = lastSlot();
        --m_end;
        Detail::RemovedElement<T> removed { element };
    }

    T takeFirst()
    {
        T result = WTF::move(first());
        removeFirst();
        return result;
    }

    T takeLast()
    {
        T result = WTF::move(last());
        removeLast();
        return result;
    }

    // Frees the buffer too.
    void clear() { Deque old(WTF::move(*this)); }

private:
    static_assert(!inlineCapacity || std::has_single_bit(inlineCapacity));
    static constexpr size_t minCapacity = 16;

    using Detail::DequeStorage<T, inlineCapacity>::inlineBuffer;

    T* slot(unsigned position) const { return m_buffer + (position & (m_capacity - 1)); }

    T* firstSlot() const
    {
        RELEASE_ASSERT(!isEmpty());
        return slot(m_start);
    }

    T* lastSlot() const
    {
        RELEASE_ASSERT(!isEmpty());
        return slot(m_end - 1);
    }

    void destroyElements()
    {
        if constexpr (!std::is_trivially_destructible_v<T>) {
            for (unsigned position = m_start; position != m_end; ++position)
                slot(position)->~T();
        }
    }

    void freeBuffer()
    {
        if (m_buffer != inlineBuffer())
            fastFree(m_buffer);
    }

    // To the start of a buffer.
    void relocateElements(T* destination)
    {
        if (isEmpty())
            return;
        T* first = slot(m_start);
        size_t firstRun = std::min<size_t>(size(), m_buffer + m_capacity - first);
        Detail::relocate(first, firstRun, destination);
        Detail::relocate(m_buffer, size() - firstRun, destination + firstRun);
    }

    // This deque is as new, and `other` will be.
    void adopt(Deque& other)
    {
        if (inlineCapacity && other.m_buffer == other.inlineBuffer()) {
            other.relocateElements(m_buffer);
            m_end = other.size();
        } else {
            m_buffer = std::exchange(other.m_buffer, other.inlineBuffer());
            m_capacity = std::exchange(other.m_capacity, inlineCapacity);
            m_start = other.m_start;
            m_end = other.m_end;
        }
        other.m_start = 0;
        other.m_end = 0;
    }

    // `offset` is from the first element. The value may be an element, so the old buffer stays until it has been used.
    template<typename U>
    NEVER_INLINE void expandAndAdd(unsigned offset, U&& value)
    {
        size_t newCapacity = std::max(minCapacity, size_t { m_capacity } * 2);
        RELEASE_ASSERT(isValidCapacityForVector<T>(newCapacity));
        auto* newBuffer = static_cast<T*>(fastMalloc(newCapacity * sizeof(T)));
        new (NotNull, newBuffer + (offset & (newCapacity - 1))) T(std::forward<U>(value));
        relocateElements(newBuffer);
        freeBuffer();
        unsigned oldSize = size();
        m_buffer = newBuffer;
        m_capacity = newCapacity;
        m_start = offset == oldSize ? 0 : offset;
        m_end = m_start + oldSize + 1;
    }

    T* m_buffer { inlineBuffer() };
    unsigned m_capacity { inlineCapacity };
    unsigned m_start { 0 };
    unsigned m_end { 0 };
};

}

using WTF::Deque;
