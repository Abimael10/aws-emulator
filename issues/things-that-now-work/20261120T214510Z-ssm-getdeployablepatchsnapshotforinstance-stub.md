Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.get-deployable-patch-snapshot-unsupported-os-rejected
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.get-deployable-patch-snapshot-unsupported-os-rejected|run#7|AssertionError|assert error["Code"] == "AccessDeniedException", f"expected AccessDeniedException, got {error!r}"
Issue-Commit: d480a9c819aca039333e6ccd75fa51254e2ac722

# Implement ssm GetDeployablePatchSnapshotForInstance: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `d480a9c819aca039333e6ccd75fa51254e2ac722`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDeployablePatchSnapshotForInstance

## Description
`GetDeployablePatchSnapshotForInstance` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-ssm-amazonssm/src/ops/get_deployable_patch_snapshot_for_instance.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `GetDeployablePatchSnapshotForInstance` call, so each one unblocks only when `GetDeployablePatchSnapshotForInstance` works: `ssm.get-deployable-patch-snapshot-unsupported-os-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-ssm-amazonssm/src/ops/get_deployable_patch_snapshot_for_instance.rs`. Replaying the corpus, the emulator refused the `GetDeployablePatchSnapshotForInstance` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_ssm_get_deployable_patch_snapshot_unsupported_os_rejected.py", line 35, in run
    assert error["Code"] == "AccessDeniedException", f"expected AccessDeniedException, got {error!r}"
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected AccessDeniedException, got {'Message': 'operation GetDeployablePatchSnapshotForInstance is scaffolded but not implemented in this emulator', 'Code': 'NotImplemented'}

## Steps to Reproduce
Boot the ssm emulator
Call GetDeployablePatchSnapshotForInstance and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
