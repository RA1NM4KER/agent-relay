#![forbid(unsafe_code)]
#![allow(dead_code)]
// Compile the exact insertion modules without the complete native dependency graph.
extern crate self as codex_core;
#[path = "host_authority.rs"]
mod host_authority;
#[path = "core_authority.rs"]
pub mod relay_authority;
