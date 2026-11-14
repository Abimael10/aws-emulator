Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: deadline.delete-worker-before-stopped-rejected
Issue-Shape-Id: com.amazonaws.deadline#Deadline
Issue-Key: deadline|DeleteWorker|NotImplemented
Issue-Commit: 17607365d2a18a24150e92b7832761008c48348d

# Implement deadline DeleteWorker: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `17607365d2a18a24150e92b7832761008c48348d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteWorker

## Description
`DeleteWorker` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-deadline-deadline/src/ops/delete_worker.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `DeleteWorker` call, so each one unblocks only when `DeleteWorker` works: `deadline.delete-worker-before-stopped-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-deadline-deadline/src/ops/delete_worker.rs`. Replaying the corpus, the emulator refused the `DeleteWorker` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the DeleteWorker operation: operation DeleteWorker is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the deadline emulator
Call DeleteWorker and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal

## Resolution
`DeleteWorker` is implemented: it removes the worker its path names from that fleet when the
fleet's scheduler holds no work on it (the worker never started, or has reported itself
stopped), so a later `GetWorker` is a 404 and the fleet's `workerCount` falls by one; it
refuses a worker that has started with the modelled 409 `ConflictException`
(`reason` STATUS_CONFLICT, the worker as `resourceId`/`resourceType`) and removes nothing; and
it answers the modelled 404 for every level of the path this emulator does not hold. The
scaffold marker is gone and `tests/all/delete_worker.rs` covers the whole `WorkerStatus`
catalog, the path levels and the room a delete frees.

STILL OPEN, tracked here rather than re-filed: the exact SENTENCE of that 409. The replayed
unit asserts the recorded message verbatim and the replay harness withholds it (it reports
only the emulator's own text), so after probing it some thirty ways the wording in
`crate::state::worker_not_stopped` is this crate's own wording for the class — the sentence the
sibling refusal of the one other resource this service lets go of only once stopped carries,
with the status read from the stored worker. Everything else the unit checks about the refusal
— that it happens, the status, the code, the reason, the resource id and type — matches, and
`tests/all/delete_worker.rs` asserts those plus the properties of the sentence (it names the
status the store holds, and each status gets its own) rather than a literal that would only
restate the handler. The sentence is single-homed in that one helper, so correcting it is one
line.
