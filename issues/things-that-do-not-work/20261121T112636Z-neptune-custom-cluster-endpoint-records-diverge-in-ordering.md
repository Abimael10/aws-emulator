Issue-Source: trace-sweep-divergence
Issue-Trace: neptune-db-cluster-lifecycle-and-custom-endpoint
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: neptune-db-cluster-lifecycle-and-custom-endpoint
Issue-Shape-Id: com.amazonaws.neptune#AmazonRDSv19
Issue-Key: neptune|DescribeDBClusterEndpoints|2xx->2xx|$.DescribeDBClusterEndpointsResponse.DescribeDBClusterEndpointsResult.DBClusterEndpoints.DBClusterEndpointList[*]
Issue-Key: neptune|CreateDBClusterEndpoint|2xx->2xx|$.CreateDBClusterEndpointResponse.CreateDBClusterEndpointResult.DBClusterEndpointResourceIdentifier
Issue-Key: neptune|DeleteDBClusterEndpoint|2xx->2xx|$.DeleteDBClusterEndpointResponse.DeleteDBClusterEndpointResult.DBClusterEndpointResourceIdentifier
Issue-Commit: 3f1d064d908eb4a469d73223dfef0ca475b0d2e0

# Neptune custom cluster-endpoint records diverge in ordering, address and resource identifier

