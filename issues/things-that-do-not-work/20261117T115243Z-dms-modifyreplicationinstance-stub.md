Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dms.modify-replication-instance-rejects-unknown-arn
Issue-Shape-Id: com.amazonaws.databasemigrationservice#AmazonDMSv20160101
Issue-Key: dms.modify-replication-instance-rejects-unknown-arn|run#24|AssertionError|assert (code, message) == _NOT_FOUND, (
Issue-Commit: 344b8f3034cadbab62678adde891e6a7e93d6f2a

# Implement dms ModifyReplicationInstance: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `344b8f3034cadbab62678adde891e6a7e93d6f2a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyReplicationInstance

## Description
`ModifyReplicationInstance` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/modify_replication_instance.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `ModifyReplicationInstance` call, so each one unblocks only when `ModifyReplicationInstance` works: `dms.modify-replication-instance-rejects-unknown-arn`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/modify_replication_instance.rs`. Replaying the corpus, the emulator refused the `ModifyReplicationInstance` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_dms_modify_replication_instance_rejects_unknown_arn.py", line 207, in run
    assert (code, message) == _NOT_FOUND, (
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: ModifyReplicationInstance on the unknown ARN arn:aws:dms:us-west-2:000000000000:rep:sim2real-behav-dms-bogus-7887821a949f returned ('NotImplemented', 'operation ModifyReplicationInstance is scaffolded but not implemented in this emulator'), expected ('ResourceNotFoundFault', 'Replication Instance not found: arn:aws:dms:us-west-2:000000000000:rep:sim2real-behav-dms-bogus-7887821a949f')

## Steps to Reproduce
Boot the dms emulator
Call ModifyReplicationInstance and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
