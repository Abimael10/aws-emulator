Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.cli-rds-import-from-s3-aurora-workflow rds__s3.rds-load-from-s3-missing-role-negative rds__s3.rds-load-from-s3-role-association-positive rds__s3.rds-mysql-load-data-from-s3-missing-role-negative rds__s3.rds-mysql-load-data-from-s3-positive
Issue-Key: rds__s3.cli-rds-import-from-s3-aurora-workflow|run#25|PublishedUrlRejected|endpoint, port = ctx.endpoint_host(cluster["Endpoint"], cluster["Port"])
Issue-Key: rds__s3.rds-load-from-s3-missing-role-negative|run#12|PublishedUrlRejected|endpoint_dial, port_dial = ctx.endpoint_host(endpoint, int(port))
Issue-Key: rds__s3.rds-load-from-s3-role-association-positive|_load_from_s3_and_read_back#13|TimeoutError|raise TimeoutError(f"MySQL connection to {endpoint}:{port}: the run budget ended first; not graded")
Issue-Key: rds__s3.rds-mysql-load-data-from-s3-missing-role-negative|_mysql_load_from_s3#15|PublishedUrlRejected|host, port = ctx.endpoint_host(host, port)
Issue-Key: rds__s3.rds-mysql-load-data-from-s3-positive|_mysql_load_from_s3#15|PublishedUrlRejected|host_dial, port_dial = ctx.endpoint_host(host, int(port))
Issue-Commit: def798b9c9a5a532a0598d5d900881d6a843c290

# rds__s3: an available Aurora cluster publishes its writer Endpoint on an unserved '<id>.cluster-<hash>.<region>.rds.localhost' host, so every LOAD-DATA-FROM-S3 / import scenario is ungradeable

> _Note: this issue was identified against the code base at commit `def798b9c9a5a532a0598d5d900881d6a843c290`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters

## Description
1 cause, grounded in 5 failing checks.

Cause 1 of 1: the DB cluster endpoint address the RDS control plane publishes is a host the emulator does not serve and does not answer on, so no client can open a MySQL session against a cluster the emulator reports as available.

AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) documents `DBCluster$Endpoint` as "The connection endpoint for the primary instance of the DB cluster." It is the address a client connects to; for an Aurora cluster AWS's form is `<cluster-id>.cluster-<12-char-token>.<region>.rds.amazonaws.com` on the cluster's `Port`, and a TCP connect to it reaches the engine as soon as `Status` is `available`. For an emulator the requirement is the same contract in the emulator's own vocabulary: the published host must be an address the emulator itself answers on (its own host[:port], a documented own host, or a loopback address for a loopback endpoint) and the engine must accept a connection there.

Emulator: `DescribeDBClusters` / `CreateDBCluster` publish `Endpoint` as a synthesized AWS-shaped name whose domain is `.rds.localhost` — the general form is `<cluster-identifier>.cluster-<random-token>.<region>.rds.localhost` paired with an ephemeral port (e.g. the recordings show `...cluster-xw6oowu4w3sf.us-west-2.rds.localhost:33865`; the identifier, token and port differ per run and are not part of the requirement). That host is not one of the emulator's own hosts: the replay client refuses it with `simparity.published.PublishedUrlRejected: endpoint_host was given <host>:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address`. Where a check does dial it anyway, nothing answers and the connect attempt consumes the entire run budget: `TimeoutError: MySQL connection to <host>:<port>: the run budget ended first; not graded`.

Why: a client that reads `Endpoint`/`Port` off an available cluster and connects — which is the only documented way to use the cluster — can never reach the engine. Every scenario built on a live SQL session is lost: `LOAD DATA FROM S3`, `LOAD XML FROM S3`, the aurora_load_from_s3 role-association happy path, and the negative variants that must observe the engine refusing the statement when no role is associated. The emulator reports success at the control plane while the data plane it advertised is unreachable, so the IAM-role-to-S3 integration cannot be exercised at all.