> _Note: this issue was identified against the code base at commit `3f1d064d908eb4a469d73223dfef0ca475b0d2e0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusterEndpoints

## Description
Creating a custom cluster endpoint on a Neptune DB cluster succeeds with status 200, but the endpoint record the emulator projects does not match the record Neptune keeps: the endpoint list is served in a different order from Neptune's, the built-in cluster endpoints carry the emulator's own listener address rather than a cluster endpoint address, and the generated DBClusterEndpointResourceIdentifier does not satisfy the shape the recordings assert. This bundle carries 3 CAUSES; all three must be closed.

Cause 1 of 3: DescribeDBClusterEndpoints after a custom endpoint is created returns a differently ordered list whose built-in entries carry the wrong Endpoint value.
AWS: Smithy: DescribeDBClusterEndpoints output DBClusterEndpoints is a DBClusterEndpointList of DBClusterEndpoint, each with DBClusterEndpointIdentifier, DBClusterIdentifier, DBClusterEndpointResourceIdentifier, Endpoint, Status, EndpointType, CustomEndpointType, StaticMembers, ExcludedMembers, DBClusterEndpointArn; the model fixes no ordering, so recorded behavior only: the recording returns the CUSTOM endpoint as the FIRST list member — a full record carrying EndpointType=CUSTOM, CustomEndpointType, empty StaticMembers/ExcludedMembers, Status=available and a DBClusterEndpointArn of the form arn:aws:rds:<region>:<account>:cluster-endpoint:<DBClusterEndpointIdentifier> — followed by the cluster's built-in WRITER and READER entries, each of which carries only DBClusterIdentifier/Endpoint/Status/EndpointType and whose Endpoint is a resolvable cluster endpoint DNS name of the form <identifier>.cluster-custom-<cluster-resource-token>.<region>.neptune.amazonaws.com (custom), <identifier>.cluster-<token>... (writer) and <identifier>.cluster-ro-<token>... (reader).
Emulator: serves the same three endpoints in a different order — the WRITER entry first and the CUSTOM record third — and gives every built-in entry Endpoint='127.0.0.1', i.e. the emulator's own listener address instead of a cluster-endpoint-shaped address. The per-entry diff therefore reports list[0] missing (CUSTOM record expected, WRITER record served) and list[2] extra (READER record expected, CUSTOM record served).
Why: a client that reads DBClusterEndpoints[0] to pick up the custom endpoint it just created — the common pattern, since Neptune lists the custom endpoint ahead of the built-ins — picks up the writer endpoint instead and gets no CustomEndpointType, no DBClusterEndpointArn and no DBClusterEndpointResourceIdentifier. A client that resolves the returned Endpoint to distinguish the writer, the reader and the custom endpoint cannot: all three collapse onto one loopback literal, so reader/custom routing silently sends every connection to the same place.
Fix: the DescribeDBClusterEndpoints projection in the Neptune cluster-endpoint state/serializer — endpoint records must be emitted in Neptune's order and each entry's Endpoint must be derived from the cluster's endpoint-address form for its EndpointType, not from the emulator's bind address. (The particular host, port, region, account and generated cluster-resource token differ per environment and are not themselves the requirement; the per-EndpointType distinction and the list ordering are.)
Traces: neptune-db-cluster-lifecycle-and-custom-endpoint#task#5.

Cause 2 of 3: CreateDBClusterEndpoint returns a DBClusterEndpointResourceIdentifier that does not match the shape the recording asserts.
AWS: Smithy: CreateDBClusterEndpoint output member DBClusterEndpointResourceIdentifier (String). Recorded behavior only: the value is per-run, so the fixture asserts its SHAPE rather than its text ('<shaped-not-asserted>') — Neptune mints an immutable resource id of the form 'cluster-endpoint-' followed by a fixed-width token of uppercase letters and digits; the emulator's value fails that shape check.
Emulator: returns DBClusterEndpointResourceIdentifier 'cluster-endpoint-NDXIJTISLM29VSANPW4G0', which the shape assertion rejects, so the generator does not produce Neptune-shaped resource ids.
Why: the resource identifier is the stable, rename-proof handle for a cluster endpoint (it appears in CloudTrail and in resource-level policies). A client that validates or stores it with Neptune's format in mind rejects or mis-stores the emulator's value.
Fix: the cluster-endpoint resource-identifier generator used by the CreateDBClusterEndpoint handler/state.
Traces: neptune-db-cluster-lifecycle-and-custom-endpoint#task#3.

Cause 3 of 3: DeleteDBClusterEndpoint echoes the same mis-shaped DBClusterEndpointResourceIdentifier.
AWS: Smithy: DeleteDBClusterEndpoint output member DBClusterEndpointResourceIdentifier (String); the delete response echoes the record being deleted. Recorded behavior only: shape-asserted exactly as in Cause 2.
Emulator: returns the same out-of-shape 'cluster-endpoint-...' value on the delete response.
Why: same as Cause 2 — the identifier is wrong wherever it is surfaced, including on the terminal response a client may persist for audit.
Fix: follows the generator fix in Cause 2, but must be confirmed on the DeleteDBClusterEndpoint response projection too.
Traces: neptune-db-cluster-lifecycle-and-custom-endpoint#task#10.

Related open issue (does not carry these replay keys): 20261116T010226Z-neptune-dbcluster-projection-publishes-the-emulator-s-listener.md covers DBCluster.Port only; the endpoint-list ordering, the per-endpoint Endpoint address and the resource identifier above are net-new.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 90.6%, 17 mismatched steps):

1. fixture neptune-db-cluster-lifecycle-and-custom-endpoint step task#5, DescribeDBClusterEndpoints, expected 200 / actual 200:
   - $...DBClusterEndpoints.DBClusterEndpointList[0]: missing — expected {'EndpointType': 'CUSTOM', 'CustomEndpointType': 'READER', 'Endpoint': '<identifier>.cluster-custom-<token>.<region>.neptune.amazonaws.com', 'DBClusterEndpointArn': 'arn:aws:rds:<region>:<account>:cluster-endpoint:<identifier>', 'DBClusterEndpointIdentifier': '<identifier>', 'DBClusterEndpointResourceIdentifier': '<shaped-not-asserted>', 'StaticMembers': '', 'ExcludedMembers': '', 'Status': 'available'}; got {'DBClusterIdentifier': '<cluster>', 'Endpoint': '127.0.0.1', 'Status': 'available', 'EndpointType': 'WRITER'}
   - $...DBClusterEndpointList[2]: extra — expected the built-in READER entry {'EndpointType': 'READER', 'Endpoint': '<cluster>.cluster-ro-<token>.<region>.neptune.amazonaws.com', 'Status': '<shaped-not-asserted>'}; got the CUSTOM record (EndpointType CUSTOM, CustomEndpointType READER, DBClusterEndpointArn present, Endpoint '127.0.0.1')
   i.e. the CUSTOM record is served third where the recording has it first, and every served entry's Endpoint is the loopback literal.
2. fixture neptune-db-cluster-lifecycle-and-custom-endpoint step task#3, CreateDBClusterEndpoint, expected 200 / actual 200: $.CreateDBClusterEndpointResponse.CreateDBClusterEndpointResult.DBClusterEndpointResourceIdentifier value_mismatch — expected '<shaped-not-asserted>' (shape assertion), got 'cluster-endpoint-NDXIJTISLM29VSANPW4G0'.
3. fixture neptune-db-cluster-lifecycle-and-custom-endpoint step task#10, DeleteDBClusterEndpoint, expected 200 / actual 200: $.DeleteDBClusterEndpointResponse.DeleteDBClusterEndpointResult.DBClusterEndpointResourceIdentifier value_mismatch — expected '<shaped-not-asserted>', got 'cluster-endpoint-NDXIJTISLM29VSANPW4G0'.

The cluster/endpoint names, region, account, token and port in the quoted records are this recording's values filling the forms described in the issue, not literal requirements.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1 (replay_wire_corpus).
2. Action=CreateDBCluster with Engine=neptune, then Action=CreateDBClusterEndpoint on it with EndpointType=READER (fixture neptune-db-cluster-lifecycle-and-custom-endpoint, step task#3) and read DBClusterEndpointResourceIdentifier from the response — Cause 2.
3. Action=DescribeDBClusterEndpoints with that DBClusterIdentifier — step task#5. Compare the list order (the CUSTOM record is served third, not first) and each entry's Endpoint value (all '127.0.0.1', with no writer/reader/custom distinction) — Cause 1.
4. Action=DeleteDBClusterEndpoint for that endpoint — step task#10 — and read DBClusterEndpointResourceIdentifier off the delete response — Cause 3.
5. Re-replay neptune-db-cluster-lifecycle-and-custom-endpoint (task#3, task#5, task#10) to confirm all three causes.
