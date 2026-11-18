Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pcs.create-compute-node-group-nonexistent-cluster-not-found pcs.create-queue-nonexistent-cluster-not-found
Issue-Shape-Id: com.amazonaws.pcs#AWSParallelComputingService
Issue-Key: pcs.create-compute-node-group-nonexistent-cluster-not-found|_assert_error#11|AssertionError|assert error.get("Code") == code, f"expected {code}, got {error.get('Code')!r}"
Issue-Key: pcs.create-queue-nonexistent-cluster-not-found|_assert_error#5|AssertionError|assert error.get("Code") == code, f"expected {code}, got {error.get('Code')!r}"
Issue-Commit: 55eb763e8158a713d28edd00c224c291b78f1e19

# pcs: cluster-scoped child operations refuse a nonexistent pcs_-ID clusterIdentifier with AccessDeniedException instead of ResourceNotFoundException (2 causes)

> _Note: this issue was identified against the code base at commit `55eb763e8158a713d28edd00c224c291b78f1e19`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetComputeNodeGroup

## Description
REOPENS 20261029T190856Z-pcs-5-net-new-root-causes-cluster-queue.md: its fix left the call refused. That issue's Cause 3 made every cluster-scoped child operation answer AccessDeniedException when the clusterIdentifier resolves to no cluster. That is right only for the NAME form of the identifier (which is all its recordings used: 'sim2real-probe-pcs-nosuchcluster-…'). The behavioral corpus records the other admitted form — a well-formed but nonexistent `pcs_…` cluster ID — and there PCS answers ResourceNotFoundException with its fixed cluster sentence. The emulator does not discriminate the two identifier forms, so the ID form is now refused with the wrong error class.

Where it lives in the checkout: every cluster-scoped handler resolves the identifier with `crate::state::ComAmazonawsPcsAwsparallelcomputingserviceState::cluster_id_for` and, on None, unconditionally returns `crate::state::cluster_authorization::cluster_denied(...)` — see services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_queue.rs around the `let Some(cluster_id) = state.cluster_id_for(&input.cluster_identifier) else { … AccessDeniedException … }` branch, and the module doc of src/state/cluster_authorization.rs, which states the rule for the identifier 'a name or a `pcs_…` id' alike (its unit test even asserts the denial for `pcs_…` ids). The not-found the ID form needs is already built by `crate::state::not_found(crate::state::CLUSTER, identifier)` in src/state.rs.

TWO causes, both to be closed before submitting; each names its own failing check so each can be re-replayed independently.

