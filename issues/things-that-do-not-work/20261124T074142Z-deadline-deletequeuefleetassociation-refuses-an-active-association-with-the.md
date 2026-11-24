Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: deadline.queue-fleet-association-delete-before-stopped-rejected
Issue-Shape-Id: com.amazonaws.deadline#Deadline
Issue-Key: deadline.queue-fleet-association-delete-before-stopped-rejected|run#13|AssertionError|assert error.get("Message") == "QueueFleetAssociation is in ACTIVE status and cannot be deleted.", (
Issue-Commit: 960085fede7475f864d055ec106450d0cb96a6ce

# deadline DeleteQueueFleetAssociation refuses an ACTIVE association with the generic status-conflict sentence instead of AWS's "QueueFleetAssociation is in ACTIVE status and cannot be deleted."

> _Note: this issue was identified against the code base at commit `960085fede7475f864d055ec106450d0cb96a6ce`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteQueueFleetAssociation

## Description
ONE root cause, grounded in behavioral check deadline.queue-fleet-association-delete-before-stopped-rejected.

Cause 1 of 1 — the ConflictException (409) that DeleteQueueFleetAssociation answers for an association that has not stopped carries this emulator's own generic status-conflict wording rather than the sentence AWS words for this refusal.

AWS: Smithy: DeleteQueueFleetAssociation is DELETE /2023-10-12/farms/{farmId}/queue-fleet-associations/{queueId}/{fleetId} (idempotent, success 200, empty output) and models ConflictException with httpError 409; ConflictException's required members are message, reason (ConflictExceptionReason), resourceId and resourceType. The model documents no message text for the shape, so the message is recorded behavior only: deleting a queue-fleet association that is still ACTIVE is refused HTTP 409, error code ConflictException, with the message

    QueueFleetAssociation is in ACTIVE status and cannot be deleted.

Fixed text of this error class: "QueueFleetAssociation is in <status> status and cannot be deleted." — the resource kind spelled as the PascalCase shape name "QueueFleetAssociation", the sentence ending in a period. The one variable part is <status>, the association's current QueueFleetAssociationStatus read from the service's state (ACTIVE in the recording; the three ids in the request are NOT named by the sentence at all). The 409 status and the ConflictException code the emulator returns already match and are not part of this finding.

Emulator: the same call answers 409 ConflictException — correct status, correct code — but with the message "Request could not be completed. Status ACTIVE of resource <farmId>, <queueId> and <fleetId> does not permit the operation. The valid statuses are [STOPPED]". That is the shared status-conflict template (no trailing period, a composite resource id AWS does not mention, and a valid-status list AWS does not emit) applied to an operation whose refusal AWS words differently.

Why: the message is part of the contract for this refusal. A client that distinguishes the "association must be stopped first" conflict from the other ConflictException reasons by matching AWS's sentence — the pattern the AWS CLI/console surface and that test suites and retry/drain wrappers assert on — never matches, so it either reports an opaque conflict to the operator or takes a wrong branch instead of driving UpdateQueueFleetAssociation to STOP_SCHEDULING_AND_* and retrying. The association is also left unidentified in the operator-visible text that AWS names by resource kind.

Fix: the refusal built for this operation — DeleteQueueFleetAssociation's conflict branch and the association-specific refusal builder it calls, not the shared status-conflict class, which other operations (DeleteWorker, DeleteQueueLimitAssociation) legitimately word that way. The queue-fleet-association refusal needs its own sentence, with the status taken from the stored association. No open issue carries this: 20261102T095011Z-deadline-queue-fleet-association-lifecycle-diverges-on-five.md (fixed) covered the association's timestamps, 404 wording, status progression and DeleteFleet conflict, and says nothing about this 409's message; 20261114T202101Z-deadline-deleteworker-on-a-started-worker-returns-a.md (fixed) is the DeleteWorker status-conflict wording, i.e. the generic class this operation is now wrongly reusing.

Traces: deadline.queue-fleet-association-delete-before-stopped-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout (20/30 passed, bootstrap_signal=false; service token 'deadline' is bootstrapped).

[mismatch] deadline.queue-fleet-association-delete-before-stopped-rejected
  check_deadline_queue_fleet_association_delete_before_stopped_rejected.py:59
  assert error.get("Message") == "QueueFleetAssociation is in ACTIVE status and cannot be deleted."
  AssertionError: expected the exact ConflictException message, got 'Request could not be completed. Status ACTIVE of resource farm-85e7dc5430335e96b3b25419915dcb78, queue-2e4fba69c4a4be93ce0328509232b028 and fleet-a432c93cb68e7bde27d49df81847df8c does not permit the operation. The valid statuses are [STOPPED]'

The farm-/queue-/fleet- ids in the observed message are that run's generated values and are not requirements; the requirement is the sentence template and the status token read from state. The preceding assertion in the same check — error Code == "ConflictException" — PASSED, so the status class and error code are right and only the message diverges. The check stops at line 59, so its later assertions (association remains ACTIVE; delete succeeds once STOPPED; the subsequent ResourceNotFoundException wording) were never reached and are not claimed here.

Model evidence: smithy_query(operation="DeleteQueueFleetAssociation") -> DELETE /2023-10-12/farms/{farmId}/queue-fleet-associations/{queueId}/{fleetId}, idempotent, success 200, empty output, errors include ConflictException httpError 409; smithy_query(shape="ConflictException") -> errorKind client, httpError 409, required members message/reason/resourceType/resourceId, optional context.

Code: services/service-com-amazonaws-deadline-deadline/src/ops/delete_queue_fleet_association.rs:224-235 refuses with crate::state::queue_fleet_association_not_deletable(farm_id, queue_id, fleet_id, status); services/service-com-amazonaws-deadline-deadline/src/state.rs:1938-1950 forwards to status_does_not_permit(RESOURCE_TYPE_QUEUE_FLEET_ASSOCIATION, queue_fleet_association_id(..), status, &[QUEUE_FLEET_ASSOCIATION_STATUS_STOPPED]); services/service-com-amazonaws-deadline-deadline/src/state.rs:1907-1928 is the shared template that produces the diverging sentence ("Request could not be completed. Status {status} of resource {resource_id} does not permit the operation. The valid statuses are [{..}]").

## Steps to Reproduce
Replay behavioral check deadline.queue-fleet-association-delete-before-stopped-rejected, or by hand against the emulator:

1. deadline.create_farm(displayName=...) -> farmId
2. deadline.create_queue(farmId=farmId, displayName=..., jobRunAsUser={"runAs": "WORKER_AGENT_USER"}) -> queueId
3. deadline.create_fleet(farmId=farmId, displayName=..., roleArn=<fleet trust role>, maxWorkerCount=1, configuration={"customerManaged": {...}}) -> fleetId; poll deadline.get_fleet until status != "CREATE_IN_PROGRESS"
4. deadline.create_queue_fleet_association(farmId=farmId, queueId=queueId, fleetId=fleetId) — the association is ACTIVE
5. deadline.delete_queue_fleet_association(farmId=farmId, queueId=queueId, fleetId=fleetId)

Observed: HTTP 409 ConflictException with Message 'Request could not be completed. Status ACTIVE of resource <farmId>, <queueId> and <fleetId> does not permit the operation. The valid statuses are [STOPPED]'.
Expected: HTTP 409 ConflictException with Message 'QueueFleetAssociation is in ACTIVE status and cannot be deleted.' — i.e. 'QueueFleetAssociation is in <current association status> status and cannot be deleted.'
