Issue-Source: trace-sweep-divergence
Issue-Trace: bcm-dashboards-list-dashboards-max-results-above-range-rejected
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bcm-dashboards-list-dashboards-max-results-above-range-rejected
Issue-Shape-Id: com.amazonaws.bcmdashboards#AWSBCMDashboardsService
Issue-Key: bcm-dashboards|ListDashboards|4xx->4xx|$.message
Issue-Commit: b235f43ec24f1e947704718b25995d7af0836105

# bcm-dashboards: ListDashboards @range ValidationException message adds a quoted rejected value AWS does not emit

> _Note: this issue was identified against the code base at commit `b235f43ec24f1e947704718b25995d7af0836105`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDashboards

## Description
ONE root cause (Cause 1 of 1): the numeric @range constraint-violation message formatter for this service renders a quoted rejected-value clause that the recorded AWS response does not contain.

IMPORTANT — this is the OPPOSITE direction of open issue 20261119T012557Z-bcm-dashboards-listdashboards-range-validationexception-message-omits-the.md, which asserts AWS emits "Value '101' at 'maxResults'" and asks for the value clause to be ADDED. The wire recording of the real service in this corpus shows no value clause at all for this error class, and the replay reports this step as NOT owned by that issue. The two must be reconciled before either is closed: implementing that issue's requested form is what now produces this divergence.

AWS: Smithy (services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/smithy-model/com-amazonaws-bcmdashboards-awsbcmdashboardsservice.json) — ListDashboards.maxResults targets MaxResults with @range min 1 / max 100 and @default 20; ListDashboards models ValidationException at HTTP 400. The model states the bound and the error, but says nothing about message text, so the message is recorded behavior only. The recording shows a maxResults above the modeled maximum refused with HTTP 400 / ValidationException and the message "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100". Its FIXED text is "1 validation error detected: Value at '" + "' failed to satisfy constraint: Member must have value less than or equal to " — i.e. the word "Value" is followed directly by " at '", with NO quoted value in between. Its VARIABLE parts are the violated input member's name (here 'maxResults', taken from the request) and the modeled maximum (here 100, taken from the @range trait). The rejected request value does not appear anywhere in the message.

Emulator: status 400 and error code ValidationException are CORRECT; only the message diverges. The emulator serves "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" — it injects an extra "'<rejected value>' " clause after the word "Value", so the message carries a segment the recorded service never emits. (101 here is just this fixture's request value and is not part of the template; the defect is the presence of the clause, not that particular number.)

Why: a client, CLI or test suite that string-compares or regex-parses this service's ValidationException message gets a message whose prefix does not match the real service's for the whole numeric-@range error class. Because the formatter is shared, every bcm-dashboards operation with a numeric @range member reports a differently-shaped message than AWS, so error-text assertions and message-driven user output are wrong wherever the real wording is relied upon.

Fix: the shared request-validation (boundary) constraint-violation message formatter for this service — the path that refuses maxResults before the handler runs (documented at services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/src/ops/list_dashboards.rs:9-10 and :166-167). It must emit the recorded no-value form for numeric @range violations rather than substituting the request value into a quoted value position.

Traces: bcm-dashboards-list-dashboards-max-results-above-range-rejected#task#1

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (49 steps, 43 passed, 6 mismatched, score 87.8%).

Fixture bcm-dashboards-list-dashboards-max-results-above-range-rejected, step task#1 — service bcm-dashboards, operation ListDashboards, verdict mismatch.
  Expected status: 400. Actual status: 400 (MATCHED).
  Error code ValidationException (MATCHED).
  Divergence detail, $.message value_mismatch:
    expected "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
    actual   "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
  The only difference is the emulator's extra "'101' " clause between "Value" and "at".
  owned_by: [] and stub_path: "" — no filed issue carries this replay key.

Fixed vs variable in the recorded message: fixed text = "1 validation error detected: Value at '" ... "' failed to satisfy constraint: Member must have value less than or equal to "; variables = the member name ('maxResults', from the request) and the limit (100, from the model's @range max). The value 101 is a per-request value and appears in neither the recorded template nor the recorded message.

Model cited: smithy_query(operation=ListDashboards) on services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/smithy-model/com-amazonaws-bcmdashboards-awsbcmdashboardsservice.json — maxResults @range {min:1,max:100}, @default 20; errors include ValidationException (httpError 400).

## Steps to Reproduce
1. Boot the emulator from this checkout and point an AWS client at the bcm-dashboards endpoint.
2. Replay fixture bcm-dashboards-list-dashboards-max-results-above-range-rejected, step task#1 (equivalently: call ListDashboards with maxResults=101).
3. Observe HTTP 400 with code ValidationException (correct) and message "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100".
4. Compare with the recorded response for that step: "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" — the emulator adds a quoted rejected-value clause the recording does not contain.
5. Repeat with another out-of-range value (e.g. maxResults=0, whose recorded form would be "Value at 'maxResults' failed to satisfy constraint: Member must have value greater than or equal to 1") to confirm the extra clause is emitted for the whole numeric-@range class, not just one input.
6. Before changing the formatter, reconcile with open issue 20261119T012557Z-bcm-dashboards-listdashboards-range-validationexception-message-omits-the.md, which requests the opposite wording; the wire recording in simparity-wire-aws-v1 is the evidence for the no-value form.
