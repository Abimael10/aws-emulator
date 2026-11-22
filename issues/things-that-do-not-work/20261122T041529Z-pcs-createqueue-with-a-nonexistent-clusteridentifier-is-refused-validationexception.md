Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pcs.create-queue-nonexistent-cluster-not-found
Issue-Shape-Id: com.amazonaws.pcs#AWSParallelComputingService
Issue-Key: pcs.create-queue-nonexistent-cluster-not-found|_assert_error#5|AssertionError|assert error.get("Code") == code, f"expected {code}, got {error.get('Code')!r}"
Issue-Commit: 861999866ea9ac28e07c2c6c1f5e841f1e39e080

# CreateQueue with a nonexistent clusterIdentifier is refused ValidationException on computeNodeGroupConfigurations instead of ResourceNotFoundException "AWS PCS couldn't find the cluster you specified."

> _Note: this issue was identified against the code base at commit `861999866ea9ac28e07c2c6c1f5e841f1e39e080`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateQueue

## Description
Cause 1 of 1 — CreateQueue resolves the cluster AFTER applying its own queue-association minimum, so a request naming a cluster that does not exist is refused for the wrong reason, with the wrong error code and the wrong HTTP status.

Failing check ids: pcs.create-queue-nonexistent-cluster-not-found

AWS: CreateQueue models ResourceNotFoundException (httpError 404) and CreateQueueRequest$clusterIdentifier is @required with pattern ^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,40})$; computeNodeGroupConfigurations targets ComputeNodeGroupConfigurationList and is NOT @required and carries NO @length (Smithy: CreateQueue input members + errors, pcs model). A request whose clusterIdentifier is well formed (a generated-id form, pcs_ followed by alphanumerics) but names no cluster is answered ResourceNotFoundException with the service's fixed sentence "AWS PCS couldn't find the cluster you specified." — fixed text in full, no variable part; the request's own values are reported in the modelled members (resourceId = the identifier as supplied, resourceType = the identifier label). Recorded behavior: the same request, with no computeNodeGroupConfigurations supplied, is answered ResourceNotFoundException by AWS, i.e. the cluster is resolved before any rule about the queue's compute-node-group associations is applied. Hypothesis: AWS treats the association requirement as a rule about the queue it would write inside a cluster, so it cannot be reached for a cluster that does not exist.

Emulator: the call is refused ValidationException (HTTP 400). Replay output: `AssertionError: expected ResourceNotFoundException, got 'ValidationException'`. The handler applies its own lower bound on the number of configurations that NAME a compute node group (state::queue_association_violation / MIN_QUEUE_ASSOCIATIONS = 1, surfaced through state::field_validation as the constraint clause "Member must have length greater than or equal to 1" against member computeNodeGroupConfigurations) BEFORE it locks the store and resolves clusterIdentifier; the ResourceNotFoundException/AccessDeniedException arm for an unresolved cluster is only reached once that check has passed. So for a well-formed-but-unknown cluster the emulator never asks whether the cluster exists at all.

Why: a client that provisions a cluster and then creates queues against the id it was handed relies on 404/ResourceNotFoundException to mean "cluster not there (yet)" and retries; a 400/ValidationException naming computeNodeGroupConfigurations is a non-retryable input error, so the client aborts the provisioning run, or "fixes" an input that was never wrong. Error-handling code that branches on the error code (ResourceNotFoundException -> create/await the cluster) takes the wrong branch, and the modelled resourceId/resourceType detail that tells it WHICH identifier did not resolve is absent entirely.

Fix: the CreateQueue handler's validation ordering — resolve clusterIdentifier against the store (the existing unresolved-cluster arm) before applying the service's own queue-association lower bound, so an unknown cluster is answered ResourceNotFoundException with the fixed not-found sentence; if the association minimum is genuinely a request-shape rule for this operation it still cannot precede cluster resolution, since the model places no lower bound on that member at the boundary.

Traces: behavioral check pcs.create-queue-nonexistent-cluster-not-found (clusterIdentifier of the generated-id form naming no cluster, queueName well formed, no computeNodeGroupConfigurations). The same check then asserts ListQueues on that identifier is likewise ResourceNotFoundException with the same sentence; it was never reached because CreateQueue failed first, so ListQueues' ordering should be confirmed on the same replay.

## Evidence
Check: pcs.create-queue-nonexistent-cluster-not-found
Request: CreateQueue(clusterIdentifier="pcs_"+"2"*20, queueName=<generated, matches ^[A-Za-z][A-Za-z0-9-]+$, 3..25>) — no computeNodeGroupConfigurations.

Expected (AWS, and what the check asserts):
  HTTP 404, Error.Code = "ResourceNotFoundException"
  Error.Message = "AWS PCS couldn't find the cluster you specified."  (entirely fixed text; the supplied identifier appears in resourceId, not in the sentence)

Actual (emulator):
  Error.Code = "ValidationException" (HTTP 400)
  Replay: File "check_pcs_create_queue_nonexistent_cluster_not_found.py", line 50, in _assert_error
    assert error.get("Code") == code, f"expected {code}, got {error.get('Code')!r}"
    AssertionError: expected ResourceNotFoundException, got 'ValidationException'
  The ValidationException is the emulator's constraint-violation sentence over member "computeNodeGroupConfigurations" with the clause "Member must have length greater than or equal to 1" (fixed clause text; the member path and the bound 1 are the variable parts) — reason fieldValidationFailed, one fieldList entry.

Code path (commit as cloned):
  services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_queue.rs:242-254 — named_groups count + state::queue_association_violation -> ValidationException, executed before the store is locked.
  create_queue.rs:268-302 — state.lock(), settle(), then cluster_id_for(&input.cluster_identifier) with the ResourceNotFoundException arm (crate::state::not_found(CLUSTER, ..)).
  services/.../src/state.rs:1167-1176 — not_found() produces exactly "AWS PCS couldn't find the {noun} you specified."
  services/.../src/state.rs:203-210, 1301-1304, 1067-1069 — MIN_QUEUE_ASSOCIATIONS/queue_association_violation/length_at_least_constraint.
Smithy (pcs model, query=operation CreateQueue): clusterIdentifier required+pattern; computeNodeGroupConfigurations optional, no @length; errors include ResourceNotFoundException httpError 404 and ValidationException httpError 400.

## Steps to Reproduce
1. Point a botocore pcs client at the emulator.
2. Call CreateQueue with a clusterIdentifier of the generated-id form that names no cluster (e.g. "pcs_" followed by 20 alphanumerics), a valid queueName, and NO computeNodeGroupConfigurations.
3. Observed: ClientError with Code "ValidationException" (HTTP 400) whose message reports a constraint violation on computeNodeGroupConfigurations ("Member must have length greater than or equal to 1").
4. Expected: ClientError with Code "ResourceNotFoundException", HTTP 404, Message "AWS PCS couldn't find the cluster you specified.", resourceId = the identifier as supplied, resourceType = the cluster identifier label.
5. Then call ListQueues(clusterIdentifier=<same bogus identifier>): it must also be ResourceNotFoundException with the same sentence (the check asserts this next and never reached it).
6. Positive control from the same check: ListClusters(maxResults=1) still returns a `clusters` list.
