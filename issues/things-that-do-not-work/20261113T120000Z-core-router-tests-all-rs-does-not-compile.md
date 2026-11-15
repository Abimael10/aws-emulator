Issue-Severity: HIGH
Issue-Corpus: build
Issue-Corpus-Kind: build
Issue-Trace-Ids: core-router-tests-all-rs-e0753
Issue-Key: core/router/tests/all.rs|E0753|inner doc comments like this (starting with `//!` or `/*!`) can only appear before items

# The router's cross-service test target does not compile: `core/router/tests/all.rs` carries fragments of its own header, re-typed with `//!`, past its first item

## Operation
None — this is the build, not a service operation.

## Description
`core/router/tests/all.rs` is the declaration root of the router's one cross-service test
target. Past its declaration list the file carries thousands of lines of prose: copies and
fragments of its own header, re-typed with `//!` by successive landings. An inner doc comment
is legal only BEFORE the first item, so every one of them is `E0753` — currently 662 distinct
errors, the first at line 1470 — and the target compiles no suite at all.

Because the merge gate's `emulator_boot_serve` reviewer runs `cargo nextest run` over the
merged tree, this fails EVERY fix's submit, whatever the fix changes, so nothing can land
while it stands. It is also self-sealing: the project's file-length rule refuses a fix that
edits a file longer than 5000 lines, and this file is about 7000, so the only edit the gate
will accept is one that ALSO brings it under that ceiling.

## Fix
Rewrite `core/router/tests/all.rs` as the canonical list its own guard suites specify
(`all/all_rs_is_a_canonical_declaration_list.rs`): the module doc, then one
`#[path = "all/<name>.rs"]` / `mod <name>;` pair per file in `core/router/tests/all/`,
sorted strictly by name, and no other line. That deletes the ~5900-line tail in the same
change, which is what brings the file under the length ceiling, and it must keep every
declaration — including the handful of real pairs buried in the tail, which the build-time
layout check fails the build for if they are dropped:

* `all_rs_is_a_canonical_declaration_list`
* `events_lambda_target_permission_granted_after_the_rule`
* `zz_glue_job_source_permission_probe`

The notes the tail had collected already have a home at `all/appended_notes.rs`.

After the rewrite, the three guard suites keep the shape: `all_rs_is_a_canonical_declaration_list`
(pairs only, sorted), `all_rs_declares_each_suite_once` (one declaration per suite file) and
`all_rs_inner_doc_comments_precede_the_first_item` (no `//!` past the header) — for this root
and for every service crate's `tests/all.rs`.

## Evidence
```
error[E0753]: expected outer doc comment
    --> core/router/tests/all.rs:1470:1
     |
1470 | //! the declaration into `tests/all/`.
     | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
     |
     = note: inner doc comments like this (starting with `//!` or `/*!`) can only appear
       before items
... 661 more distinct error(s)
```
Reported by the merge gate's `emulator_boot_serve` reviewer on four consecutive submits of an
unrelated, otherwise-approved CloudFormation fix, at the same line each time.

## Steps to Reproduce
1. `cargo check --workspace --all-targets` on a clean checkout of mainline.
2. Expected: it compiles. Actual: `E0753` at `core/router/tests/all.rs:1470` and 661 more.
3. `grep -n '^//!' core/router/tests/all.rs` — every hit past the header block is one of them.