Fix: the rds crate's endpoint publication — the component that fills `DBCluster.Endpoint`/`ReaderEndpoint`/`Port` when a cluster is created and when it is described — must announce an address the emulator actually serves the engine on, and the engine listener must accept connections there for as long as the cluster reports `available`. Note the already-open issues 20261121T105515Z-rds-5-net-new-behavioral-causes-aurora-cluster.md, 20261121T053640Z-logs-rds-an-available-aurora-postgresql-cluster-publishes.md, 20261123T041052Z-rds-4-net-new-endpoint-publication-causes-a.md and 20261125T130223Z-rds-secretsmanager-a-cloudformation-deployed-aurora-cluster-publishes.md describe the same publication seam for other service pairs; the replay suite reports none of them as owning these five rds__s3 checks, which are filed here.

Traces: rds__s3.cli-rds-import-from-s3-aurora-workflow; rds__s3.rds-load-from-s3-missing-role-negative; rds__s3.rds-load-from-s3-role-association-positive; rds__s3.rds-mysql-load-data-from-s3-missing-role-negative; rds__s3.rds-mysql-load-data-from-s3-positive.

## Evidence
Expected (AWS): DBCluster.Endpoint is a connectable address for the cluster's primary instance; a TCP/MySQL connect to Endpoint:Port on an `available` cluster is accepted.

Actual (emulator), 4 checks — the published host is refused as not the emulator's own:
  [mismatch] rds__s3.cli-rds-import-from-s3-aurora-workflow
    simparity.published.PublishedUrlRejected: endpoint_host was given sim2real-aurora-5f92d2e2926a.cluster-xw6oowu4w3sf.us-west-2.rds.localhost:33865 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address
  [mismatch] rds__s3.rds-load-from-s3-missing-role-negative
    ... endpoint_host was given sim2real-aur-norole-cluster-0b5f08e33dee.cluster-u57u7sghmggl.us-west-2.rds.localhost:46549 ... its host is not one of the emulator's own hosts ...
  [mismatch] rds__s3.rds-mysql-load-data-from-s3-missing-role-negative
    ... endpoint_host was given sim2real-aurora-mysql-da67367e68f0.cluster-9uxqkf8x2933.us-west-2.rds.localhost:36993 ... its host is not one of the emulator's own hosts ...
  [mismatch] rds__s3.rds-mysql-load-data-from-s3-positive
    ... endpoint_host was given sim2real-aurora-mysql-d176760384a4.cluster-x61hxtsputzu.us-west-2.rds.localhost:32823 ... its host is not one of the emulator's own hosts ...

and 1 check that dialed it and reached nothing:
  [error] rds__s3.rds-load-from-s3-role-association-positive
    TimeoutError: MySQL connection to sim2real-aur-cluster-16598fb35805.cluster-29ip9y05ueqo.us-west-2.rds.localhost:35793: the run budget ended first; not graded

(The cluster identifiers, cluster- tokens and ports above are per-run values filling the template `<cluster-identifier>.cluster-<token>.<region>.rds.localhost:<ephemeral-port>`; the fixed, diverging part is the `.rds.localhost` host the emulator does not serve.)

## Steps to Reproduce
1. CreateDBCluster for an aurora-mysql cluster plus a writer instance; wait until DescribeDBClusters reports Status=available.
2. Read DBCluster.Endpoint and DBCluster.Port from the response.
3. Observe the host is `<cluster-id>.cluster-<token>.<region>.rds.localhost` — not an address the emulator serves — and that a MySQL connect to Endpoint:Port never completes.
4. Replay checks: rds__s3.cli-rds-import-from-s3-aurora-workflow, rds__s3.rds-load-from-s3-missing-role-negative, rds__s3.rds-load-from-s3-role-association-positive, rds__s3.rds-mysql-load-data-from-s3-missing-role-negative, rds__s3.rds-mysql-load-data-from-s3-positive
