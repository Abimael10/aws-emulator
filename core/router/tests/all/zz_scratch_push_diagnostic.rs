// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite, deliberately: it held a throwaway probe that drove the whole assembled
//! router through AWS Config's change-triggered custom-rule push path while that path was
//! being traced. What it established is now pinned by
//! `all/config_change_triggered_custom_rule_push_invocation.rs`, which asserts the rule
//! rather than dumping state.
//!
//! The file is still here rather than deleted because this fixer's tool set can write a file
//! but not remove one — the same reason `all/zz_probe.rs` and `all/zz_probe_whole_emulator.rs`
//! are still there. The next change to touch this directory should delete all three.
