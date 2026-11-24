Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: deadline.delete-worker-before-stopped-rejected
Issue-Shape-Id: com.amazonaws.deadline#Deadline
Issue-Key: deadline.delete-worker-before-stopped-rejected|run#18|AssertionError|assert error.get("Message") == (
Issue-Commit: d9cbdb200d86ce0965ad4ff8d3a49ac488656502

# deadline DeleteWorker on a STARTED worker returns a locally-invented ConflictException message instead of AWS's status-conflict wording

> _Note: this issue was identified against the code base at commit `d9cbdb200d86ce0965ad4ff8d3a49ac488656502`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteWorker

## Description
One net-new root cause.

Cause 1 of 1: DeleteWorker's refusal of a worker that is not in a deletable status carries an emulator-authored message instead of the message AWS words for a Deadline Cloud status conflict. Failing check: deadline.delete-worker-before-stopped-rejected.

AWS: Smithy (com.amazonaws.deadline) models DeleteWorker with ConflictException (errorKind client, httpError 409), whose required members are message, reason (ConflictExceptionReason enum: CONFLICT_EXCEPTION | CONCURRENT_MODIFICATION | RESOURCE_ALREADY_EXISTS | RESOURCE_IN_USE | STATUS_CONFLICT), resourceId and resourceType. The model does not spell the message text, so the message is recorded behavior only: real AWS refuses DeleteWorker on a worker whose status is not one of the valid statuses with HTTP 409, Code ConflictException and the message

  "Request could not be completed. Status <status> of resource <workerId> does not permit the operation. The valid statuses are [CREATED, DELETED, STOPPED, NOT_RESPONDING]"

Its FIXED text is: "Request could not be completed. Status ", " of resource ", " does not permit the operation. The valid statuses are [CREATED, DELETED, STOPPED, NOT_RESPONDING]". Its VARIABLE parts are two, both read from the request and from the emulator's state: <status> is the stored worker's current status, and <workerId> is the workerId from the request path. The bracketed status list is fixed text for this operation (it is AWS's own valid-status list for DeleteWorker, which is why a never-started CREATED worker deletes cleanly, as the same check verifies).

Emulator: the refusal is classified correctly (ConflictException) but the message is

  "Worker is in STARTED status and cannot be deleted."

This text appears nowhere in AWS: it is not AWS's sentence structure, it omits the resource id, and it omits the valid-status list, so it also implies (wrongly) that only STOPPED is deletable.

Why: a client that keys retry/remediation on the refusal text — or that parses the valid-status list out of the message to decide which transition to drive the worker through before retrying the delete — gets nothing to parse and no resource id to attribute the conflict to. Against the emulator such a client silently takes the wrong branch (or loses the id when several workers are being torn down concurrently) while the same code works against AWS. Operators diffing emulator logs against AWS logs see a refusal that is not AWS's.

Fix: the deadline ConflictException message builder for a worker status conflict — `crate::state::worker_not_stopped`, the helper the DeleteWorker handler calls for the non-deletable branch. It must emit AWS's status-conflict sentence, interpolating the stored worker's status and the request's workerId, and should also carry the modeled reason (STATUS_CONFLICT), resourceId and resourceType members that AWS populates on this error. The 409 status and the ConflictException code are already right; only the message (and the error's required detail members) are wrong.

Traces: deadline.delete-worker-before-stopped-rejected. Handler: services/service-com-amazonaws-deadline-deadline/src/ops/delete_worker.rs lines 181-187 (the `!worker.deletable()` branch calling `crate::state::worker_not_stopped(&input.worker_id, &status)`); the message itself is built in services/service-com-amazonaws-deadline-deadline/src/state.rs.

## Evidence
Failing check: deadline.delete-worker-before-stopped-rejected (verdict: mismatch).

Assertion that failed (check source, lines 1596-1601):
    assert error.get("Message") == (
        f"Request could not be completed. Status STARTED of resource {worker_id} does not permit "
        "the operation. The valid statuses are [CREATED, DELETED, STOPPED, NOT_RESPONDING]"
    )

Replay output:
    AssertionError: expected the exact ConflictException message, got 'Worker is in STARTED status and cannot be deleted.'

Expected (AWS, confirmed on real AWS per the check's docstring):
  HTTP 409, Error.Code = "ConflictException"
  Error.Message = "Request could not be completed. Status <status> of resource <workerId> does not permit the operation. The valid statuses are [CREATED, DELETED, STOPPED, NOT_RESPONDING]"
  (<status> = the worker's stored status, <workerId> = the workerId from the request)

Actual (emulator):
  HTTP 409, Error.Code = "ConflictException"  (correct)
  Error.Message = "Worker is in STARTED status and cannot be deleted."  (diverges: wrong fixed text, no resource id, no valid-status list)

Everything else in the check passed: DeleteWorker on a never-started CREATED worker succeeded, the subsequent GetWorker returned ResourceNotFoundException with AWS's exact message, and the STARTED worker was left intact.

Smithy citation: com.amazonaws.deadline#ConflictException — errorKind client, httpError 409, required members message/reason/resourceId/resourceType; com.amazonaws.deadline#ConflictExceptionReason values [CONFLICT_EXCEPTION, CONCURRENT_MODIFICATION, RESOURCE_ALREADY_EXISTS, RESOURCE_IN_USE, STATUS_CONFLICT]. DeleteWorker declares ConflictException among its errors.

## Steps to Reproduce
1. CreateFarm; CreateFleet (customerManaged, NO_SCALING) in that farm and wait until its status leaves CREATE_IN_PROGRESS.
2. CreateWorker in that farm/fleet.
3. UpdateWorker(farmId, fleetId, workerId, status="STARTED", capabilities=<reserved attr.worker.*/amount.worker.* set>) and poll GetWorker until status == "STARTED".
4. Call DeleteWorker(farmId, fleetId, workerId) on that STARTED worker.
5. Inspect the error: the code is ConflictException (correct) but Error.Message is "Worker is in STARTED status and cannot be deleted." instead of "Request could not be completed. Status STARTED of resource <workerId> does not permit the operation. The valid statuses are [CREATED, DELETED, STOPPED, NOT_RESPONDING]".

Replay shortcut: re-run the behavioral check deadline.delete-worker-before-stopped-rejected.
