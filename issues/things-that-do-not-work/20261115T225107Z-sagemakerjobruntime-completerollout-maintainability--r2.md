Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: a8b5efd4a7d15f36db8efaf56cc150cd0dc8890c

# The operation-struct doc comment is attached to the wrong item in three of the four handlers

_Filed: 2026-11-15T22:51:07Z_
_Operation: CompleteRollout | Class: maintainability_

> _Note: this issue was identified against the code base at commit `a8b5efd4a7d15f36db8efaf56cc150cd0dc8890c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Each handler in this crate carries the scaffold's description of the operation STRUCT ("The `<Op>` operation. / Owns the shared engine (the constructor seam) and implements [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng` ..."). In `src/ops/sample.rs` that comment sits immediately above `pub struct Sample<Eng>`, which is what it describes. In the other three handlers — `complete_rollout.rs`, `update_reward.rs` and `sample_with_response_stream.rs` — hand-written items (a constant and two error constructors) were inserted BETWEEN that comment and the struct, so the comment is now a doc comment ON those items: a `const`/`fn` is documented as "the operation" that "owns the shared engine", "implements `smithy_server_runtime::Operation`" and is "generic over the engine type `Eng`", none of which is true of it, and the three operation structs (`CompleteRollout`, `UpdateReward`, `SampleWithResponseStream`) are left with no documentation at all. The two statements of the same thing in this crate therefore disagree: one handler documents the struct, three document an unrelated item with the struct's text, and the next hand-written helper added above a struct will inherit the same mistake. This is a documentation defect only: no response, no status, no stored state and no test observes it, so the fix (moving the misplaced lines back onto the struct they describe) leaves every response and every existing test unchanged.

## Evidence

Correct placement, services/service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice/src/ops/sample.rs:128-134:
```
/// The `Sample` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct Sample<Eng> {
```

Misattached, src/ops/complete_rollout.rs:116-131 (the text now documents a constant, and `pub struct CompleteRollout<Eng>` at src/ops/complete_rollout.rs:203 carries no doc):
```
/// The `CompleteRollout` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
/// The status an absent `Status` member means.
...
const DEFAULT_COMPLETION_STATUS: crate::state::CompletionStatus =
    crate::state::CompletionStatus::Ready;
```

Misattached, src/ops/update_reward.rs:115-122 (now documents `fn conflict`; `pub struct UpdateReward<Eng>` at src/ops/update_reward.rs:178 carries no doc):
```
/// The `UpdateReward` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
/// This operation's own modelled 409 `ConflictException`.
fn conflict(message: String) -> UpdateRewardError {
```

Misattached, src/ops/sample_with_response_stream.rs:116-123 (now documents `fn validation`; `pub struct SampleWithResponseStream<Eng>` at src/ops/sample_with_response_stream.rs:136 carries no doc):
```
/// The `SampleWithResponseStream` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
/// This operation's own modelled 400 `ValidationException`.
fn validation(message: String) -> SampleWithResponseStreamError {
```

Test coverage, disclosed: no suite in `tests/all/` and no `#[cfg(test)]` module in these files asserts anything about doc placement — the wire suites (`tests/all/trajectory_lifecycle.rs`, `tests/all/modelled_rejections.rs`, `tests/all/reward_resolution_precedes_its_own_checks.rs`, `tests/all/rollout_operations_resolve_the_job.rs`) exercise only responses and stored state, so moving the comment back onto its struct cannot change any of them.

## Steps to Reproduce

1. Open services/service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice/src/ops/sample.rs and read lines 128-134: the operation-struct description sits directly above `pub struct Sample<Eng>`.
2. Open src/ops/complete_rollout.rs and read lines 116-131: the same description is followed, with no blank line, by `/// The status an absent `Status` member means.` and `const DEFAULT_COMPLETION_STATUS`, so it documents the constant.
3. Read src/ops/complete_rollout.rs:203 (`pub struct CompleteRollout<Eng> {`): the struct has no doc comment.
4. Repeat for src/ops/update_reward.rs:115-122 vs :178 and src/ops/sample_with_response_stream.rs:116-123 vs :136 — the same description documents `fn conflict` and `fn validation` respectively.
5. Run `cargo doc -p service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice --no-deps` and observe `DEFAULT_COMPLETION_STATUS`, `conflict` and `validation` described as "The <Op> operation ... owns the shared engine ... implements Operation", while the three operation structs are undocumented.
