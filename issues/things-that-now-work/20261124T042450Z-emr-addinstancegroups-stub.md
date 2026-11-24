Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.op-jobflow-style-ops-reject-unknown-cluster
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr|AddInstanceGroups|NotImplemented
Issue-Commit: a26c0eedaf23f32bde14df1b90e2b70d0941c980

# Implement emr AddInstanceGroups: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `a26c0eedaf23f32bde14df1b90e2b70d0941c980`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AddInstanceGroups

## Description
`AddInstanceGroups` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-emr-elasticmapreduce/src/ops/add_instance_groups.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `AddInstanceGroups` call, so each one unblocks only when `AddInstanceGroups` works: `emr.op-jobflow-style-ops-reject-unknown-cluster`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-emr-elasticmapreduce/src/ops/add_instance_groups.rs`. Replaying the corpus, the emulator refused the `AddInstanceGroups` call: File "check_emr_op_jobflow_style_ops_reject_unknown_cluster.py", line 277, in run
    group_resp = _expect_success(lambda: emr.add_instance_groups(
                 ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
  File "check_emr_op_jobflow_style_ops_reject_unknown_cluster.py", line 189, in _expect_success
    raise AssertionError(
AssertionError: expected the positive control to succeed against a real, live cluster, got ClientError('An error occurred (NotImplemented) when calling the AddInstanceGroups operation: operation AddInstanceGroups is scaffolded but not implemented in this emulator')

## Steps to Reproduce
Boot the emr emulator
Call AddInstanceGroups and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
