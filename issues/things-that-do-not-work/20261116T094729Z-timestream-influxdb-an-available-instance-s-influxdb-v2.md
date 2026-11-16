Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: timestream-influxdb.native-influxdb-http-protocol-round-trip
Issue-Shape-Id: com.amazonaws.timestreaminfluxdb#AmazonTimestreamInfluxDB
Issue-Key: timestream-influxdb.native-influxdb-http-protocol-round-trip|_poll_until#27|Timeout|time.sleep(interval_s)
Issue-Commit: 6e5ce689b55e967f104715fc44ab6da89b3ec566

# timestream-influxdb: an AVAILABLE instance's InfluxDB v2 data plane refuses every Flux read — POST /api/v2/query answers 501 "not implemented" instead of 200 annotated CSV, so a write/read round trip can never complete

> _Note: this issue was identified against the code base at commit `6e5ce689b55e967f104715fc44ab6da89b3ec566`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDbInstance

## Description
One net-new root cause in the timestream-influxdb emulator, grounded in the one failing behavioral check that is not already owned by an open issue.

Cause 1 of 1: the native InfluxDB v2 HTTP API served at an AVAILABLE DB instance's published `endpoint`:`port` serves no read path at all — `POST /api/v2/query` is hard-wired to refuse with HTTP 501 — so points accepted by `POST /api/v2/write` can never be read back by any InfluxDB client.

AWS: recorded behavior only. The Smithy model of this service describes the CONTROL plane only (`CreateDbInstance`/`GetDbInstance` output `endpoint` (String) — "The endpoint used to connect to InfluxDB" — and `port` (Port, @range 1024..65535, @default 8086) — "The port number on which InfluxDB accepts connections"); it says nothing about the data plane, so the requirement here is what the recording shows. The recording shows that once the instance reports status AVAILABLE, the host in `endpoint` listening on `port` serves the InfluxDB v2 HTTP API as a complete round trip: `GET /health` -> 200 with a JSON body whose `status` is `pass`; `POST /api/v2/signin` with the instance's `username`/`password` as HTTP basic auth -> 204; `POST /api/v2/write?org=<organization>&bucket=<bucket>&precision=ns` with a line-protocol body -> 204; and then `POST /api/v2/query?org=<organization>` with a Flux body (`Content-Type: application/vnd.flux`, `Accept: application/csv`) -> HTTP 200 whose body is InfluxDB annotated CSV containing the measurement named by the query's `filter(fn: (r) => r._measurement == "<measurement>")` and the field values written for it inside the `range(start: ...)` window. A well-formed Flux query against an AVAILABLE instance is never answered with a 501 by Timestream for InfluxDB; the Flux read path is the normal way this service's data is consumed.

Emulator: the query route is a deliberate refusal. `POST /api/v2/query` authenticates and then unconditionally answers HTTP 501 with the InfluxDB error body `{"code": "not implemented", "message": "this endpoint serves no Flux engine, so a Flux query cannot be evaluated here; points written with /api/v2/write are stored and can be read back through the emulator's own state"}`. The status 501, the error `code` "not implemented" and that whole message are fixed emulator text (no part of them is taken from the request or the state); AWS answers this request class 200 with a CSV body whose variable parts are the measurement name, tag keys/values, field key and field value from the written points and the window named by the request's Flux. The other legs of the plane are served (`/health` reports `pass` for an AVAILABLE instance, `/api/v2/signin` answers 204, `/api/v2/write` parses line protocol and stores the points, answering 204), so the data exists in the instance's store and only the READ path is refused. Because the check polls `POST /api/v2/query` for a 200 containing the measurement, the refusal never converges and the whole check exhausts its budget — the replay reports `Check timed out after 60.0s` rather than an assertion, which is the shape this refusal takes under a polling reader.

Why: the entire data plane of Timestream for InfluxDB is write-only, which makes it unusable. A client that creates an instance, waits for AVAILABLE, signs in, writes points and then queries them back — the normal use of this service — gets a 501 on the only step that returns data, after every earlier step reported success. Worse than an outright unreachable endpoint, this looks like a healthy instance (`/health` = `pass`, writes = 204) that silently has no readable data, and a client polling or retrying the read (as InfluxDB clients and dashboards do) hangs or spins until its own deadline instead of failing fast.

Fix: the InfluxDB v2 data-plane query route of this service — the `query` route handler of the per-instance native HTTP surface, and the constants that make it a refusal (the "not implemented" code/message pair) — must evaluate the Flux pipeline over the points the instance's own time-series store already holds (at minimum `from(bucket:) |> range(start:) |> filter(fn: (r) => r._measurement == ...)`, i.e. bucket selection, time-window filtering and predicate filtering on `_measurement`/tags/fields) and serialize the result as InfluxDB annotated CSV with HTTP 200 and a CSV content type, keeping the existing 401 for an unauthenticated request, 404 for an unknown organization/bucket and 503 while the instance is not AVAILABLE. File and line citations: services/service-com-amazonaws-timestreaminfluxdb-amazontimestreaminfluxdb/src/state/influx_data_plane/v2_api.rs lines 39-42 (`FLUX_UNSERVED`), 80 (the route binding) and 240-250 (`fn query`, which returns `error(NOT_IMPLEMENTED, CODE_NOT_IMPLEMENTED, FLUX_UNSERVED)`); the points it must read are in services/service-com-amazonaws-timestreaminfluxdb-amazontimestreaminfluxdb/src/state/influx_data_plane/time_series.rs, reached the same way `fn write` (same file, lines 177-238) reaches it.

