Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: deadline.queue-environment-create-get-semantics
Issue-Shape-Id: com.amazonaws.deadline#Deadline
Issue-Key: deadline.queue-environment-create-get-semantics|run#18|AssertionError|assert error.get("Message") == (
Issue-Commit: e0ec1c1edefac5aae54b076e6361615d0195bedd

# deadline GetQueueEnvironment 404 names the resource type "environment" with only the environment id, instead of AWS's "queue-env" with the quoted queueId / queueEnvironmentId pair

> _Note: this issue was identified against the code base at commit `e0ec1c1edefac5aae54b076e6361615d0195bedd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueueEnvironment

## Description
One net-new root cause.

Cause 1 of 1: the ResourceNotFoundException this service answers for a queue environment it does not hold carries the wrong resourceType token and the wrong resourceId, so its message sentence is wrong. Failing check: deadline.queue-environment-create-get-semantics.

AWS: GetQueueEnvironment (Smithy: GET /2023-10-12/farms/{farmId}/queues/{queueId}/environments/{queueEnvironmentId}; modeled error ResourceNotFoundException, errorKind client, httpError 404; ResourceNotFoundException members message/resourceId/resourceType all @required). A queue environment is NOT reachable by its own id alone — the model reaches it only through its queue — and the recorded behavior shows Deadline Cloud names it as a COMPOSITE resource whose type token is `queue-env` and whose id is the queue id and the environment id together, each single-quoted and separated by ` / `. For a GetQueueEnvironment on an environment that was deleted (or never existed) under an existing farm and queue, AWS answers HTTP 404, Code `ResourceNotFoundException`, with the message:

    Resource of type queue-env with id '<queueId>' / '<queueEnvironmentId>' does not exist.

Fixed text of that template: `Resource of type queue-env with id `, the two single-quoted slots joined by ` / `, and the trailing ` does not exist.` including the period. The only variable parts are <queueId> and <queueEnvironmentId>, taken verbatim from the request path; no value from any one run (the `queue-...`/`queueenv-...` hex ids in a recording) is part of the requirement.

Emulator: answers ResourceNotFoundException/404 (code and status are correct) but words it with a different resource-type token and a non-composite id — the replay observed:

    Resource of type environment with id queueenv-34e168cdb6e4c43cd8cdbdbd17b1e32b does not exist.

i.e. type token `environment` instead of `queue-env`, the queue id omitted entirely, and the id not single-quoted. The same helper fills the exception's @required `resourceType`/`resourceId` members, so those two fields diverge too (`environment` / the bare environment id, rather than `queue-env` / the composite pair).

Why: a client that distinguishes Deadline Cloud 404s by the exception's `resourceType` member — or that parses/asserts on the message, as the behavioral check and AWS's own documented wording do — cannot tell a missing queue environment from some other "environment"-typed resource, and never learns WHICH queue the lookup was scoped to. Error-message and resourceType assertions written against real AWS fail against the emulator even though the call is refused for the right reason.

Fix: the queue-environment not-found constructor and its resource-type constant in this service's state, i.e. `queue_environment_not_found` / `RESOURCE_TYPE_ENVIRONMENT`, which the GetQueueEnvironment handler calls for its third lookup miss. The constant is currently derived from the URI segment `/environments/` rather than from the service's recorded type token, and the constructor passes only the environment id to the shared one-id `not_found` sentence, so a composite-named resource needs its own sentence (queue id and environment id, each single-quoted, joined by ` / `) the way this crate already special-cases the queue-fleet association. Any other queue-environment operation routed through the same helper inherits the same wrong wording and should be fixed with it.

Traces: deadline.queue-environment-create-get-semantics — CreateQueueEnvironment, GetQueueEnvironment round-trip, DeleteQueueEnvironment, then GetQueueEnvironment on the deleted id. Code citations: services/service-com-amazonaws-deadline-deadline/src/state/queue_environments.rs:20 (RESOURCE_TYPE_ENVIRONMENT = "environment"), :63-67 (queue_environment_not_found passes one id); services/service-com-amazonaws-deadline-deadline/src/state.rs:1723-1733 (shared single-id not_found sentence); services/service-com-amazonaws-deadline-deadline/src/ops/get_queue_environment.rs:187-190 (the call site).

## Evidence
Check deadline.queue-environment-create-get-semantics, final assertion:

  assert error.get("Code") == "ResourceNotFoundException"   -> PASSES (status 404, code correct)
  assert error.get("Message") == (
      f"Resource of type queue-env with id '{queue_id}' / '{env_id}' does not exist.")

Expected (AWS, template; <queueId>/<queueEnvironmentId> come from the request path):
  Resource of type queue-env with id '<queueId>' / '<queueEnvironmentId>' does not exist.

Actual (emulator, as replayed):
  AssertionError: expected the exact ResourceNotFoundException message, got 'Resource of type environment with id queueenv-34e168cdb6e4c43cd8cdbdbd17b1e32b does not exist.'

Divergences: resource-type token `environment` vs `queue-env`; id is the bare environment id, unquoted, with the owning queue id missing, vs the single-quoted `'<queueId>' / '<queueEnvironmentId>'` pair. The exception's @required resourceType/resourceId members are filled from the same two wrong values. The earlier part of the check (priority 5, templateType JSON, template body round-trip through GetQueueEnvironment) passed, so only the refusal's wording diverges.

## Steps to Reproduce
1. deadline.CreateFarm -> farmId; deadline.CreateQueue(farmId=...) -> queueId.
2. deadline.CreateQueueEnvironment(farmId, queueId, priority=5, templateType="JSON", template=<valid environment-2023-09 JSON document>) -> queueEnvironmentId.
3. deadline.DeleteQueueEnvironment(farmId, queueId, queueEnvironmentId).
4. deadline.GetQueueEnvironment(farmId, queueId, queueEnvironmentId) on the now-deleted id.
5. Inspect the 404 body: Error.Code is ResourceNotFoundException (correct), but Error.Message reads "Resource of type environment with id <queueEnvironmentId> does not exist." instead of AWS's "Resource of type queue-env with id '<queueId>' / '<queueEnvironmentId>' does not exist.", and the resourceType/resourceId members carry `environment` / the bare environment id instead of `queue-env` / the composite pair.
