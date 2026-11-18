//! The speculative optimizing compiler for bytecode.

#![allow(dead_code)]
#![deny(unsafe_code)]

mod builder;
mod graph;
mod lower;
mod passes;
#[allow(unsafe_code)]
mod runtime;