Traces: timestream-influxdb.native-influxdb-http-protocol-round-trip.

Related but NOT covering: open issue 20261109T183400Z-timestream-influxdb-2-net-new-causes-createdbinstance-duplicate.md carries a replay key for this same check, but for an EARLIER failure mode (`PublishedUrlRejected`: the instance published a public `<name>.timestream-influxdb.<region>.on.aws` host the emulator did not serve). That address defect is no longer what the check hits — a per-instance listener is now opened and the published host is reachable, `/health`, `/api/v2/signin` and `/api/v2/write` are served — and the check now fails deterministically inside the plane on the query route, which that issue's key does not carry. Open issue 20261104T042504Z-timestream-influxdb-validationexception-constraint-violation-message-wording-diverges.md likewise only hypothesized a non-answering data plane and does not name this route.

## Evidence
Check: timestream-influxdb.native-influxdb-http-protocol-round-trip — verdict [mismatch], detail `Check timed out after 60.0s` (the check's final step is a poll of POST /api/v2/query for a 200 whose body contains the written measurement; the emulator's fixed 501 never satisfies the predicate, so the poll runs until the check's budget ends).

CreateDbInstance -> GetDbInstance AVAILABLE -> native InfluxDB v2 round trip at the published endpoint:port:
  GET /health                                                   expected 200 {"status":"pass"}   — emulator: 200 {"status":"pass"}  (OK)
  POST /api/v2/signin (basic auth username/password)            expected 204                     — emulator: 204 (OK)
  POST /api/v2/write?org=<org>&bucket=<bucket>&precision=ns     expected 204                     — emulator: 204 (OK)
  POST /api/v2/query?org=<org>  (Content-Type: application/vnd.flux, Accept: application/csv,
       body `from(bucket:"<bucket>") |> range(start: -5m) |> filter(fn: (r) => r._measurement == "<measurement>")`)
       expected: HTTP 200, annotated CSV containing <measurement> and the written field value
       actual:   HTTP 501, body {"code":"not implemented","message":"this endpoint serves no Flux engine, so a Flux query cannot be evaluated here; points written with /api/v2/write are stored and can be read back through the emulator's own state"}

That 501, the `code` "not implemented" and the whole message are fixed emulator text, not rendered from the request or the state: see v2_api.rs:245-250 (`fn query` -> `error(NOT_IMPLEMENTED, CODE_NOT_IMPLEMENTED, FLUX_UNSERVED)`), v2_api.rs:39-42 (`FLUX_UNSERVED`), v2_api.rs:423 (`NOT_IMPLEMENTED: u16 = 501`), v2_api.rs:435 (`CODE_NOT_IMPLEMENTED = "not implemented"`). The emulator's own unit test at v2_api.rs:754-781 asserts the 501 refusal, so this is intended behavior in the code and must change there. The measurement name, bucket name, field value and instance id in any one run are that run's values filling the templates above; the defect is that no Flux query is ever evaluated.

## Steps to Reproduce
1. Create a VPC with two private subnets and a security group admitting the client on port 8086.
2. Call CreateDbInstance(name=<N>, username=<U>, password=<8-64 chars matching ^[a-zA-Z0-9]+$>, organization=<ORG>, bucket=<BKT>, dbInstanceType="db.influx.medium", vpcSubnetIds=[<two subnets>], vpcSecurityGroupIds=[<sg>], allocatedStorage=20, dbStorageType="InfluxIOIncludedT1", deploymentType="SINGLE_AZ", publiclyAccessible=false, networkType="IPV4") and keep the returned `id`.
3. Poll GetDbInstance(identifier=<id>) until `status` is AVAILABLE; read `endpoint` and `port`.
4. GET http(s)://<endpoint>:<port>/health — expect 200 with {"status":"pass"} (emulator: OK).
5. POST /api/v2/signin with HTTP basic auth <U>/<password> on a session that keeps cookies — expect 204 (emulator: OK).
6. POST /api/v2/write?org=<ORG>&bucket=<BKT>&precision=ns with body `<measurement>,host=check value=42 <epoch_ns>` and Content-Type `text/plain; charset=utf-8` — expect 204 (emulator: OK).
7. POST /api/v2/query?org=<ORG> with Content-Type `application/vnd.flux`, Accept `application/csv` and body `from(bucket:"<BKT>") |> range(start: -5m) |> filter(fn: (r) => r._measurement == "<measurement>")`.
   EXPECTED: HTTP 200 with annotated CSV whose rows contain <measurement> and the field value 42.
   ACTUAL:   HTTP 501 with {"code":"not implemented","message":"this endpoint serves no Flux engine, so a Flux query cannot be evaluated here; ..."}. A client that polls this read (as the check does) never converges and hangs until its own deadline.