=== Cause 1 of 2: the compute-node-group read/list/delete family on a nonexistent `pcs_` cluster ID ===
Failing check: pcs.create-compute-node-group-nonexistent-cluster-not-found (it asserts GetComputeNodeGroup, then ListComputeNodeGroups and DeleteComputeNodeGroup on the same identifier; it fails on the first of them).
AWS: Smithy (services/service-com-amazonaws-pcs-awsparallelcomputingservice/smithy-model/com-amazonaws-pcs-awsparallelcomputingservice.json): GetComputeNodeGroup / ListComputeNodeGroups / DeleteComputeNodeGroup take clusterIdentifier of shape ClusterIdentifier, @pattern ^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,40})$ — i.e. EITHER a generated `pcs_` id OR a name — and each models both AccessDeniedException (client, @httpError 403, required member message) and ResourceNotFoundException (client, @httpError 404, required members message/resourceId/resourceType, doc: 'The requested resource can't be found. The cluster, node group, or queue you're attempting to get, update, list, or delete doesn't exist.'). The model does not say which branch applies to which identifier form, so for that: recorded behavior only. The recording shows that when clusterIdentifier is in `pcs_` ID form and no such cluster exists, the refusal is ResourceNotFoundException whose message is the service's FIXED cluster sentence, "AWS PCS couldn't find the cluster you specified." — the whole sentence is fixed text, it echoes no caller value; the request's value belongs in resourceId, with resourceType the identifier-kind token ('Cluster ID', per the already-filed resourceType rule). Hypothesis (neither model nor recording states the mechanism): IAM authorization is decided against the cluster ARN, which is built from the cluster ID, so a `pcs_` id can be authorized without resolving anything and the call proceeds to resolution and reports not-found, while a NAME cannot be turned into an ARN before resolution and is therefore refused as undecidable authorization — which is why the name form legitimately answers AccessDeniedException.
Emulator: answers AccessDeniedException ('User: <principal> is not authorized to perform: pcs:GetComputeNodeGroup on resource: <identifier> as either the resource does not exist, some policy explicitly denies access, or no policy grants access') for the `pcs_` ID form too; the replay reports 'expected ResourceNotFoundException, got AccessDeniedException'.
Why: the error CODE is wrong in the direction that stops a client dead. A caller polling or tearing down by cluster ID, or one that tolerates a not-found as 'already gone', sees a terminal permissions failure instead and reports a credentials/policy problem to a human; permission-diagnostics tooling records an authorization denial that PCS never issued. The body also differs: ResourceNotFoundException carries message/resourceId/resourceType, so a client reading resourceId or switching on resourceType gets nothing.
Fix: the cluster-resolution branch of the compute-node-group read/list/delete handlers and the shared decision in src/state/cluster_authorization.rs — make the refusal depend on the FORM of the supplied clusterIdentifier: a `pcs_`-prefixed id that resolves to nothing must raise the modeled ResourceNotFoundException built by the state module's cluster not-found helper; only a NAME keeps the AccessDeniedException path.
Traces: pcs.create-compute-node-group-nonexistent-cluster-not-found.

=== Cause 2 of 2: the queue create/list family on a nonexistent `pcs_` cluster ID ===
Failing check: pcs.create-queue-nonexistent-cluster-not-found (CreateQueue, then ListQueues on the same identifier; it fails on CreateQueue).
AWS: Smithy: CreateQueue and ListQueues take the same ClusterIdentifier shape (same @pattern admitting both forms) and model AccessDeniedException and ResourceNotFoundException alongside each other. Recorded behavior: with a nonexistent `pcs_` cluster ID, both CreateQueue and ListQueues are refused ResourceNotFoundException with the same fixed sentence "AWS PCS couldn't find the cluster you specified." (all fixed text), the supplied identifier in resourceId; no queue is created. The emulator's own positive control in the same check — ListClusters still returning a list — shows the refusal is specific to the identifier, not to the credential.
Emulator: CreateQueue answers AccessDeniedException ('… not authorized to perform: pcs:CreateQueue on resource: <identifier> …'); the replay reports 'expected ResourceNotFoundException, got AccessDeniedException'. ListQueues takes the same branch (not reached, as the check stops at the first assertion).
Why: the same inversion as Cause 1, on the create path: provisioning code that creates a cluster and then a queue by the cluster's returned ID treats a 'cluster not there yet' not-found as retryable, but an authorization denial as fatal, so it aborts the deployment against the emulator where it would retry and succeed against PCS.
Fix: the cluster-resolution branch of ops/create_queue.rs and ops/list_queues.rs, through the same shared identifier-form rule as Cause 1 — the `pcs_`-ID form must produce the modeled ResourceNotFoundException (message/resourceId/resourceType) and persist nothing; the name form keeps AccessDeniedException.
Traces: pcs.create-queue-nonexistent-cluster-not-found.

Related but NOT covering: open 20261106T154605Z-pcs-2-net-new-root-causes-resourcenotfoundexception-resourcetype.md (the resourceType token carried by a not-found that IS already raised), open 20261107T101541Z-pcs-cluster-scoped-delete-operations-refuse-an-unknown.md (the NAME form on DeleteComputeNodeGroup/DeleteQueue, which must stay AccessDeniedException), open 20261107T062642Z-pcs-createcomputenodegroup-never-enforces-the-iam-instance-profile.md and open 20261107T094955Z-pcs-lifecycle-transitions-are-deferred-behind-a-fixed.md (the other two failing pcs checks in this run, already owning them). None of them carries the replay key of either check above.

## Evidence
Behavioral replay of simparity-behavioral-aws-v1 against the booted emulator: 9/13 checks passed; 4 failed, all service pcs. Two are owned by open issues (compute-node-group-and-queue-lifecycle, delete-dependents-out-of-order-conflict-rejected) and skipped. The two below are owned by nothing.

Cause 1 — pcs.create-compute-node-group-nonexistent-cluster-not-found [mismatch]:
  check_pcs_create_compute_node_group_nonexistent_cluster_not_found.py:60
  _assert_error(get_error, "ResourceNotFoundException", "AWS PCS couldn't find the cluster you specified.")
  AssertionError: expected ResourceNotFoundException, got 'AccessDeniedException'
The call is pcs.get_compute_node_group(clusterIdentifier='pcs_' + '1'*20, computeNodeGroupIdentifier='pcs_' + '2'*20) — a well-formed `pcs_` cluster ID that was never created (the 20-digit filler is that run's value, not a requirement). The earlier assertion in the same check (CreateComputeNodeGroup on the same bogus cluster + a nonexistent instance profile => ValidationException 'Instance Profile <name> cannot be found. …') PASSED, so only the cluster-resolution refusal class is at issue. The checks on ListComputeNodeGroups and DeleteComputeNodeGroup with the same expectation follow immediately and are unreached.

Cause 2 — pcs.create-queue-nonexistent-cluster-not-found [mismatch]:
  check_pcs_create_queue_nonexistent_cluster_not_found.py:24
  _assert_error(error, "ResourceNotFoundException", "AWS PCS couldn't find the cluster you specified.")
  AssertionError: expected ResourceNotFoundException, got 'AccessDeniedException'
The call is pcs.create_queue(clusterIdentifier='pcs_' + '2'*20, queueName=<any valid name>). The ListQueues assertion with the same expectation is unreached.

Source confirmation in the checkout (commit of this run's branch): services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_queue.rs — `let Some(cluster_id) = state.cluster_id_for(&input.cluster_identifier) else { return Err(... AccessDeniedException(crate::state::cluster_authorization::cluster_denied(&caller, Self::NAME, &input.cluster_identifier)) ...) };` with no inspection of the identifier's form; src/state.rs `cluster_id_for` accepts an id key or a name key and returns Option<String>; src/state/cluster_authorization.rs documents the denial as applying to 'a name or a `pcs_…` id' and its test asserts the denial for `pcs_00000000000000000000` and `pcs_11111111111111111111`. The ResourceNotFoundException the ID form needs already exists as `crate::state::not_found(kind, identifier)` in src/state.rs, which renders exactly "AWS PCS couldn't find the {noun} you specified." with resourceId = the identifier and resourceType = the kind's identifier label.

Model support: smithy_query on the PCS model — ClusterIdentifier is `@pattern ^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,40})$` (two distinct identifier forms), and GetComputeNodeGroup, ListComputeNodeGroups, DeleteComputeNodeGroup, CreateQueue and ListQueues each bind BOTH AccessDeniedException and ResourceNotFoundException, so both branches are modeled and the identifier form is what selects between them.

## Steps to Reproduce
Boot the emulator and point a botocore PCS client at it (awsJson1_0, sigv4). No cluster of the identifier used below may exist in emulator state.

Cause 1 (check pcs.create-compute-node-group-nonexistent-cluster-not-found):
1. bogus = 'pcs_' + '1'*20  (any well-formed, never-created `pcs_` cluster ID)
2. pcs.get_compute_node_group(clusterIdentifier=bogus, computeNodeGroupIdentifier='pcs_' + '2'*20)
   Expect ResourceNotFoundException, Message exactly "AWS PCS couldn't find the cluster you specified.", resourceId = bogus, resourceType the cluster identifier-kind token.
   Observe AccessDeniedException 'User: <principal> is not authorized to perform: pcs:GetComputeNodeGroup on resource: <bogus> as either the resource does not exist, some policy explicitly denies access, or no policy grants access'.
3. pcs.list_compute_node_groups(clusterIdentifier=bogus) and pcs.delete_compute_node_group(clusterIdentifier=bogus, computeNodeGroupIdentifier='pcs_' + '2'*20) — same expectation, same observed denial.

Cause 2 (check pcs.create-queue-nonexistent-cluster-not-found):
1. bogus = 'pcs_' + '2'*20
2. pcs.create_queue(clusterIdentifier=bogus, queueName=<any name matching ^(?!pcs_)[A-Za-z][A-Za-z0-9-]+$, 3..25 chars>)
   Expect ResourceNotFoundException with the same fixed sentence and no queue created. Observe AccessDeniedException with action pcs:CreateQueue.
3. pcs.list_queues(clusterIdentifier=bogus) — same expectation, same observed denial.
4. pcs.list_clusters(maxResults=1) still returns a list, so the denial is not a credential problem.

Contrast (must NOT regress): with a clusterIdentifier in NAME form that resolves to nothing (e.g. 'no-such-cluster'), these same operations must keep answering AccessDeniedException with the not-authorized sentence, as the wire corpus records and as 20261029T190856Z / 20261107T101541Z require.
