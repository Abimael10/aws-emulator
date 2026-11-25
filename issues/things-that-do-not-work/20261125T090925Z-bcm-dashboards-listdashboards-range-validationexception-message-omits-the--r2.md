Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bcm-dashboards.list-dashboards-max-results-out-of-range-rejected
Issue-Shape-Id: com.amazonaws.bcmdashboards#AWSBCMDashboardsService
Issue-Key: bcm-dashboards.list-dashboards-max-results-out-of-range-rejected|run#4|AssertionError|assert message == (
Issue-Commit: 0b60f15d5c198cfe7911e1c64799fc675078ad74

# bcm-dashboards: ListDashboards @range ValidationException message omits the rejected-value clause AWS emits before the member path

> _Note: this issue was identified against the code base at commit `0b60f15d5c198cfe7911e1c64799fc675078ad74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDashboards

## Description
ONE cause.

=== Cause 1 of 1: the @range constraint-violation message for ListDashboards.maxResults is rendered WITHOUT the "Value '<rejected>' " clause that precedes the member path in AWS's validation message ===

AWS: Smithy (services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/smithy-model/com-amazonaws-bcmdashboards-awsbcmdashboardsservice.json) — ListDashboards.maxResults targets MaxResults, type integer, @range min 1 max 100, @default 20; ListDashboards models ValidationException (errorKind client, httpError 400) whose single required member is `message`. MaxResults is not @sensitive, so the rejected number is not redacted. A maxResults above the modeled maximum is refused with HTTP 400 / code ValidationException. For the message text this is recorded behavior only: the behavioral recording shows AWS wording a numeric @range violation in the standard client-side validation form, whose FIXED text is
  "<N> validation error detected: Value '<v>' at '<member>' failed to satisfy constraint: Member must have value less than or equal to <max>"
and whose VARIABLE parts are the error count N, the rejected value <v> taken from the request, the member name <member> taken from the request (`maxResults`), and the bound <max> taken from the Smithy @range on MaxResults (`100`). The recorded probe's 101/100 are only one filling of that template, not part of it.

Emulator: HTTP status 400 and error code ValidationException are CORRECT. Only the message diverges: the emulator serves
  "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
with the whole "Value '<v>' " position elided, where AWS serves
  "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100".
The positive control in the same check (maxResults=100) succeeds and returns a `dashboards` key, so the constraint boundary itself is enforced correctly — the defect is confined to the message template for this constraint class.

Why: the message is part of the contract for a modeled client error. A client, SDK test or assertion library that reads or matches a ValidationException message for an out-of-range pagination size cannot recover WHICH value was rejected from the emulator's text, and exact-message comparisons that hold against AWS fail against the emulator. For a request with several members the clause is the only part of the message that distinguishes the offending value, so diagnostics regress.

Context a fixer must reconcile (not a separate cause): this message template has oscillated. Open wire issue 20261124T193907Z-bcm-dashboards-validationexception-message-for-the-maxresults-range.md asserts the OPPOSITE requirement for the same operation and the same constraint (it demands the value position be removed), and fixed issues 20261119T134026Z-bcm-dashboards-listdashboards-range-validationexception-message-adds-a.md / 20261114T225916Z-bcm-dashboards-listdashboards-maxresults-range-refusal-message-interpolates-the-rejected.md removed it while 20261119T012557Z-bcm-dashboards-listdashboards-range-validationexception-message-omits-the.md / 20261113T142527Z-bcm-dashboards-listdashboards-maxresults-range-refusal-message-drops-the-rejected.md put it back. Neither of those carries this behavioral check's replay key, so none of them covers this failure. The behavioral recording replayed here requires the value clause present; whoever fixes this must settle the template once against the recordings rather than flipping it again, and must keep the @range template distinct from the @length/@pattern ones.

Fix: the constraint-violation message formatter in this service's request-validation layer (the layer that builds ValidationException.message for ListDashboards' maxResults @range check) — it must render the numeric @range template with the rejected value interpolated in the "Value '<v>' at '<member>'" prefix. Status code, error code and the success path need no change.

Traces: bcm-dashboards.list-dashboards-max-results-out-of-range-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 27/29 passed; this check is [mismatch].

Check bcm-dashboards.list-dashboards-max-results-out-of-range-rejected, operation ListDashboards:
  bcm.list_dashboards(maxResults=101) -> HTTP 400, Error.Code "ValidationException" (both correct).
  Error.Message assertion failed:
    expected: "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
    actual:   "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
  AssertionError raised at check line 19 on the message comparison.

Fixed text of the required message: "<N> validation error detected: Value '" + <rejected value> + "' at '" + <member> + "' failed to satisfy constraint: Member must have value less than or equal to " + <modeled max>. Variable parts: N (count of validation errors), the rejected value (from the request; 101 in this replay), the member name (`maxResults`, from the request) and the bound (`100`, from Smithy @range on MaxResults). The emulator drops the "Value '<rejected value>' " fragment entirely.

Positive control in the same check passed: maxResults=100 succeeded and the response contained a `dashboards` key.

## Steps to Reproduce
1. Boot the emulator and point a botocore client at the bcm-dashboards endpoint.
2. Call ListDashboards with maxResults=101 (any value above the modeled maximum of 100).
3. Observe HTTP 400 with Error.Code ValidationException (correct) and Error.Message "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100".
4. AWS/the recording gives "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" — the rejected value appears in a "Value '<v>' " clause before the member path.
5. Call ListDashboards with maxResults=100 to confirm the in-range control still succeeds and returns a `dashboards` key.
6. Re-replay behavioral check bcm-dashboards.list-dashboards-max-results-out-of-range-rejected to confirm the fix.
