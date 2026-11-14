Issue-Source: trace-sweep-divergence
Issue-Trace: bcm-dashboards-list-dashboards-max-results-above-range-rejected
Issue-Step: task#1
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bcm-dashboards-list-dashboards-max-results-above-range-rejected
Issue-Shape-Id: com.amazonaws.bcmdashboards#AWSBCMDashboardsService
Issue-Key: bcm-dashboards|ListDashboards|4xx->4xx|$.message
Issue-Commit: fa1c1c9d2e17cac1aa32d43ac054c126cd511eb8

# ListDashboards maxResults @range refusal message interpolates the rejected value the wire recording omits

> _Note: this issue was identified against the code base at commit `fa1c1c9d2e17cac1aa32d43ac054c126cd511eb8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDashboards

## Description
ONE root cause (Cause 1 of 1). This is the SAME refusal message that open issue 20261113T142527Z-bcm-dashboards-listdashboards-maxresults-range-refusal-message-drops-the-rejected.md covers from the behavioral side, re-diverging in the OPPOSITE direction: that issue asked the integer @range formatter to START emitting the "Value '<submitted>' " clause, the formatter now emits it, and this wire fixture step grades the same message WITHOUT that clause. The two recordings of this one message contradict each other and must be reconciled, not flipped again. Do NOT close this by simply removing the clause: that is exactly the edit that produced the previously-fixed issue 20261111T084409Z-bcm-dashboards-listdashboards-maxresults-range-refusal-message-interpolates.md and started the oscillation.

=== Cause 1 of 1: the integer @range ValidationException message carries a rejected-value clause the wire recording does not ===
AWS: Smithy (services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/smithy-model/com-amazonaws-bcmdashboards-awsbcmdashboardsservice.json, smithy_query query="operation" operation="ListDashboards") - ListDashboards.maxResults targets MaxResults, integer, @default 20, @range min 1 max 100; modelled errors AccessDeniedException (403), InternalServerException (500), ThrottlingException (429), ValidationException (400); @readonly, @paginated(pageSize=maxResults, items=dashboards). A maxResults above the modelled maximum is a client constraint violation, so the call is refused with HTTP 400 and error code ValidationException - that much the model fixes, and the emulator already gets it right. The message WORDING is recorded behavior only, and the two recordings disagree. This wire fixture records the refusal sentence as fixed text "<n> validation error detected: Value at '<member>' failed to satisfy constraint: Member must have value less than or equal to <max>", with NO value clause; its variable parts are the violation count, the member name taken from the request (`maxResults`) and the ceiling taken from the member's modelled @range maximum (100). The behavioral recording cited above records the same sentence WITH an interpolated "Value '<rejected value>' " clause, the rejected value taken from the request. Nothing in the model decides between them.
Emulator: status and error code match - HTTP 400, ValidationException - but the message is "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" where this fixture step recorded "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100". The sole difference is the interpolated rejected-value clause between "Value" and "at '<member>'"; the count, the member name, the constraint phrase and the bound all match. (The '101' in this replay is the value the request submitted, not a fixed part of the template.)
Why: a client branching only on HTTP status or error code is unaffected. Anything reading the message diverges: a log matcher, test assertion or client that pattern-matches the AWS constraint template against this emulator matches one shape of the sentence and not the other, and because the emulator's own two corpora disagree about which shape it is, any such matcher is broken by whichever direction the formatter is last flipped to. The user-visible cost is that the refusal text for every integer @range violation in this service is not stably reproducible.
Fix: the shared boundary constraint validator that enforces @range before the handler runs - services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/src/ops/list_dashboards.rs documents at its module head that a maxResults outside 1..100 is refused by the boundary validator before `handle` runs, so the message comes from that validator's integer-@range message formatter. The status (400) and the error shape (ValidationException) already match and need no change; no change belongs in the ListDashboards handler, its pagination projection or its serializer. Before touching the formatter, decide which of the two recordings of this single message is authoritative and record that decision, then make BOTH the wire fixture step and the behavioral check agree with it - re-replay them together. Flipping the clause to satisfy one corpus alone will re-open the other, which is what has already happened twice.
Traces: bcm-dashboards-list-dashboards-max-results-above-range-rejected#task#1 (this wire step); behavioral check bcm-dashboards.list-dashboards-max-results-out-of-range-rejected records the same message in the opposite direction.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 49 steps, 43 passed, 6 mismatched, score 87.8%, emulator_up=true (/__admin/health answered 200). Five of the six mismatches are GetDashboard widget-list clusters already owned by 20261114T112305Z-bcm-dashboards-getdashboard-serves-the-wrong-widget-list-for-every.md; this is the sixth and is unowned.

[mismatch] fixture bcm-dashboards-list-dashboards-max-results-above-range-rejected step task#1 (service bcm-dashboards, operation ListDashboards)
  Expected status 400, actual status 400 - the refusal itself is correct, including error code ValidationException.
  $.message: value_mismatch
    Expected: "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
    Actual:   "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"
  Message breakdown for the recorded (expected) form - fixed text: "1 validation error detected: Value at '" + member + "' failed to satisfy constraint: Member must have value less than or equal to " + bound. Variable parts: the violation count, the member name `maxResults` taken from the request, and the bound 100 taken from the member's modelled @range maximum. The emulator additionally interpolates "'" + rejected value + "' " after "Value"; the 101 there is the value this request submitted, not template text.
  No other key in the response diverged, and no other step of this fixture diverged.

Conflicting recording of the same message: open issue 20261113T142527Z-bcm-dashboards-listdashboards-maxresults-range-refusal-message-drops-the-rejected.md (corpus simparity-behavioral-aws-v1, check bcm-dashboards.list-dashboards-max-results-out-of-range-rejected) asserts the value clause MUST be present and is currently graded as failing when it is absent; previously-fixed issue 20261111T084409Z-bcm-dashboards-listdashboards-maxresults-range-refusal-message-interpolates.md asserted, from this same wire fixture step, that it must be ABSENT. The emulator presently satisfies the behavioral recording and diverges from this wire recording.

Smithy consulted: ListDashboards input {maxResults -> MaxResults integer, @default 20, @range 1..100; nextToken -> NextPageToken @length 1..8192 @pattern ^[\S\s]*$}, output {dashboards required, nextToken}, errors AccessDeniedException 403 / InternalServerException 500 / ThrottlingException 429 / ValidationException 400.

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it (health: /__admin/health returns 200).
2. Replay wire fixture bcm-dashboards-list-dashboards-max-results-above-range-rejected, step task#1 - equivalently, point a botocore bcm-dashboards client at the emulator and call list_dashboards(maxResults=101).
3. Confirm the refusal metadata matches: HTTP 400, error code ValidationException. That part passes.
4. Compare the `message` field. The fixture recorded "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100"; the emulator returns "1 validation error detected: Value '101' at 'maxResults' failed to satisfy constraint: Member must have value less than or equal to 100" - the extra "Value '<submitted value>' " clause is the only difference.
5. Call list_dashboards(maxResults=100) and confirm it still succeeds and returns a `dashboards` key (positive control: the bound itself is enforced correctly).
6. Read services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice/src/ops/list_dashboards.rs - its module head confirms the 1..100 refusal is applied by the shared boundary validator before `handle` runs, so the text comes from that validator's integer-@range formatter, not from this operation's handler.
7. Before editing the formatter, reconcile the two contradictory recordings of this one message: this wire step and behavioral check bcm-dashboards.list-dashboards-max-results-out-of-range-rejected. Record which is authoritative, align both, and re-replay this wire step AND that check together - changing only one will re-open the other.
