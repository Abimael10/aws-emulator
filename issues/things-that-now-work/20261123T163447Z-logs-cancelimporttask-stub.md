Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.create-import-task-lifecycle
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: logs.create-import-task-lifecycle|run#25|AssertionError|assert code == "ResourceNotFoundException", (
Issue-Commit: a5a3c6bd63789eb42e1283a455a4e127273ca01a

# Implement logs CancelImportTask: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `a5a3c6bd63789eb42e1283a455a4e127273ca01a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CancelImportTask

## Description
`CancelImportTask` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/ops/cancel_import_task.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `CancelImportTask` call, so each one unblocks only when `CancelImportTask` works: `logs.create-import-task-lifecycle`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-cloudwatchlogs-logs-20140328/src/ops/cancel_import_task.rs`. Replaying the corpus, the emulator refused the `CancelImportTask` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_logs_create_import_task_lifecycle.py", line 120, in run
    assert code == "ResourceNotFoundException", (
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected ResourceNotFoundException for a non-existent importId, got 'NotImplemented' 'operation CancelImportTask is scaffolded but not implemented in this emulator'

## Steps to Reproduce
Boot the logs emulator
Call CancelImportTask and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
