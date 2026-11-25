Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle
Issue-Key: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle|phase#4|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 22c0b68190b6d53f08de46118af925b6725f9997

# rds__secretsmanager: a CloudFormation-deployed Aurora cluster publishes ReaderEndpoint as the bare host 'localhost', with no <cluster-id>.cluster-ro- infix

> _Note: this issue was identified against the code base at commit `22c0b68190b6d53f08de46118af925b6725f9997`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters

## Description
Cause 1 of 1: the ReaderEndpoint of a CloudFormation-deployed Aurora Serverless v2 cluster is published as a bare emulator host instead of a reader DNS name derived from the cluster identifier.

AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) DBCluster carries Endpoint (the writer connection endpoint), ReaderEndpoint — documented "The reader endpoint for the DB cluster. The reader endpoint for a DB cluster load-balances connections across the Aurora Replicas that are available in a DB cluster" — Port (the integer port both are reached on) and HostedZoneId. Aurora mints the two names from the cluster identifier and one shared DNS suffix: the writer as <DBClusterIdentifier>.cluster-<suffix> and the reader as <DBClusterIdentifier>.cluster-ro-<suffix>. The identifier-derived prefix and the discriminating 'cluster-ro-' infix are the requirement; the concrete suffix, zone and port of any one environment are not, and an emulator may keep the address reachable as long as the published name carries that structure.

Emulator: DescribeDBClusters on the cluster the check's CloudFormation stack deploys returns ReaderEndpoint = 'localhost' — a bare host with no cluster identifier, no 'cluster-ro-' infix and no DNS suffix. The check's production-readiness audit fails in its 'solve' phase on exactly that field, after the stack has deployed and the cluster is readable.

Why: the 'cluster-' vs 'cluster-ro-' infix is the only thing in the response that tells a writer endpoint from a reader endpoint. A client, DSN builder or reviewer that derives or validates the reader endpoint from the cluster identifier (or that pairs the name with HostedZoneId, or splits its DNS suffix) sees a value with no structure at all, and code that routes reads to the reader and writes to the writer can no longer tell that it is sending both to the same place.

Fix: the cluster-endpoint representation in the RDS cluster state and the DescribeDBClusters serializer — mint Endpoint/ReaderEndpoint from the cluster identifier with the 'cluster-' / 'cluster-ro-' infixes over one shared suffix for every creation path, including the CloudFormation AWS::RDS::DBCluster provider, and report the port only in Port.

Related open issue that does NOT carry this check's replay key: 20261111T060457Z-rds-secretsmanager-describedbclusters-returns-a-literal-host-port-in-readerendpoint.md describes the same missing structure observed as a '<host>:<port>' literal on a directly created cluster; the replay report did not mark this check OWNED by it, and this check reaches the field through the CloudFormation-deployed Aurora Serverless v2 stack and observes the bare host 'localhost'. A fixer closing the two together must make both paths publish the identifier-derived names.

Traces: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle.

## Evidence
Check rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle [mismatch]; the 'deploy' phase succeeded and the audited trajectory (rds.DescribeDBClusters, cloudformation.GetTemplate, secretsmanager.DescribeSecret) ran, so only the endpoint representation diverged:
  File "check_rds__secretsmanager_refarch_aur_e2e_review_aurora_production_readiness_lifecycle.py", line 231, in phase
    raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
  AssertionError: [phase 2/4 solve] the reader endpoint must carry the cluster-ro- infix, got 'localhost'

Expected (AWS): DescribeDBClusters.DBClusters[0].ReaderEndpoint is a DNS host name beginning '<DBClusterIdentifier>.cluster-ro-', sharing its suffix with Endpoint ('<DBClusterIdentifier>.cluster-<suffix>'), with the port reported only in the integer member Port.
Actual (emulator): ReaderEndpoint == 'localhost'. The cluster identifier is a per-run generated value; the requirement is the identifier-derived prefix and the 'cluster-ro-' infix, not any particular suffix or port.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite against the booted emulator.
2. Deploy the check's cfn-aurora-serverless stack (AWS::RDS::DBCluster, aurora-postgresql Serverless v2, with a Secrets Manager managed master secret) and wait for CREATE_COMPLETE.
3. Call rds.describe_db_clusters(DBClusterIdentifier=<the stack's cluster id>) and read DBClusters[0].ReaderEndpoint, Endpoint and Port.
4. Observe ReaderEndpoint == 'localhost' — no '<cluster-id>.cluster-ro-' prefix and no shared DNS suffix with Endpoint — where AWS publishes '<cluster-id>.cluster-ro-<suffix>'.
5. Replay rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle to confirm a fix.
