Issue-Source: trace-sweep-divergence
Issue-Trace: rds-aurora-serverless-data-api-managed-secret-sql-round-trip
Issue-Step: teardown#17
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-aurora-serverless-data-api-managed-secret-sql-round-trip
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: secretsmanager|DescribeSecret|4xx->2xx|$
Issue-Related: 20261109T205028Z-secretsmanager-an-rds-managed-master-user-secret-outlives.md
Issue-Commit: 51f17b774124d07cc1970a907a5af4e8c96232ea

# secretsmanager: DescribeSecret still answers 200 for the managed master user secret of an already-deleted DB cluster

> _Note: this issue was identified against the code base at commit `51f17b774124d07cc1970a907a5af4e8c96232ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSecret

## Description
One cause. The replay reported no blocking stub and no owning issue for this step.

Cause 1 of 1: after the owning Aurora DB cluster is deleted, DescribeSecret on its RDS-managed master user secret keeps answering HTTP 200 with a live secret record instead of refusing the lookup.

AWS: Smithy (com.amazonaws.secretsmanager#secretsmanager) models DescribeSecret with input member SecretId (required, @length min 1 max 2048) and the client error ResourceNotFoundException at HTTP 400 for a secret Secrets Manager cannot resolve. An RDS-managed master user secret is service-owned: its DescribeSecret projection carries OwningService 'rds', a name of the form 'rds!cluster-<cluster-resource-id>', and the tag aws:rds:primaryDBClusterArn naming the owning cluster. Its lifecycle belongs to RDS, not to the caller: when the owning DB cluster is deleted, RDS removes the managed secret, after which DescribeSecret on that secret's ARN or name is refused with ResourceNotFoundException at HTTP 400. The message for that error class is the fixed text "Secrets Manager can't find the specified secret." with no variable parts; everything else in the exchange (ARN suffix, cluster resource id, version id, timestamps, request id) is a per-run value, not a requirement.

Emulator: the step polls DescribeSecret with the SecretArn the cluster reported, after DeleteDBCluster has completed. Every one of the 19 attempts over the 180 s budget answered HTTP 200 carrying the full record (ARN, 'rds!cluster-...' Name, OwningService 'rds', RotationEnabled false, Tags [{Key 'aws:rds:primaryDBClusterArn', Value '<arn of the already-deleted cluster>'}], VersionIdsToStages {<version>: ['AWSCURRENT']}). ResourceNotFoundException never appeared, so the cluster delete never cascades to the secret RDS owns and the lookup gate never refuses it.

Why: a client cannot tell whether an RDS-managed secret is still live. Teardown logic that waits for the managed secret to disappear hangs until its own timeout and then reports failure; re-creating a cluster with the same identifier collides with a stale 'rds!cluster-...' secret AWS would have reaped; and master credentials for a destroyed cluster remain readable, which inverts the deletion guarantee callers depend on.

Fix: two components. The RDS DeleteDBCluster completion path in rds state must cascade to the managed master user secret it owns and remove it; the secretsmanager DescribeSecret handler's lookup gate must then refuse an absent secret with ResourceNotFoundException at HTTP 400 and AWS's fixed message rather than projecting a retained record.

Related open issue: 20261109T205028Z-secretsmanager-an-rds-managed-master-user-secret-outlives.md describes the same symptom but is not reported as owning this step's replay key, so this divergence is still live in the current replay (score 54.3%). Open issues 20261107T030257Z-rds-secretsmanager-2-net-new-causes-the-rds.md, 20261108T102113Z-rds-secretsmanager-the-rds-managed-master-user-secret.md and 20261114T104717Z-rds-secretsmanager-2-net-new-causes-describesecret-invents.md concern other aspects of the managed secret (its name, its rotation reporting) and do not cover its deletion.

Traces: rds-aurora-serverless-data-api-managed-secret-sql-round-trip#teardown#17.

## Evidence
Replay of trace set simparity-wire-aws-v1 (99 fixtures, 993 steps, 539 passed, score 54.3%).

Fixture rds-aurora-serverless-data-api-managed-secret-sql-round-trip, step teardown#17, secretsmanager DescribeSecret:
  status: expected 400, got 200
  $: poll: error code None not in ['ResourceNotFoundException'] after 19 attempt(s)
  $: poll capped at 180s — the emulator returned the live secret record on every attempt, e.g. {'ARN': 'arn:aws:secretsmanager:us-west-2:000000000000:secret:rds!cluster-cluster-<id>-<suffix>', 'Name': 'rds!cluster-cluster-<id>', 'RotationEnabled': False, 'Tags': [{'Key': 'aws:rds:primaryDBClusterArn', 'Value': '<arn of the deleted cluster>'}], 'VersionIdsToStages': {'<version>': ['AWSCURRENT']}, 'OwningService': 'rds'}

Expected: HTTP 400 ResourceNotFoundException, fixed message "Secrets Manager can't find the specified secret.", once the owning cluster's deletion has completed.
Actual: HTTP 200 with the secret record, indefinitely.
The ARN suffix, cluster resource id, version id, timestamps and request id in the recorded record are per-run values; the HTTP status, the error code and that fixed message text are the requirement.

## Steps to Reproduce
1. git_clone(), then replay_wire_corpus() against the emulator built from that checkout.
2. Replay fixture rds-aurora-serverless-data-api-managed-secret-sql-round-trip: create an Aurora Serverless cluster with ManageMasterUserPassword=true, record DBCluster.MasterUserSecret.SecretArn, run the Data API round trip, then DeleteDBCluster and wait until the cluster is gone.
3. Step teardown#17: poll secretsmanager DescribeSecret with that SecretArn. The emulator answers HTTP 200 with the full secret record on every attempt for the whole 180 s budget; AWS answers HTTP 400 ResourceNotFoundException once RDS has reaped the managed secret.
