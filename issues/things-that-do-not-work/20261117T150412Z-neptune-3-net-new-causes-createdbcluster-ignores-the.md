Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: neptune.cluster-port-survives-modify-then-blocks-duplicate-and-busy-delete neptune.instance-endpoint-port-inherits-cluster-custom-port neptune.modify-cluster-port-outside-valid-range-rejected neptune.create-cluster-port-outside-valid-range-rejected neptune.delete-cluster-final-snapshot-rejected-while-instance-present
Issue-Shape-Id: com.amazonaws.neptune#AmazonRDSv19
Issue-Key: neptune.cluster-port-survives-modify-then-blocks-duplicate-and-busy-delete|run#10|AssertionError|assert created["Port"] == _PORT, f"cluster Port was {created['Port']!r}, expected {_PORT}"
Issue-Key: neptune.instance-endpoint-port-inherits-cluster-custom-port|run#10|AssertionError|assert created_cluster["Port"] == _CUSTOM_PORT, created_cluster
Issue-Key: neptune.modify-cluster-port-outside-valid-range-rejected|run#9|AssertionError|assert created["Port"] == _STARTING_PORT, created
Issue-Key: neptune|CreateDBCluster|InvalidParameterValue
Issue-Key: neptune.delete-cluster-final-snapshot-rejected-while-instance-present|run#20|AssertionError|assert message == expected_refusal_message, (
Issue-Commit: 238fa933eaa755325de1be4f75aa8cc4e9812c10

# neptune: 3 net-new causes — CreateDBCluster ignores the request's Port, and the port-range and cluster-not-empty refusal messages are emulator-authored

> _Note: this issue was identified against the code base at commit `238fa933eaa755325de1be4f75aa8cc4e9812c10`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBCluster

## Description
Three net-new root causes in the Neptune DB-cluster control plane, all found by the behavioral suite. THIS ISSUE HAS 3 CAUSES — all three must be closed before submitting.

Related open issue 20261116T010226Z-neptune-dbcluster-projection-publishes-the-emulator-s-listener.md covers the DBCluster projection's Port when the create request names NO port (it must be the engine default 8182, not the emulator's listener port). It does NOT cover cause 1 below, which is the request's OWN Port value being discarded, and it does not cover causes 2 and 3 at all; the checks here are not marked owned by it.

Cause 1 of 3: CreateDBCluster discards the Port supplied in the request and reports an unrelated, per-run port instead.
AWS: Smithy (services/service-com-amazonaws-neptune-amazonrdsv19/smithy-model/com-amazonaws-neptune-amazonrdsv19.json) CreateDBClusterMessage.Port (IntegerOptional), doc "The port number on which the instances in the DB cluster accept connections. Default: 8182"; neptune#DBCluster.Port doc "Specifies the port that the database engine is listening on". So a create that names a port in the valid range stores THAT port, and CreateDBCluster / DescribeDBClusters / ModifyDBCluster all report exactly it; it changes only when ModifyDBCluster changes it, and it does not depend on the serving process or on the run. Member DB instances inherit it: DescribeDBInstances reports Endpoint.Port equal to the cluster's Port.
Emulator: the cluster comes back with a port unrelated to the request and different on every run — the checks requested 8191, 8183 and 8182 and the emulator reported 46505, 45935 and 43191 respectively (full DescribeDBClusters record otherwise plausible: Engine 'neptune', EngineVersion '1.4.8.0', Status 'creating'). Because the stored port is wrong from creation, everything downstream of it is wrong too: a modify that must preserve the port cannot be graded, and an instance's Endpoint.Port cannot inherit the cluster's requested port.
Why: a client that creates a Neptune cluster on a chosen port (the documented way to move the graph endpoint off 8182, e.g. for security-group or proxy rules) is told the engine listens somewhere else, and the value it is told is not even stable across runs, so no connection string, security-group rule or DNS record built from DBCluster.Port or DBInstance.Endpoint.Port can be correct.
Fix: the CreateDBCluster handler's input binding and the cluster state it writes — the request's Port must be stored on the cluster and projected by the shared DBCluster projection (and inherited by member-instance endpoints), with 8182 used only as the default when the request omits Port.
Traces: neptune.cluster-port-survives-modify-then-blocks-duplicate-and-busy-delete (requested 8191, got 46505); neptune.instance-endpoint-port-inherits-cluster-custom-port (requested 8183, got 45935); neptune.modify-cluster-port-outside-valid-range-rejected (requested 8182, got 43191).

