//! What the client is to do where the back end leaves a hole.

use bun_engine_codegen::ir::{Origin, Token};

use crate::graph::CallKind;

/// What a patchpoint of the client's is for.
#[derive(Clone, Copy)]
pub(super) enum Site {
    /// Bytes that are written over with a jump once an assumption fails. The state is the one to leave with.
    InvalidationPoint,
    /// Keeps the stores to a new cell ahead of what lets the collector see it. The operand is the threshold.
    Publish,
    /// Builds the callee's frame, calls through the executable and restores the stack pointer.
    Call(CallTarget),
    /// Puts the callee's frame in the place of this one and jumps to it.
    TailCall(CallTarget),
    /// A call of a run-time function that may throw, collect or walk the stack. The back end emits the call;
    /// the client records where it returns to.
    Operation,
    /// A call of a run-time function that does none of these.
    Leaf,
}

/// What `Kind::Call` and its relatives need from the client.
#[derive(Clone, Copy)]
pub(super) struct CallTarget {
    pub(super) kind: CallKind,
    /// Including `this`.
    pub(super) argument_count: u32,
    /// Where the callee's frame starts, from the frame pointer.
    pub(super) frame_offset: i32,
    /// The entry and the code block are known, so neither the lookup nor the arity check is needed.
    pub(super) is_direct: bool,
}

/// What the lowering leaves for the client, by token.
#[derive(Default)]
pub(super) struct Sites {
    sites: Vec<Site>,
    /// By token: the instruction that the patchpoint stands for.
    origins: Vec<Origin>,
}

impl Sites {
    pub(super) fn clear(&mut self) {
        self.sites.clear();
        self.origins.clear();
    }

    pub(super) fn add(&mut self, site: Site, origin: Origin) -> Token {
        self.origins.push(origin);
        self.sites.push(site);
        Token(self.sites.len() as u32 - 1)
    }

    pub(super) fn at(&self, token: Token) -> Site {
        self.sites[token.0 as usize]
    }

    pub(super) fn origin_of(&self, token: Token) -> Origin {
        self.origins[token.0 as usize]
    }
}
