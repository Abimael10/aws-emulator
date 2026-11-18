Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.aurora-writer-reader-endpoint-query-routing rds.describe-cluster-backtracks
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.aurora-writer-reader-endpoint-query-routing|run#23|AssertionError|assert writer_ep != reader_ep, (
Issue-Key: rds.describe-cluster-backtracks|run#13|AssertionError|assert message == "Could not find specified DB Cluster", message
Issue-Commit: 563d9546b2aa96bbba80dbdb1955fbc3f9af54b6

# rds: 2 net-new causes — DescribeDBClusters publishes an IDENTICAL writer and reader endpoint (read/write split unaddressable), and DescribeDBClusterBacktracks' DBClusterNotFoundFault message is emulator-authored

> _Note: this issue was identified against the code base at commit `563d9546b2aa96bbba80dbdb1955fbc3f9af54b6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters, DescribeDBClusterBacktracks

## Description
Two net-new root causes in the rds emulator, both grounded in failing behavioral checks under the `rds` heading. Every other failing rds check in this run was marked ALREADY FILED or OWNED by an open issue and is excluded. This issue bundles 2 causes; each must be closed before submitting.

=== Cause 1 of 2: an Aurora cluster's Endpoint and ReaderEndpoint are the SAME value, so the writer and the reader endpoint cannot be told apart at all ===
AWS: Smithy (com.amazonaws.rds#AmazonRDSv19) models how a cluster is reached with separate members on DBCluster: `Endpoint` (the writer/primary connection endpoint), `ReaderEndpoint` — documented as "The reader endpoint for the DB cluster. The reader endpoint for a DB cluster load-balances connections across the Aurora Replicas that are available in a DB cluster" — `Port` (an Integer, the single port both endpoints are reached on) and `HostedZoneId` (the Route 53 zone the endpoint names live in). Both endpoint members carry a DNS host NAME only, never an embedded port. Aurora mints the two names from the cluster identifier over ONE shared DNS suffix: the writer as `<DBClusterIdentifier>.cluster-<suffix>` and the reader as `<DBClusterIdentifier>.cluster-ro-<the same suffix>`. The identifier-derived prefix, the discriminating `cluster-` vs `cluster-ro-` infix, and above all the fact that the two values are DISTINCT are the requirement; the concrete suffix, zone, address and port of any one environment are not.
Emulator: for a freshly available Aurora cluster, DescribeDBClusters returns `Endpoint` and `ReaderEndpoint` as the identical bare address (the replay observed both as '127.0.0.1' — the literal address is environment-specific and is NOT the finding). There is no cluster-identifier prefix, no shared `cluster-<suffix>` domain, no `cluster-ro-` infix, and the two members are byte-for-byte equal, so a client that compares them cannot even detect that a read path exists.
Why: read-scaling is unaddressable. A client that sends reads to ReaderEndpoint and writes to Endpoint silently sends both to the same target; code that derives the reader host from the writer host (the documented Aurora pattern), that splits the DNS suffix, or that pairs the host with HostedZoneId gets nothing usable; a reader-endpoint read-only enforcement test cannot be expressed because there is no second address to connect to.
Fix: the Aurora cluster endpoint projection in the rds crate — where CreateDBCluster / RestoreDBClusterFromSnapshot mint a cluster's endpoints into state and where DescribeDBClusters serializes DBCluster.Endpoint / ReaderEndpoint / Port / HostedZoneId. The served address must stay reachable, but the two endpoint names must be distinct and derived from the cluster identifier with the `cluster-` and `cluster-ro-` infixes over one shared suffix, with the port reported only in Port.
Related (does NOT cover this check): open issue 20261111T194847Z-rds-7-net-new-causes-cluster-writer-reader.md, Cause 1, describes the same representation defect but carries only the replay key of rds.aurora-cluster-writer-and-reader-endpoints-share-cluster-domain (where ReaderEndpoint was still a distinct 'host:port' literal); and 20261111T060457Z-rds-secretsmanager-describedbclusters-returns-a-literal-host-port-in-readerendpoint.md carries only an rds__secretsmanager key. Neither carries this check's key, and this grounding is now stronger: the two members are no longer merely mis-shaped, they are equal.
Traces: rds.aurora-writer-reader-endpoint-query-routing (mismatch).

=== Cause 2 of 2: DescribeDBClusterBacktracks' not-found refusal carries an emulator-authored message instead of AWS's DBClusterNotFoundFault text ===
AWS: DescribeDBClusterBacktracks validates the parent cluster before listing. For a DBClusterIdentifier that names no cluster it refuses with HTTP 404, error code `DBClusterNotFoundFault` (Smithy: DescribeDBClusterBacktracks errors = DBClusterBacktrackNotFoundFault, DBClusterNotFoundFault; both httpError 404, awsQueryError code DBClusterNotFoundFault / httpResponseCode 404; shape doc: "DBClusterIdentifier doesn't refer to an existing DB cluster."), and the message is AWS's fixed sentence `Could not find specified DB Cluster` — the whole message is fixed text for this error class and interpolates NO value from the request. The status (404) and the code (DBClusterNotFoundFault) are correct in the emulator; only the message text diverges.
Emulator: the refusal's message is `DBCluster <identifier> not found.` — emulator-authored prose that also interpolates the requested identifier, where AWS interpolates nothing. (The identifier in the replay, 'sim2real-behav-clu-missing-b4b3a7beb1cb', is a per-run value supplied by the check and is not part of the requirement; the requirement is the fixed sentence.) The positive leg of the same check — an empty but successful DBClusterBacktracks list for a fresh aurora-mysql cluster with no backtrack history — passed, so only the refusal message diverges.
Why: clients, CLIs and test suites that match on the refusal message (and operators reading it) see text AWS never emits; any assertion or log-scrape keyed on "Could not find specified DB Cluster" fails against the emulator even though the code and status are right.
Fix: the DescribeDBClusterBacktracks handler's cluster-existence validation in the rds crate — the DBClusterNotFoundFault it raises must carry AWS's fixed message, with no identifier interpolated.
Traces: rds.describe-cluster-backtracks (mismatch).

## Evidence
Behavioral suite simparity-behavioral-aws-v1.

Cause 1 — [mismatch] rds.aurora-writer-reader-endpoint-query-routing:
  File "check_rds_aurora_writer_reader_endpoint_query_routing.py", line 299, in run
    assert writer_ep != reader_ep, (
  AssertionError: Endpoint and ReaderEndpoint must be different hostnames — every read-scaling client relies on the split; both are '127.0.0.1'
  Expected (AWS): DescribeDBClusters.DBClusters[0].Endpoint == "<cluster-id>.cluster-<suffix>" and ReaderEndpoint == "<cluster-id>.cluster-ro-<same suffix>" — two DISTINCT, host-only DNS names, port only in the Integer member Port, zone in HostedZoneId.
  Actual (emulator): Endpoint == ReaderEndpoint == a bare address with no identifier prefix and no cluster-ro- infix (observed '127.0.0.1' for both; the literal address is environment-specific and not the requirement). HTTP 200 — no status or error-code divergence here, only representation.

Cause 2 — [mismatch] rds.describe-cluster-backtracks:
  File "check_rds_describe_cluster_backtracks.py", line 89, in run
    assert message == "Could not find specified DB Cluster", message
  AssertionError: DBCluster sim2real-behav-clu-missing-b4b3a7beb1cb not found.
  Expected (AWS): HTTP 404, Error.Code "DBClusterNotFoundFault", Error.Message exactly "Could not find specified DB Cluster" (entirely fixed text; no request value interpolated).
  Actual (emulator): HTTP 404 with Error.Code "DBClusterNotFoundFault" (both correct) but Error.Message "DBCluster <requested-identifier> not found." — non-AWS wording plus an interpolated identifier (the identifier shown is the check's per-run name). The preceding assertion on the same check (code == "DBClusterNotFoundFault") and the empty-list positive leg both passed.

## Steps to Reproduce
Replay the behavioral suite (simparity-behavioral-aws-v1) against the emulator and re-run exactly these two checks:

1. rds.aurora-writer-reader-endpoint-query-routing — CreateDBSubnetGroup over the run's network, CreateDBCluster(Engine='aurora-mysql'/'aurora-postgresql') plus a writer and a reader instance, poll DescribeDBClusters until Status == 'available', then read DBClusters[0].Endpoint and DBClusters[0].ReaderEndpoint. Expected: two different host-only names, '<cluster-id>.cluster-<suffix>' and '<cluster-id>.cluster-ro-<same suffix>', with the port only in Port. Actual: the two members are the identical bare address, so the read/write split cannot be addressed and the routing legs of the check are never reached.

2. rds.describe-cluster-backtracks — CreateDBCluster(Engine='aurora-mysql', BackupRetentionPeriod=1) over a DB subnet group, wait for 'available', call DescribeDBClusterBacktracks(DBClusterIdentifier=<that cluster>) and confirm DBClusterBacktracks == [] (this passes), then call DescribeDBClusterBacktracks(DBClusterIdentifier=<an identifier no cluster has>). Expected: HTTP 404, code DBClusterNotFoundFault, message "Could not find specified DB Cluster". Actual: same status and code, message "DBCluster <identifier> not found."
