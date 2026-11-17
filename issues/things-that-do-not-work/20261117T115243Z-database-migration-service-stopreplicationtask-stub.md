Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dms__ec2__iam__s3.stop-replication-task-rejected-for-a-ready-or-stopped-task
Issue-Key: dms__ec2__iam__s3.stop-replication-task-rejected-for-a-ready-or-stopped-task|run#47|AssertionError|assert got == not_running, f"stopping a ready task gave {got!r}, expected {not_running!r}"
Issue-Commit: 344b8f3034cadbab62678adde891e6a7e93d6f2a

# Implement database-migration-service StopReplicationTask: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `344b8f3034cadbab62678adde891e6a7e93d6f2a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopReplicationTask

## Description
`StopReplicationTask` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/stop_replication_task.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `StopReplicationTask` call, so each one unblocks only when `StopReplicationTask` works: `dms__ec2__iam__s3.stop-replication-task-rejected-for-a-ready-or-stopped-task`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/stop_replication_task.rs`. Replaying the corpus, the emulator refused the `StopReplicationTask` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_dms__ec2__iam__s3_stop_replication_task_rejected_for_a_ready_or_stopped_task.py", line 513, in run
    assert got == not_running, f"stopping a ready task gave {got!r}, expected {not_running!r}"
           ^^^^^^^^^^^^^^^^^^
AssertionError: stopping a ready task gave ('NotImplemented', 'operation StopReplicationTask is scaffolded but not implemented in this emulator'), expected ('InvalidResourceStateFault', 'Replication Task: arn:aws:dms:us-west-2:000000000000:task:ATOB3RDBTPO4WK2WXN8J6DXAY7 is currently not running.')

## Steps to Reproduce
Boot the database-migration-service emulator
Call StopReplicationTask and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
