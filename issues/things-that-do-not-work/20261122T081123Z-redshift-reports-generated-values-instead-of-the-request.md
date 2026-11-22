Issue-Source: trace-sweep-divergence
Issue-Trace: redshift-aqua-configuration-modify-transition
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: redshift-aqua-configuration-modify-transition redshift-create-cluster-lifecycle redshift-snapshot-copy-grant-and-authentication-profile
Issue-Shape-Id: com.amazonaws.redshift#RedshiftServiceVersion20121201
Issue-Key: redshift|DescribeClusters|2xx->2xx|$.DescribeClustersResponse.DescribeClustersResult.Clusters.Cluster.Endpoint.Port
Issue-Key: redshift|CreateSnapshotCopyGrant|2xx->2xx|$.CreateSnapshotCopyGrantResponse.CreateSnapshotCopyGrantResult.SnapshotCopyGrant.KmsKeyId
Issue-Commit: b38efef85af881a9c357f09f754c4d7783936268

# Redshift reports generated values instead of the request's configured ones for cluster Endpoint.Port and snapshot-copy-grant KmsKeyId

> _Note: this issue was identified against the code base at commit `b38efef85af881a9c357f09f754c4d7783936268`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeClusters

## Description
2 net-new causes in the redshift emulator, sharing one fault: a response field that AWS populates from the creating request (or from that field's documented default) is instead filled with a value the emulator invented at creation time, so the describe/create response does not reflect what the client asked for.

Cause 1 of 2: DescribeClusters reports an arbitrary allocated port in Cluster.Endpoint.Port instead of the cluster's configured database port.
  AWS: Smithy: Redshift Endpoint.Port -> Integer, "The port that the database engine is listening on"; CreateClusterMessage.Port -> "The port number on which the cluster accepts incoming connections. Default: 5439. Valid Values: for clusters with RG or RA3 nodes select a port within 5431-5455 or 8191-8215; for dc2 nodes within 1150-65535." The port a cluster reports is therefore the Port member of the CreateCluster (or RestoreFromClusterSnapshot) request that made it, and 5439 when the request omitted Port. It is a property of the cluster's configuration, not of the host the API happens to run on, and it is stable across every DescribeClusters of the same cluster.
  Emulator: for a cluster created without a Port member, DescribeClusters answers 200 with $.DescribeClustersResponse.DescribeClustersResult.Clusters.Cluster.Endpoint.Port set to a high ephemeral port the emulator allocated per cluster (the recording shows 38597 where the default 5439 was expected). The value is not derived from the request and is not the documented default.
  Why: a client builds its JDBC/ODBC connection string from Endpoint.Address plus Endpoint.Port, and asserts the port it configured. It gets a port it never asked for, outside the valid ranges the API documents for the node type, so connection strings synthesized from the describe output are wrong and any check that the default port is 5439 (or that an explicitly requested port was honored) fails.
  Fix: the cluster state/creation path that materializes the Endpoint structure must persist the request's Port (defaulting to 5439 when absent) and the DescribeClusters serializer must emit that stored value, rather than reporting whatever port was allocated for the emulator's own listener.
  Traces: redshift-aqua-configuration-modify-transition#task#3, redshift-aqua-configuration-modify-transition#task#5, redshift-create-cluster-lifecycle#task#2.

Cause 2 of 2: CreateSnapshotCopyGrant does not echo the request's KmsKeyId; it returns a newly generated key id.
  AWS: Smithy: CreateSnapshotCopyGrantMessage.KmsKeyId -> String, "The unique identifier of the encrypted symmetric key to which to grant Amazon Redshift permission. If no key is specified, the default key is used."; the response shape SnapshotCopyGrant.KmsKeyId -> "The unique identifier of the encrypted symmetric key in Amazon Web Services KMS to which Amazon Redshift is granted permission." The grant therefore reports the KMS key the request named (and the account's default Redshift KMS key when the request omitted it); the identifier is never minted by Redshift.
  Emulator: CreateSnapshotCopyGrant answers 200 but $.CreateSnapshotCopyGrantResponse.CreateSnapshotCopyGrantResult.SnapshotCopyGrant.KmsKeyId carries a fresh emulator-generated UUID unrelated to the key id supplied in the request (the fixture asserts the key id it created and captured earlier in the same run; the emulator returned an unrelated uuid instead).
  Why: a client that creates a KMS key, creates a snapshot copy grant for it, and then uses the grant for cross-region snapshot copy cannot correlate the grant with its key: the returned KmsKeyId names a key that does not exist, so key-to-grant assertions fail and subsequent DescribeSnapshotCopyGrants-based lookups by key id find nothing.
  Fix: the CreateSnapshotCopyGrant handler must copy the request's KmsKeyId into the stored grant and into the serialized SnapshotCopyGrant (falling back to the account's default Redshift KMS key id when the member is absent) instead of generating an identifier.
  Traces: redshift-snapshot-copy-grant-and-authentication-profile#task#1.

Neither cause is covered by an existing open issue: the open redshift issues concern static reference catalogs (DescribeEventCategories / DescribeReservedNodeOfferings), not-found refusal wording, CreateCluster validation, and snapshot refusal/batch error shapes.

## Evidence
Replay of trace set simparity-wire-aws-v1 (51 fixtures, 353 steps, 34 mismatched).

Cause 1 — fixture redshift-aqua-configuration-modify-transition step task#3 (DescribeClusters), expected status 200, actual 200:
  $.DescribeClustersResponse.DescribeClustersResult.Clusters.Cluster.Endpoint.Port: value_mismatch (expected '5439', got '38597')
  Same cluster also at redshift-aqua-configuration-modify-transition#task#5 and redshift-create-cluster-lifecycle#task#2.
  Expected per model: the Port given at CreateCluster, or the documented default 5439 when omitted (the recorded 5439 is that default, not a literal requirement of one run); 38597 is an emulator-allocated port.

Cause 2 — fixture redshift-snapshot-copy-grant-and-authentication-profile step task#1 (CreateSnapshotCopyGrant), expected status 200, actual 200:
  $.CreateSnapshotCopyGrantResponse.CreateSnapshotCopyGrantResult.SnapshotCopyGrant.KmsKeyId: value_mismatch (expected '{{copy_grant_kms_key_id}}' — the KMS key id the fixture created and passed in the request — got '4087de7b-3835-4055-a71a-2e96449c94f9', an unrelated generated uuid).

## Steps to Reproduce
Cause 1:
1. CreateCluster with a ClusterIdentifier and no Port member (as fixture redshift-create-cluster-lifecycle / redshift-aqua-configuration-modify-transition do).
2. Wait for the cluster to report an Endpoint, then call DescribeClusters for that ClusterIdentifier.
3. Read Clusters.Cluster.Endpoint.Port: the emulator returns an allocated ephemeral port; AWS returns 5439 (the documented default). Repeat with an explicit Port (e.g. 5440) and observe the requested value is likewise not reported.

Cause 2:
1. Create a KMS key and note its key id.
2. Call CreateSnapshotCopyGrant with SnapshotCopyGrantName=<name> and KmsKeyId=<that key id>.
3. Read CreateSnapshotCopyGrantResult.SnapshotCopyGrant.KmsKeyId: the emulator returns a freshly generated uuid instead of the key id from the request.

Replay to confirm: redshift-aqua-configuration-modify-transition#task#3 and redshift-snapshot-copy-grant-and-authentication-profile#task#1.
