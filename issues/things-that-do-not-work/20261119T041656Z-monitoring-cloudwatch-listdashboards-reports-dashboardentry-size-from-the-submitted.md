Issue-Source: trace-sweep-divergence
Issue-Trace: monitoring-dashboard-tags-and-not-found-boundary
Issue-Step: task#2
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: monitoring-dashboard-tags-and-not-found-boundary
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|ListDashboards|2xx->2xx|$.DashboardEntries[*].Size
Issue-Commit: 4e953d0ceb021d373a4669be7884bbadfda507a7

# cloudwatch: ListDashboards reports DashboardEntry.Size from the submitted body instead of the stored dashboard document

> _Note: this issue was identified against the code base at commit `4e953d0ceb021d373a4669be7884bbadfda507a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDashboards

## Description
Cause 1 of 1: ListDashboards serves a DashboardEntry.Size that is far smaller than the one CloudWatch serves for the same PutDashboard body, because the emulator counts the request string rather than the dashboard document the service holds.

AWS: Smithy: DashboardEntry$Size is a `long` documented "The size of the dashboard, in bytes." (com.amazonaws.cloudwatch#DashboardEntry, target com.amazonaws.cloudwatch#Size); PutDashboardInput$DashboardBody is an unconstrained @required string whose documentation defers the document's shape to AWS's "Dashboard Body Structure and Syntax" reference. The model states NO rule for how that document is completed or measured, so for the measurement itself this is recorded behavior only: in the recorded run, a dashboard created by this fixture's PutDashboard is listed by ListDashboards with Size = 323 for a submitted body of roughly a hundred bytes — i.e. AWS reports the size of the dashboard it STORED, which is the submitted body completed by the service, not the length of the text the request carried. Hypothesis: the completion CloudWatch performs spans the whole document (per-widget layout members and per-widget rendering properties, as well as the top-level members), which is why the stored document is several times the size of a minimal submitted body.

Emulator: ListDashboards returns Size straight from StoredDashboard.document_bytes, which PutDashboard computes as `stored_dashboard_document(body).len()`. That completion adds exactly ONE top-level member — `"periodOverride":"auto"` — appended before the closing brace when the body omits it, and nothing else; no widget is completed and no other member is defaulted. For the fixture's dashboard the replay reports `$.DashboardEntries[0].Size: value_mismatch (expected 323, got 130)`: the emulator's 130 is the submitted body's own byte count plus that single appended member. Status is 200 on both sides and every other DashboardEntry member matches, so this is purely the size accounting.

Why: Size is the only field a client has for the dashboard-document size, and clients use it to track a dashboard against CloudWatch's document-size limit and to decide whether a body can still absorb another widget before a PutDashboard is rejected. An emulator reporting roughly 40% of the real size makes a dashboard look like it has headroom it does not have, so a client that budgets against Size proceeds to a write that real CloudWatch refuses; a client that diffs Size to detect that a dashboard changed also sees a number that cannot be reproduced against AWS.

Fix: the dashboard-document completion in this service's state — the one function that builds the document whose bytes both the write and the listing count — must complete the whole document the way CloudWatch does (the defaulted top-level members AND the per-widget members the body structure gives a default), so the byte count the write stores and the listing reports is the size of the dashboard the service holds. The listing handler itself needs no change: it already reads the stored count, so the measurement must be corrected where the document is built, not where it is served. Note that the body must still round-trip verbatim through GetDashboard, so completion has to feed the SIZE accounting without replacing the stored body text.

No open issue covers this: the 25 other monitoring clusters in this replay are each owned by an existing issue, and the dashboard-related open issues (20261119T012557Z-bcm-dashboards-listdashboards-range-validationexception-message-omits-the.md, 20261114T112305Z-bcm-dashboards-getdashboard-serves-the-wrong-widget-list-for-every.md) are against the bcm-dashboards service, not CloudWatch's monitoring endpoint, and concern a @range validation message and a seeded widget catalog rather than DashboardEntry.Size.

Traces: monitoring-dashboard-tags-and-not-found-boundary#task#2.

## Evidence
Replay cluster (service=monitoring|ListDashboards|status=200->200|keys=$.DashboardEntries[0].Size), fixture monitoring-dashboard-tags-and-not-found-boundary, step task#2, verdict mismatch, affected_count 1.

Expected status 200, actual status 200.
Detail, verbatim: `$.DashboardEntries[0].Size: value_mismatch (expected 323, got 130)`

Expected (AWS, recorded): DashboardEntries[0].Size = 323 — the byte count of the dashboard document CloudWatch stored for the body this fixture put.
Actual (emulator): DashboardEntries[0].Size = 130 — the submitted body's byte count plus the single `"periodOverride":"auto"` member the emulator appends.

The 323 and 130 above are this one recording's values for this one body and are NOT the requirement: the requirement is that Size equals the byte length of the completed dashboard document CloudWatch holds for whatever body was submitted. No error code or message is involved — both sides answer 200 and all other members of the entry (DashboardName, DashboardArn, LastModified) match.

Emulator code carrying the defect (citations only):
- services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/list_dashboards.rs:213-217 — `size: Some(i64::try_from(stored.document_bytes)...)`, serving the stored count as-is.
- services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/put_dashboard.rs:249-256 — `document_bytes: crate::state::stored_dashboard_document_bytes(&input.dashboard_body)`, counted once at write time.
- services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state/dashboard_document.rs:29 — `const DEFAULTED_MEMBERS: &[(&str, &str)] = &[("periodOverride", "\"auto\"")];` — the entire completion, one top-level member.
- services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state/dashboard_document.rs:43-74 — `stored_dashboard_document` appends only the missing top-level default before the final `}`, and `stored_dashboard_document_bytes` returns that string's `.len()`; no widget-level completion exists.

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay the wire corpus: replay_wire_corpus().
2. Fixture monitoring-dashboard-tags-and-not-found-boundary, step task#2 diverges on ListDashboards.
3. Equivalently, by hand against the emulator's monitoring endpoint:
   a. PutDashboard with a DashboardName and a minimal but valid DashboardBody — a JSON object with one metric widget that omits the layout members (x/y/width/height) and the rendering properties (view/stacked/region/period) and omits periodOverride.
   b. ListDashboards with a DashboardNamePrefix matching that name.
   c. Read DashboardEntries[0].Size.
4. Observed: Size equals the submitted DashboardBody's byte length plus the 25 bytes of the appended `,"periodOverride":"auto"` member (130 for this fixture's body).
5. Expected: Size equals the byte length of the dashboard document the service holds after completing the submitted body with every member CloudWatch defaults — several times the submitted body for a minimal body (323 for this fixture's body).
