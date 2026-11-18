#pragma once

// The frames on the stack, and stack traces.

#include "engine/api/Code.h"

namespace JSC {

class StackFrame {
public:
    StackFrame() = default;

    JSCell* callee() const { return m_isWasm ? nullptr : m_callee; }
    CodeBlock* codeBlock() const { return m_codeBlock; }
    bool hasLineAndColumnInfo() const { return m_codeBlock; }
    bool isAsyncFrame() const { return m_isAsync; }
    bool isWasmFrame() const { return m_isWasm; }

private:
    static void checkLayout();

    JSCell* m_callee { nullptr };
    CodeBlock* m_codeBlock { nullptr };
    uint32_t m_bytecodeOffset { 0 };
    bool m_isAsync { false };
    bool m_isWasm { false };
};

} // namespace JSC
