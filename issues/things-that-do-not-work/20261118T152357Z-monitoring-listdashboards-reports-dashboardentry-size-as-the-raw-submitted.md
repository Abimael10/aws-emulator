Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatch-dashboard-create-update-and-invalid-body
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatch-dashboard-create-update-and-invalid-body cloudwatch-dashboard-crud-round-trip monitoring-dashboard-lifecycle monitoring-dashboard-put-get-delete-lifecycle monitoring-dashboard-tags-and-not-found-boundary
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|ListDashboards|2xx->2xx|$.DashboardEntries[*]
Issue-Key: monitoring|ListDashboards|2xx->2xx|$.DashboardEntries[*].Size
Issue-Commit: 9279d0e488cae7bddd5fab1b173f5621c68407ec

# ListDashboards reports DashboardEntry.Size as the raw submitted body length, undercounting the stored dashboard document

> _Note: this issue was identified against the code base at commit `9279d0e488cae7bddd5fab1b173f5621c68407ec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDashboards

## Description
Two net-new divergence clusters, both on CloudWatch (service token `monitoring`) ListDashboards, both the same fault: the `Size` the emulator reports for a dashboard entry is smaller than the size real CloudWatch reports for the same dashboard, by a constant 24 bytes in every recording.

Cause 1 of 2 — ListDashboards after PutDashboard create+update (fixture `cloudwatch-dashboard-create-update-and-invalid-body`, step `task#3`; same cluster also covers `cloudwatch-dashboard-crud-round-trip#task#3` and `monitoring-dashboard-lifecycle#task#3`).
  AWS: Smithy: `ListDashboards` -> `ListDashboardsOutput$DashboardEntries` -> `DashboardEntry$Size`, documented as "The size of the dashboard, in bytes." The model states no formula, so recorded behavior only: for the dashboard created by the fixture's PutDashboard, the recorded ListDashboards entry reports `Size` = 149 for a dashboard whose submitted DashboardBody is 125 bytes, i.e. AWS measures a stored document 24 bytes larger than the request's body string. Status is 200 and the entry's `DashboardName`/`DashboardArn` match; only `Size` is a real divergence.
  Emulator: returns 200 with `Size` = 125 — exactly `DashboardBody.len()` of the string the caller submitted.
  Why: a client sizing dashboards against the 100 KB-per-dashboard / account dashboard-size budgets (capacity checks, "is this dashboard near the limit" logic, cost or inventory reporting, diffing a dashboard's stored size against the body it holds locally) systematically under-measures every dashboard and concludes it has headroom it does not have. The number is also the only size the API exposes, so there is no second source for a client to correct against.
  Fix: ListDashboards' DashboardEntry projection, and the dashboard state write that feeds it — the handler sets `size: Some(i64::try_from(stored.body.len())...)` directly from the verbatim body PutDashboard stored, so neither the normalization AWS applies to the document it stores nor any per-dashboard storage overhead is accounted for. Hypothesis: real CloudWatch sizes the NORMALIZED document it persists (what it re-serializes from the submitted body) rather than the request string, which is why the delta is a fixed 24 bytes across differently sized bodies; whichever of the two it is, the size reported must be the size of the document the service stores, computed in one place shared by the state write and this projection.
  Traces: `cloudwatch-dashboard-create-update-and-invalid-body#task#3` (representative), plus `cloudwatch-dashboard-crud-round-trip#task#3`, `monitoring-dashboard-lifecycle#task#3`. Code: `services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/list_dashboards.rs` lines 213-215 (Size from `stored.body.len()`), `.../src/ops/put_dashboard.rs` line 248 (`body: input.dashboard_body.clone()`).

Cause 2 of 2 — ListDashboards in the put/get/delete lifecycle and the tags/not-found boundary (fixture `monitoring-dashboard-put-get-delete-lifecycle`, step `task#3`; same cluster also covers `monitoring-dashboard-tags-and-not-found-boundary#task#2`).
  AWS: same member, `DashboardEntry$Size`, "The size of the dashboard, in bytes." Recorded behavior only: 200 with `Size` = 165 for this fixture's dashboard body, again 24 bytes more than the submitted body string.
  Emulator: returns 200 with `Size` = 141.
  Why: identical consequence, reached through a different fixture family (a dashboard that is then tagged and deleted), so the undercount is not specific to one body or one call order — every ListDashboards page the emulator serves carries an understated size.
  Fix: the same ListDashboards projection / dashboard state write; fixing the size computation in one place closes both clusters.
  Traces: `monitoring-dashboard-put-get-delete-lifecycle#task#3`, `monitoring-dashboard-tags-and-not-found-boundary#task#2`.

Not part of this report: the recorded `LastModified` instants differ from the emulator's (e.g. 1787861045.0 vs 1795015275.918). The instant itself is a per-run clock value and is NOT a requirement; it is named here only so a fixer does not chase it. No other open issue covers DashboardEntry.Size — the existing open monitoring/cloudwatch issues concern alarm writes, alarm history data, anomaly detectors, ValidationException message text and the awsQuery XML serializer, none of which touch dashboard sizing.

## Evidence
Cause 1 — fixture `cloudwatch-dashboard-create-update-and-invalid-body`, step `task#3`, operation ListDashboards, expected status 200, actual status 200:
  expected `$.DashboardEntries[0]` = {'DashboardArn': 'arn:aws:cloudwatch::<account>:dashboard/<name>', 'DashboardName': '<name>', 'LastModified': <epoch>, 'Size': 149}
  actual   `$.DashboardEntries[0]` = {'DashboardName': '<name>', 'DashboardArn': 'arn:aws:cloudwatch::<account>:dashboard/<name>', 'LastModified': <epoch>, 'Size': 125}
  The only non-per-run difference is Size: 149 expected vs 125 served (= len(DashboardBody)).
  Cluster members: cloudwatch-dashboard-create-update-and-invalid-body#task#3, cloudwatch-dashboard-crud-round-trip#task#3, monitoring-dashboard-lifecycle#task#3.

Cause 2 — fixture `monitoring-dashboard-put-get-delete-lifecycle`, step `task#3`, operation ListDashboards, expected status 200, actual status 200:
  `$.DashboardEntries[0].Size`: value_mismatch (expected 165, got 141).
  Cluster members: monitoring-dashboard-put-get-delete-lifecycle#task#3, monitoring-dashboard-tags-and-not-found-boundary#task#2.

In both clusters the served value equals the byte length of the DashboardBody string the fixture submitted, and the recorded value is that length plus 24 — a constant offset across two differently sized bodies. No HTTP status or error code diverges; this is a success-path value defect.

## Steps to Reproduce
1. Replay the wire corpus (`replay_wire_corpus`) against the emulator built from this checkout; clusters `service=monitoring|ListDashboards|status=200->200|keys=$.DashboardEntries[0]` and `...|keys=$.DashboardEntries[0].Size` diverge.
2. By hand, against the emulator's CloudWatch (monitoring) endpoint:
   a. `PutDashboard` with DashboardName=<name> and a DashboardBody of exactly N bytes (e.g. the 125-byte single-widget body used by `cloudwatch-dashboard-create-update-and-invalid-body`).
   b. `ListDashboards` with DashboardNamePrefix=<name>.
3. Observe `DashboardEntries[0].Size` == N (125). Real CloudWatch reports the size of the stored document for the same request (149 in the recording, i.e. N+24).
4. Repeat with the 141-byte body from `monitoring-dashboard-put-get-delete-lifecycle`: the emulator reports 141, the recording reports 165.
