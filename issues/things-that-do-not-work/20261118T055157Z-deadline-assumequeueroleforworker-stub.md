Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: deadline.assume-queue-role-for-worker-signs-as-the-queue-role-only-while-the-worker-works-on-that-queue
Issue-Shape-Id: com.amazonaws.deadline#Deadline
Issue-Key: deadline.assume-queue-role-for-worker-signs-as-the-queue-role-only-while-the-worker-works-on-that-queue|run#13|AssertionError|assert early["Code"] == "ConflictException", f"expected ConflictException before any session, got {early!r}"
Issue-Commit: f589dd7fea9a9534b25c5489c0d2c567188188ad

# Implement deadline AssumeQueueRoleForWorker: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `f589dd7fea9a9534b25c5489c0d2c567188188ad`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssumeQueueRoleForWorker

## Description
`AssumeQueueRoleForWorker` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-deadline-deadline/src/ops/assume_queue_role_for_worker.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `AssumeQueueRoleForWorker` call, so each one unblocks only when `AssumeQueueRoleForWorker` works: `deadline.assume-queue-role-for-worker-signs-as-the-queue-role-only-while-the-worker-works-on-that-queue`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-deadline-deadline/src/ops/assume_queue_role_for_worker.rs`. Replaying the corpus, the emulator refused the `AssumeQueueRoleForWorker` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_deadline_assume_queue_role_for_worker_signs_as_the_queue_role_only_while_the_worker_works_on_that_queue.py", line 46, in run
    assert early["Code"] == "ConflictException", f"expected ConflictException before any session, got {early!r}"
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: expected ConflictException before any session, got {'Message': 'operation AssumeQueueRoleForWorker is scaffolded but not implemented in this emulator', 'Code': 'NotImplemented'}

## Steps to Reproduce
Boot the deadline emulator
Call AssumeQueueRoleForWorker and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
