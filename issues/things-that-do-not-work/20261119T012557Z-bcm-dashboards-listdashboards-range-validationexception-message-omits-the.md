Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bcm-dashboards.list-dashboards-max-results-out-of-range-rejected
Issue-Shape-Id: com.amazonaws.bcmdashboards#AWSBCMDashboardsService
Issue-Key: bcm-dashboards.list-dashboards-max-results-out-of-range-rejected|run#4|AssertionError|assert message == (
Issue-Commit: 1dc698be9e80e0112562bfdd5ab001c28f556c88

# bcm-dashboards: ListDashboards @range ValidationException message omits the rejected value from the "Value '<x>' at '<member>'" prefix

> _Note: this issue was identified against the code base at commit `1dc698be9e80e0112562bfdd5ab001c28f556c88`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDashboards

## Description
ONE root cause. Related but NOT covered: open issue 20261022T215638Z-bcm-dashboards-dashboardname-pattern-unenforced-constraint-not-found.md, Cause 2, reports the same formatter rendering a placeholder ('21 items') in the value position for an aggregate @length violation on CreateDashboard.widgets; that issue's trace list does not contain this check, and this is the @range path on an integer member where the value position is dropped entirely rather than substituted.

Cause 1 of 1: the constraint-violation message built for a numeric @range violation drops the value clause.

AWS: Smithy (services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/smithy-model/com-amazonaws-bcmdashboards-awsbcmdashboardsservice.json) — ListDashboards.maxResults targets MaxResults, @range min 1 max 100, @default 20; ListDashboards models ValidationException (HTTP 400). A maxResults above the modeled maximum is refused with code ValidationException at HTTP 400, and the message follows AWS's standard request-derived constraint template, whose FIXED text is "1 validation error detected: Value '" + "' at '" + "' failed to satisfy constraint: Member must have value less than or equal to "; its VARIABLE parts are, in order, the rejected value taken from the request, the member name of the violated input member, and the modeled maximum. For this member and a request of maxResults=101 that renders as: "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" (101 is the request's value and is not part of the template; 100 is the model's @range max; 'maxResults' is the member name).

Emulator: HTTP status 400 and error code ValidationException are CORRECT, but the message is "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" — the entire "Value '<rejected value>'" clause collapses to the bare word "Value", so the message never reports WHICH value was rejected. The positive control in the same check (maxResults=100) succeeds, so the @range bound itself is enforced at the right boundary; only the message text diverges. The refusal is raised by the shared boundary constraint validator before the handler runs (see services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/src/ops/list_dashboards.rs:9-10 and :166-167, which both document that maxResults' @range is enforced at the boundary), so the defect is in that validator's message formatter for numeric @range violations, not in the handler.

Why: the value position of a ValidationException message is request-derived and deterministic, so clients, CLIs and test suites that parse it to report which input was rejected (and tooling that string-compares AWS error messages) read a malformed message with no value at all. Any other operation of this service with a numeric @range member is affected the same way, since the formatter is shared.

Fix: the constraint-violation message formatter in this service's request-validation (boundary) layer — render the rejected member's value in the quoted value position for @range violations on numeric shapes, matching the form already expected for @length and @pattern violations.

Traces: bcm-dashboards.list-dashboards-max-results-out-of-range-rejected

## Evidence
Suite simparity-behavioral-aws-v1 — bcm-dashboards 25/26 passed (96%), 1 failed.

[mismatch] bcm-dashboards.list-dashboards-max-results-out-of-range-rejected
Call: bcm.list_dashboards(maxResults=101)
  Code/status: ValidationException / HTTP 400 — MATCHED.
  Expected message: "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
  Actual message:   "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
  Assertion: check_bcm_dashboards_list_dashboards_max_results_out_of_range_rejected.py:19, `assert message == (...)`.
Positive control in the same check: list_dashboards(maxResults=100) returned a response containing 'dashboards' (so the 1..100 bound is enforced at the correct edge).

Diff is exactly the missing "'101' " clause after "Value" — fixed text and the trailing limit match.

## Steps to Reproduce
1. Boot the emulator and point a botocore client at the bcm-dashboards endpoint.
2. Call list_dashboards(maxResults=101). Expect a ClientError with Code=ValidationException, HTTP 400, and Message "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100".
3. Observe the same code and status but the Message "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" (no quoted rejected value).
4. Call list_dashboards(maxResults=100) to confirm the in-range control still succeeds and returns a 'dashboards' key.
5. Repeat with any other out-of-range value (e.g. maxResults=0, which should render "Value '0' ... Member must have value greater than or equal to 1") to confirm the value clause is dropped for the whole @range class, not just one input.