Cause 2 of 3: the out-of-range-port refusal carries an emulator-authored message instead of AWS's endpoint-port wording.
AWS: recorded behavior only for the message text (Smithy models Port only as IntegerOptional with no range trait, and InvalidParameterValue is a common query-protocol error): a CreateDBCluster (and likewise ModifyDBCluster) whose Port is below the floor is refused HTTP 400, Error.Code 'InvalidParameterValue', Error.Message 'Invalid endpoint port 1149. Valid range is: 1150-65535 '. Fixed text: 'Invalid endpoint port ', '. Valid range is: ', '-65535' and the TRAILING SPACE; variable parts: the rejected port value from the request (1149 here) and the floor 1150 (the engine's minimum) — a value from one run is never part of the template.
Emulator: the code is right but the message is the emulator's generic validation prose: 'invalid value 1149 for Port: must be a value from 1150 to 65535'.
Why: a client or operator that matches on the service's wording (or surfaces it to a user) sees text AWS never emits, and the refusal cannot be distinguished from a generic parameter-shape complaint; CloudFormation-style wrappers that embed the service message verbatim propagate the wrong text.
Fix: the port validation shared by the CreateDBCluster and ModifyDBCluster handlers — it must raise the Neptune endpoint-port message, not the generic 'invalid value X for <member>' template.
Traces: neptune.create-cluster-port-outside-valid-range-rejected; neptune.modify-cluster-port-outside-valid-range-rejected also asserts this refusal on ModifyDBCluster but is blocked earlier by cause 1.

Cause 3 of 3: DeleteDBCluster's refusal for a cluster that still has member instances is misworded.
AWS: Smithy DeleteDBCluster errors include InvalidDBClusterStateFault (client error, httpError 400, awsQueryError code 'InvalidDBClusterStateFault'). Recorded behavior for the text: deleting a cluster that still contains a DB instance in a non-deleting state is refused 400 / 'InvalidDBClusterStateFault' with Error.Message "Cluster can't be deleted. It still contains DB instances in a non-deleting state." — that message is entirely fixed text, with no values from the request or state. The refusal must also leave the cluster 'available' and create no final snapshot (those parts of the check passed).
Emulator: same code, but the message reads 'Cluster cannot be deleted, it still contains DB instances in non-deleting state.' — different wording ("cannot be deleted," vs "can't be deleted.", and "in non-deleting state" vs "in a non-deleting state").
Why: a client that keys teardown retry logic or user-facing text off the service message gets text AWS never emits; the sentence structure differs too (one sentence vs two), so even prefix matching fails.
Fix: the DeleteDBCluster handler's non-empty-cluster guard — the InvalidDBClusterStateFault message string.
Traces: neptune.delete-cluster-final-snapshot-rejected-while-instance-present.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 2/19 checks passed; per-service neptune 2/7 (5 failed). None of the five checks below is marked ALREADY FILED or OWNED.

Cause 1:
- neptune.cluster-port-survives-modify-then-blocks-duplicate-and-busy-delete [mismatch]: AssertionError: cluster Port was 46505, expected 8191 (check creates the cluster with Port=8191 and asserts the created cluster reports it).
- neptune.instance-endpoint-port-inherits-cluster-custom-port [mismatch]: AssertionError at `assert created_cluster["Port"] == _CUSTOM_PORT` (_CUSTOM_PORT = 8183); the DescribeDBClusters record shows 'Port': 45935 alongside Engine 'neptune', EngineVersion '1.4.8.0', DBClusterParameterGroup 'default.neptune1.4'.
- neptune.modify-cluster-port-outside-valid-range-rejected [mismatch]: AssertionError at `assert created["Port"] == _STARTING_PORT` (_STARTING_PORT = 8182); record shows 'Port': 43191.
The three reported ports (46505 / 45935 / 43191) are all unrelated to the requested value and differ per run.

Cause 2:
- neptune.create-cluster-port-outside-valid-range-rejected [mismatch]: AssertionError: expected message 'Invalid endpoint port 1149. Valid range is: 1150-65535 ', got 'invalid value 1149 for Port: must be a value from 1150 to 65535' (the error Code assertion passed, so only the message diverges).

Cause 3:
- neptune.delete-cluster-final-snapshot-rejected-while-instance-present [mismatch]: AssertionError: unexpected refusal message 'Cluster cannot be deleted, it still contains DB instances in non-deleting state.', expected "Cluster can't be deleted. It still contains DB instances in a non-deleting state."

Model evidence (smithy_query on services/service-com-amazonaws-neptune-amazonrdsv19/smithy-model/com-amazonaws-neptune-amazonrdsv19.json): CreateDBClusterMessage.Port -> IntegerOptional, doc 'The port number on which the instances in the DB cluster accept connections. Default: 8182'; ModifyDBClusterMessage.Port -> IntegerOptional; DeleteDBCluster errors include InvalidDBClusterStateFault (httpError 400, awsQueryError code InvalidDBClusterStateFault).

Not claimed as requirements: the generated cluster/instance identifiers, the DbClusterResourceId, ClusterCreateTime and the endpoint hostnames in the recorded describe records; only the stored/reported port value, the status codes, the error codes and the message templates above are.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from this checkout, and replay the simparity-behavioral-aws-v1 behavioral suite.
2. Cause 1: replay neptune.cluster-port-survives-modify-then-blocks-duplicate-and-busy-delete, neptune.instance-endpoint-port-inherits-cluster-custom-port and neptune.modify-cluster-port-outside-valid-range-rejected. Or directly: neptune CreateDBCluster with Engine=neptune and Port=8191 in a two-AZ subnet group, then DescribeDBClusters — DBCluster.Port must be 8191 (and must still be 8191 after an unrelated ModifyDBCluster); create a member DB instance and check DescribeDBInstances Endpoint.Port equals the cluster's port. Re-run twice: the port must not vary between runs.
3. Cause 2: replay neptune.create-cluster-port-outside-valid-range-rejected. Or directly: CreateDBCluster with Port=1149 — expect HTTP 400, Error.Code InvalidParameterValue, Error.Message 'Invalid endpoint port 1149. Valid range is: 1150-65535 ' (note the trailing space); the same wording is expected from ModifyDBCluster with Port=1149 once cause 1 is fixed.
4. Cause 3: replay neptune.delete-cluster-final-snapshot-rejected-while-instance-present. Or directly: create a cluster, create one DB instance in it, then DeleteDBCluster with SkipFinalSnapshot=false and a FinalDBSnapshotIdentifier — expect 400 / InvalidDBClusterStateFault with message "Cluster can't be deleted. It still contains DB instances in a non-deleting state.", the cluster still 'available', and no snapshot created.
