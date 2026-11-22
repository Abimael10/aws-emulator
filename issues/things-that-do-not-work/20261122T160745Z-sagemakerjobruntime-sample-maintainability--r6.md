Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: e6fc59220a21643213ef781c20ea38f623e54ae0

# The missing-store server-error branch is written out four times, once per handler, instead of behind the one helper state.rs documents the rule on

_Filed: 2026-11-22T16:07:45Z_
_Operation: Sample | Class: maintainability_

> _Note: this issue was identified against the code base at commit `e6fc59220a21643213ef781c20ea38f623e54ae0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::service_state` states the rule for a store lookup that answers nothing exactly once, on the helper itself: "Answer `None` with a modelled server error, never with a fresh engine." All four handlers of this crate then implement that rule themselves, in four byte-identical `let ... else` blocks that pick the same modelled error (`InternalServiceError`) and state the same sentence ("the AgenticRFT runtime emulator has no state store"). Nothing holds the four copies to each other: changing the sentence, or the error the rule is reported through, in one handler leaves the other three stating the old one, and no test fails (the `None` arm is unreachable from the suites — `harness::mounted_service` always mounts an `engine::InMemoryEngine`, which `service_state` downcasts successfully).

This is the one shared decision of this crate still written per handler. Every other one has already been hoisted into `state.rs` behind a per-operation error constructor, and `state.rs` states that as the crate's own pattern: `SamplingRefusal::into_modelled_error` says "The branch itself lives HERE, beside the flag that decides it, so a sampling operation hands this its two error constructors and names nothing but its own enum", and `serve_rollout_trajectory` / `serve_sampling_turn` do the same for resolution and capture. The same shape fits here: one helper in `state.rs` that owns the sentence and the branch and is handed the calling operation's `InternalServiceError` constructor, so each handler names only its own enum.

The fix is response-preserving: all four copies already build the identical message and the identical modelled error, so hoisting them changes no status, no error code and no `Message` on any path, and leaves every existing test untouched.

## Evidence

The rule, stated ONCE on the helper — services/service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice/src/state.rs:584-586:
```
/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
```

Copy 1 — src/ops/sample.rs:171-176:
```
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(SampleErrorModeled::InternalServiceError(InternalServiceError {
                message: "the AgenticRFT runtime emulator has no state store".to_string(),
            })
            .into());
        };
```

Copy 2 — src/ops/sample_with_response_stream.rs:172-179:
```
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(SampleWithResponseStreamErrorModeled::InternalServiceError(
                InternalServiceError {
                    message: "the AgenticRFT runtime emulator has no state store".to_string(),
                },
            )
            .into());
        };
```

Copy 3 — src/ops/complete_rollout.rs:238-246:
```
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(CompleteRolloutErrorModeled::InternalServiceError(
                InternalServiceError {
                    message: "the AgenticRFT runtime emulator has no state store".to_string(),
                },
            )
            .into());
        };
```

Copy 4 — src/ops/update_reward.rs:214-221:
```
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(
                UpdateRewardErrorModeled::InternalServiceError(InternalServiceError {
                    message: "the AgenticRFT runtime emulator has no state store".to_string(),
                })
                .into(),
            );
        };
```

The crate's OWN stated pattern for exactly this kind of per-handler branch — src/state.rs:761-781:
```
    /// Report this refusal as one of the calling operation's OWN modelled
    /// errors: a quota through `quota`, anything else through `invalid`, each
    /// handed the sentence to report.
    ///
    /// The branch itself lives HERE, beside the flag that decides it, so a
    /// sampling operation hands this its two error constructors and names
    /// nothing but its own enum
```
and src/state.rs:668-676 for the rollout pair ("`CompleteRollout` and `UpdateReward` reach the SAME state through this, so neither can drift from the other ... What is left to each of them is what genuinely differs: the modelled error constructor the sentence is reported through").

Test coverage: no suite reaches the `None` arm — every suite mounts through `tests/all/harness.rs` `mounted_service()`, which builds an `engine::InMemoryEngine`, the exact type `service_state` downcasts to (src/state.rs:587-593), so the four copies are uncovered and a divergence between them would go unnoticed. The behaviour that MUST stay unchanged is pinned by tests/all/trajectory_lifecycle.rs, tests/all/sampling_path_is_shared.rs and tests/all/rollout_resolution_is_one_path.rs, none of which observe this branch.

## Steps to Reproduce

1. grep -n 'has no state store' services/service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice/src/ops/ -r — four hits, one per handler (sample.rs:173, sample_with_response_stream.rs:175, complete_rollout.rs:241, update_reward.rs:217), each inside an identical `let Some(state) = crate::state::service_state(&*self.engine) else { ... }` block.
2. Read src/state.rs:584-586: the rule the four blocks implement is stated once, on `service_state` itself ("Answer `None` with a modelled server error, never with a fresh engine").
3. Read src/state.rs:761-781 and 668-676: the crate already hoists every other shared decision into `state.rs` and hands it the calling operation's error constructor, naming that as the reason the copies cannot drift.
4. Edit the sentence (or the modelled error) in ONE of the four handlers and run `cargo test -p service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice`: the suites still pass, because no test mounts the service on an engine `service_state` answers `None` for, so the three stale copies are never observed.
