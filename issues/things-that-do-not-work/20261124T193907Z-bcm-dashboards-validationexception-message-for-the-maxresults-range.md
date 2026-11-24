Issue-Source: trace-sweep-divergence
Issue-Trace: bcm-dashboards-list-dashboards-max-results-above-range-rejected
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bcm-dashboards-list-dashboards-max-results-above-range-rejected
Issue-Shape-Id: com.amazonaws.bcmdashboards#AWSBCMDashboardsService
Issue-Key: bcm-dashboards|ListDashboards|4xx->4xx|$.message
Issue-Commit: 6df4780ed6263f6fd5cc79bc833a1ad99a020747

# bcm-dashboards: ValidationException message for the maxResults @range violation inserts a value position AWS omits

> _Note: this issue was identified against the code base at commit `6df4780ed6263f6fd5cc79bc833a1ad99a020747`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDashboards

## Description
ONE cause.

=== Cause 1 of 1: the @range constraint-violation message for ListDashboards.maxResults renders a "Value '<n>' " position that AWS's message for this constraint class does not contain ===

AWS: Smithy (services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/smithy-model/com-amazonaws-bcmdashboards-awsbcmdashboardsservice.json) — ListDashboards.maxResults targets MaxResults, type integer, @range min 1 max 100, @default 20; ListDashboards models ValidationException (errorKind client, httpError 400), whose single required member is `message`. A maxResults above the modeled maximum is refused with HTTP 400 and code ValidationException. Recorded behavior only for the message text: AWS words a numeric @range violation for this service WITHOUT a value position, i.e. the fixed text is "<N> validation error(s) detected: Value at '<member>' failed to satisfy constraint: Member must have value less than or equal to <max>", where the variable parts are the error count, the member name taken from the request (`maxResults`) and the modeled bound (`100`); the rejected number itself is not interpolated. MaxResults is not @sensitive, so the omission is the service's message template for this constraint class, not redaction.

Emulator: status 400 and code ValidationException are CORRECT, but the message adds a value position: "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" — the "Value '<the rejected number>' " fragment is emulator-added text that is absent from AWS's template for this constraint. (101 is only the recorded probe's value; the defect is the extra value position, not that particular number.)

Why: a client or test suite that compares or parses a ValidationException message for an out-of-range pagination size sees text AWS never emits, so exact-message assertions fail against the emulator and succeed against AWS (or vice versa). Note the message formatter here diverges in the OPPOSITE direction from the already-open behavioral issue 20261022T215638Z-bcm-dashboards-dashboardname-pattern-unenforced-constraint-not-found.md (its Cause 2 requires the value position to be filled in for an aggregate @length violation on CreateDashboard.widgets, where AWS does echo the rejected value). That issue is grounded in behavioral check ids for CreateDashboard and carries no replay key for this fixture step, so it does not cover this wire divergence; both must hold — the value position is present for @length on aggregates and absent for @range on integers.

Fix: the constraint-violation message formatter in this service's request-validation layer — it must emit the @range template without a value position for numeric members (and keep per-constraint-class templates distinct from the @length/@pattern ones), rather than using one generic "Value '<v>' at '<member>' ..." form for every constraint.

Traces: bcm-dashboards-list-dashboards-max-results-above-range-rejected#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 (9 fixtures, 49 steps, 43 passed, 6 mismatched).

Fixture bcm-dashboards-list-dashboards-max-results-above-range-rejected, step task#1, operation ListDashboards, verdict mismatch.
  Expected status 400, actual status 400 (match); error code ValidationException (match).
  $.message: value_mismatch
    expected: "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
    actual:   "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"

Fixed text of the expected message: "1 validation error detected: Value at '" + <member> + "' failed to satisfy constraint: Member must have value less than or equal to " + <modeled max>. Variable parts: the member name (`maxResults`, from the request) and the bound (`100`, from the Smithy @range on MaxResults). The rejected request value does not appear anywhere in AWS's message; the emulator inserts it as "Value '101' ".

## Steps to Reproduce
1. Boot the emulator and point a client at the bcm-dashboards endpoint.
2. Call ListDashboards with maxResults set above the modeled maximum (the fixture uses 101).
3. Observe HTTP 400 with code ValidationException (correct) and message "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100".
4. AWS/the recorded response gives "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" — no value position.
5. Re-replay fixture bcm-dashboards-list-dashboards-max-results-above-range-rejected step task#1 to confirm.
