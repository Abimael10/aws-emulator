Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mediatailor.delete-channel-is-idempotent-and-removes-resource-for-dependent-ops
Issue-Shape-Id: com.amazonaws.mediatailor#MediaTailor
Issue-Key: mediatailor.delete-channel-is-idempotent-and-removes-resource-for-dependent-ops|run#20|AssertionError|assert code == "NotFoundException", f"ListAlerts on deleted channel's arn gave {code}: {message!r}"
Issue-Commit: 2cf3d1a87eac0dad062db8e481c5140d7209694f

# Implement mediatailor ListAlerts: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `2cf3d1a87eac0dad062db8e481c5140d7209694f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAlerts

## Description
`ListAlerts` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-mediatailor-mediatailor/src/ops/list_alerts.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `ListAlerts` call, so each one unblocks only when `ListAlerts` works: `mediatailor.delete-channel-is-idempotent-and-removes-resource-for-dependent-ops`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-mediatailor-mediatailor/src/ops/list_alerts.rs`. Replaying the corpus, the emulator refused the `ListAlerts` call: File "run.py", line 445, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_mediatailor_delete_channel_is_idempotent_and_removes_resource_for_dependent_ops.py", line 64, in run
    assert code == "NotFoundException", f"ListAlerts on deleted channel's arn gave {code}: {message!r}"
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: ListAlerts on deleted channel's arn gave NotImplemented: 'operation ListAlerts is scaffolded but not implemented in this emulator'

## Steps to Reproduce
Boot the mediatailor emulator
Call ListAlerts and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
