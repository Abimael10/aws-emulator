Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pi.describe-dimension-keys-sorted-by-load-descending
Issue-Key: pi.describe-dimension-keys-sorted-by-load-descending|_lock_contention#20|OperationalError|setup = pymysql.connect(
Issue-Commit: 2a533320acecc5e3d5510f43fe45b8adcca9ff2d

# pi, rds: the replay client cannot dial the DB instance endpoint the control plane publishes, so no pi check that needs a real MySQL session can be graded

> _Split out of `issues/things-that-now-work/20261104T085941Z-pi-3-net-new-causes-the-db-metric.md`, whose cause 1 (the empty `db` metric catalog, and the TagResource refusals its positive-control poll was blocking) and cause 3 (`DescribeDimensionKeys` reporting no keys) are fixed. This report carries the ONE replay key of that issue that still does not pass, re-filed here so it is not recorded as working. Everything below is what that fix established about it; a later fixer starts from here rather than from scratch._

## Operation
DescribeDBInstances

## Description
The check `pi.describe-dimension-keys-sorted-by-load-descending` creates a PI-enabled MySQL DB
instance, reads `Endpoint.Address`/`Endpoint.Port` out of `DescribeDBInstances`, and opens a
MySQL session on that address to drive row-lock contention before it asks Performance Insights
for the top wait events. It dies at the connect:
`pymysql.err.OperationalError: (2003, "Can't connect to MySQL server on '127.0.0.1' ([Errno 111]
Connection refused)")`.

What the parent fix established, by building the whole flow in-process over REAL sockets:

* the endpoint a served engine publishes IS a listener that serves the wire. The cross-service
  suite `core/router/tests/all/pi_reports_the_rds_engine_session_load.rs` creates a DB instance
  through Amazon RDS, reads the published `Endpoint{Address,Port}`, opens two sessions on it over
  TCP from the same process, makes one transaction wait for a row the other holds, and then reads
  the load back through `pi.DescribeDimensionKeys` and `pi.GetResourceMetrics` — all inside two
  seconds. The RDS crate's own `tests/all/db_instance_engine_endpoint.rs` does the same over the
  MySQL protocol, handshake and all. So the engine, the listener and the published address agree:
  what the replayed check cannot do, an in-process client on the same address does.
* every replayed unit that needs a WORKING native session fails at the connect, while the units
  that need no session pass. The same signature is carried for the rds service by the open issue
  `20261118T181500Z-rds-the-published-endpoint-of-a-served-engine-is-the-core-listeners.md`
  (its cause 1 and cause 4), filed from the same observation.

Blocked by: `vendor/server/src/listeners.rs` — `bind_std`/`bind_tokio` bind every resource
listener on the ONE operator-chosen interface (loopback by default) at an OS-assigned port, and
`ResourceListener::host()`/`port()` is the only address a service can publish for it. A service
crate cannot bind anywhere else, cannot ask for the engine's own port, and cannot publish a name
the replay client's own host resolves, so the address the control plane publishes
(`127.0.0.1:<OS-assigned port>`) is reachable from inside this process and from a client on the
same host only.

REQUIRED CORE CHANGE: let a service open a resource listener at a requested port and on an
address a client outside this process can dial (a per-resource loopback alias, a published host
name, or a forwarded port on the emulator's own interface), and report that address back through
`ResourceListener`. Then the control plane can publish an endpoint the replay client reaches, and
this check — and every other data-plane check that dies at its connect — can be graded.

What is NOT missing, and should not be re-done: the Performance Insights side of this check.
`DescribeDimensionKeys` now reports the dimension keys of the activity the engine recorded,
ordered by `Total` descending, with each key's `Dimensions` and a float `Total`
(`services/service-com-amazonaws-pi-performanceinsightsv20180227/src/ops/describe_dimension_keys.rs`,
`src/state/activity.rs`), fed by the samples the RDS data plane records as each statement
finishes (`services/service-com-amazonaws-rds-amazonrdsv19/src/state/data_plane/activity_samples.rs`,
`services/common/src/db_activity_samples.rs`). The suites above pin both halves; what the replay
cannot reach is the session that produces the load.

## Evidence
[error] pi.describe-dimension-keys-sorted-by-load-descending
  pymysql.err.OperationalError: (2003, "Can't connect to MySQL server on '127.0.0.1' ([Errno 111]
  Connection refused)")
  Expected (AWS): the Endpoint.Address/Port returned by DescribeDBInstances accepts a MySQL
  session; after concurrent row-lock + CPU load, DescribeDimensionKeys(Metric='db.load.avg',
  GroupBy={Group:'db.wait_event', Limit:10}) returns >=2 Keys, each with Dimensions and a float
  Total, with Totals sorted descending.
  Actual (emulator): the connect is refused from the replay client, so no load is generated and
  the pi calls under test are never made. The same address accepts a session, and the same
  assertions hold, from a client inside this process.

## Steps to Reproduce
1. `CreateDBInstance(Engine='mysql', DBInstanceClass='db.t3.medium', AllocatedStorage=20,
   EnablePerformanceInsights=True, DatabaseInsightsMode='advanced')`, poll to
   `DBInstanceStatus='available'`.
2. Read `Endpoint.Address` and `Endpoint.Port` from `DescribeDBInstances`. Observe
   `127.0.0.1:<OS-assigned port>`.
3. From the replay client, open a MySQL connection to that address — ECONNREFUSED.
4. From inside the emulator process, open a TCP connection to the same address and complete the
   MySQL handshake — it is served (see `tests/all/db_instance_engine_endpoint.rs`).
