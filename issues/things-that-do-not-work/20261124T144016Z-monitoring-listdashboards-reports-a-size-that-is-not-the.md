Issue-Source: trace-sweep-divergence
Issue-Trace: monitoring-dashboard-tags-and-not-found-boundary
Issue-Step: task#2
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: monitoring-dashboard-tags-and-not-found-boundary
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|ListDashboards|2xx->2xx|$.DashboardEntries[*].Size
Issue-Commit: 9672f4fa24517e3f79e948eab71d2d612885f9ee

# ListDashboards reports a Size that is not the stored dashboard body's byte length

> _Note: this issue was identified against the code base at commit `9672f4fa24517e3f79e948eab71d2d612885f9ee`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDashboards

## Description
Cause 1 of 1: ListDashboards returns a DashboardEntry whose Size member does not equal the byte length of the dashboard body that PutDashboard stored for that dashboard, so the reported size is far smaller than AWS's.

AWS: Smithy (cloudwatch, GraniteServiceVersion20100801): ListDashboards -> ListDashboardsOutput.DashboardEntries, list of DashboardEntry; DashboardEntry.Size is documented as "The size of the dashboard, in bytes." PutDashboard.DashboardBody is the dashboard document the caller supplies, and GetDashboard echoes it back. AWS reports Size as the byte count of that stored dashboard body as submitted — the same bytes GetDashboard returns — so for a body of N bytes ListDashboards reports Size = N. Status is 200 and the operation is otherwise successful; no error is involved.

Emulator: for a dashboard whose stored body is 323 bytes the replayed ListDashboards answers 200 with DashboardEntries[0].Size = 130 — the entry is present and every other member matched, only the byte count is wrong, and it is substantially smaller than the stored body. Hypothesis: the handler/state computes Size over a re-serialized (compacted / whitespace-stripped) or partial form of the dashboard document — e.g. a parsed-then-reprinted JSON value, or only the widgets array — rather than over the raw body bytes that PutDashboard persisted and GetDashboard returns.

Why: a client that uses ListDashboards to account for dashboard sizes — checking a dashboard against the documented per-dashboard body-size limit, choosing which dashboards to prune, or diffing the listing's Size against the length of the body it just PutDashboard'ed or just read with GetDashboard — reads a size that disagrees with the body the same emulator serves. The inconsistency is silent (HTTP 200, well-formed response), so size-based logic simply computes the wrong answer, and any assertion of "ListDashboards Size == len(GetDashboard DashboardBody)" fails even though both calls succeeded.

Fix: ListDashboards' response projection / the dashboard state record — Size must be derived from the exact stored dashboard-body bytes (the same value GetDashboard returns and PutDashboard accepted), measured in bytes, not from a normalized or re-encoded rendering of it. Capture the body's byte length when PutDashboard stores it, or measure the stored raw body at list time.

Traces: monitoring-dashboard-tags-and-not-found-boundary#task#2 (ListDashboards, expected Size 323, got 130). Note: the open monitoring issues (e.g. 20261029T203422Z-monitoring-cloudwatch-alarm-writes-and-pagination-skip-validation-or.md, 20261102T112036Z-monitoring-cloudwatch-validationexception-messages-drop-the-offending-value-and.md) cover alarm writes, alarm history and ValidationException wording; none covers dashboard Size accounting, and the bcm-dashboards ListDashboards issue is a different service.

## Evidence
Fixture monitoring-dashboard-tags-and-not-found-boundary, step task#2, operation ListDashboards (service monitoring), verdict mismatch.
Expected (recorded AWS): HTTP 200, $.DashboardEntries[0].Size = 323 — the byte length of the dashboard body stored by the preceding PutDashboard in the same fixture.
Actual (emulator): HTTP 200, $.DashboardEntries[0].Size = 130.
Divergence key: service=monitoring|ListDashboards|status=200->200|keys=$.DashboardEntries[0].Size; affected_count 1.
The requirement is the rule, not the number: Size must equal the byte length of that dashboard's stored body; 323 is this recording's value for this recording's body, not a constant to hard-code.
Smithy cited: DashboardEntry.Size doc "The size of the dashboard, in bytes."; ListDashboards output DashboardEntries/NextToken; PutDashboard input DashboardName+DashboardBody; GetDashboard output DashboardBody.

## Steps to Reproduce
1. Replay fixture monitoring-dashboard-tags-and-not-found-boundary against the emulator (replay_wire_corpus) and read step task#2.
2. Equivalently, by hand: PutDashboard with a DashboardName and a DashboardBody document of known byte length (the fixture's body is 323 bytes, including its whitespace).
3. Call ListDashboards (optionally with DashboardNamePrefix scoping to that dashboard) and read DashboardEntries[0].Size.
4. Observe Size = 130 instead of the stored body's byte length (323). Cross-check by calling GetDashboard for the same DashboardName and measuring the returned DashboardBody in bytes: it is the 323-byte body, so the listing's Size disagrees with the body the emulator itself serves.
