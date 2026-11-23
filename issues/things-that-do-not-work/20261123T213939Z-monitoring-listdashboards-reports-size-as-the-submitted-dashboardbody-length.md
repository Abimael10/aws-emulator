Issue-Source: trace-sweep-divergence
Issue-Trace: monitoring-dashboard-tags-and-not-found-boundary
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: monitoring-dashboard-tags-and-not-found-boundary
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|ListDashboards|2xx->2xx|$.DashboardEntries[*].Size
Issue-Commit: b594a8ba36cfc3c333d42d08382259bcd0c33412

# ListDashboards reports Size as the submitted DashboardBody length, not the size of the dashboard document CloudWatch holds

> _Note: this issue was identified against the code base at commit `b594a8ba36cfc3c333d42d08382259bcd0c33412`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDashboards

## Description
One net-new cause in the cloudwatch (monitoring, 2010-08-01) emulator.

Cause 1 of 1: DashboardEntry$Size is computed from the request string PutDashboard was given rather than from the dashboard document the service stores, so every listed dashboard is reported substantially smaller than AWS reports it.

AWS: Smithy (cloudwatch 2010-08-01) models ListDashboards -> ListDashboardsOutput$DashboardEntries, a list of DashboardEntry; DashboardEntry$Size is documented simply as "The size of the dashboard, in bytes." (smithy_query query="doc" shape="DashboardEntry" member="Size"). The model says nothing further about how that byte count is derived, so the requirement here is recorded behavior only: for the recorded dashboard the fixture creates with PutDashboard and then lists, AWS answers HTTP 200 with DashboardEntries[0].Size = 323, i.e. a byte count materially larger than the body text the write submitted. The invariant is that Size measures the dashboard document CloudWatch HOLDS — the submitted body after the service has completed it with the top-level and per-widget members the dashboard body syntax defines defaults for (the reference PutDashboardInput$DashboardBody points at: "For more information about the syntax, see Dashboard Body Structure and Syntax.") — not the length of the caller's request string. Hypothesis: AWS normalises and completes the stored document with more than one defaulted member (per-widget defaults such as region/view/stacked and the remaining top-level members), which is why the held document is ~2.5x the submitted body here. Size is a plain count with no fixed or variable message text; no error class is involved and the status (200) and the rest of the entry (DashboardName, DashboardArn, LastModified) match.

Emulator: the listing answers HTTP 200 but reports DashboardEntries[0].Size = 130 where AWS reported 323. The count comes from crate::state::dashboard_document::stored_dashboard_document_bytes, which is the submitted DashboardBody string length plus, at most, the single member `"periodOverride":"auto"` when the body omitted it (DEFAULTED_MEMBERS lists only periodOverride); PutDashboard stores that number once in StoredDashboard.document_bytes and ListDashboards echoes it verbatim. For a body that already names periodOverride the reported Size is exactly the request string length. The shortfall is therefore systematic, not specific to this recording: every dashboard in every listing is reported short by whatever the service's own document completion would have added.

Why: a client that uses Size to track dashboard growth against CloudWatch's documented per-dashboard body limit, to decide whether a widget can still be added, or to compare a listing against what it believes it stored, is told the dashboard is far smaller than AWS says it is. Checks of the form "stay under the dashboard size budget" pass against the emulator and fail against AWS, and any client that round-trips Size (list, compare, re-put) sees a value that never agrees with the service it is emulating.

Fix: the dashboard document model in the cloudwatch service crate's state — the function that builds the stored document and counts its bytes must produce the document CloudWatch actually holds (the submitted body completed with every member the dashboard body syntax defaults, not only periodOverride), with ListDashboards continuing to report that count. GetDashboard must keep returning the submitted body verbatim, so the completion used for the byte count has to be modelled separately from the stored body text.

Traces: monitoring-dashboard-tags-and-not-found-boundary#task#2 (ListDashboards, status 200 both sides, $.DashboardEntries[0].Size expected 323, got 130).

## Evidence
Fixture monitoring-dashboard-tags-and-not-found-boundary, step task#2, operation ListDashboards, service monitoring.
Expected (recorded AWS response): HTTP 200, $.DashboardEntries[0].Size = 323.
Actual (emulator): HTTP 200, $.DashboardEntries[0].Size = 130.
Replay verdict: mismatch, divergence key service=monitoring|ListDashboards|status=200->200|keys=$.DashboardEntries[0].Size. Every other asserted member of the entry (DashboardName, DashboardArn, LastModified) and every other step of the fixture matched, so the divergence is isolated to the byte count.
Source of the emulator value: services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state/dashboard_document.rs lines 29 and 43-74 (DEFAULTED_MEMBERS contains only ("periodOverride", "\"auto\""); stored_dashboard_document_bytes returns stored_dashboard_document(body).len()), written into state by services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/put_dashboard.rs lines 254-256 and echoed by services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/list_dashboards.rs lines 213-217.
No open issue covers this: the replay cluster reports no owning issue, and the open cloudwatch bundle 20261117T082319Z-cloudwatch-7-net-new-causes-putmetricalarm-accepts-threshold.md touches dashboards only via GetDashboard's DashboardName validation message (its Cause 3), not DashboardEntry$Size.

## Steps to Reproduce
1. Boot the emulator and point a CloudWatch client at it.
2. PutDashboard(DashboardName=<name>, DashboardBody=<a valid dashboard JSON object with at least one widget>), e.g. the body the fixture monitoring-dashboard-tags-and-not-found-boundary writes at task#1.
3. ListDashboards() and read DashboardEntries[0].Size.
4. Observe that Size equals the length in bytes of the DashboardBody string just submitted (plus 22 bytes when the body omitted periodOverride), rather than the size of the completed dashboard document AWS holds: the recording for this fixture shows 323, the emulator answers 130.
5. Re-put the same dashboard with an explicit "periodOverride" member and observe Size collapse to exactly the request string length, confirming the count is taken from the request rather than from the stored document.
