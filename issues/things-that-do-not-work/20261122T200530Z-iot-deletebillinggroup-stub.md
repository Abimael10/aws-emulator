Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.billing-group-stale-version-delete-fails
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.billing-group-stale-version-delete-fails|run#7|AssertionError|assert code == ("VersionConflictException", f"The version for billing group {group} does not match the expected version."), (
Issue-Commit: d8571c81dd15a8f22d7c2603104b67b8a6bcc578

# Implement iot DeleteBillingGroup: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `d8571c81dd15a8f22d7c2603104b67b8a6bcc578`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteBillingGroup

## Description
`DeleteBillingGroup` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-iot-awsiotservice/src/ops/delete_billing_group.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `DeleteBillingGroup` call, so each one unblocks only when `DeleteBillingGroup` works: `iot.billing-group-stale-version-delete-fails`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-iot-awsiotservice/src/ops/delete_billing_group.rs`. Replaying the corpus, the emulator refused the `DeleteBillingGroup` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_iot_billing_group_stale_version_delete_fails.py", line 41, in run
    assert code == ("VersionConflictException", f"The version for billing group {group} does not match the expected version."), (
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected VersionConflictException for the stale expectedVersion, got ('NotImplemented', 'operation DeleteBillingGroup is scaffolded but not implemented in this emulator')

## Steps to Reproduce
Boot the iot emulator
Call DeleteBillingGroup and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
