Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatch-metric-alarm-history-and-contributors
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatch-metric-alarm-history-and-contributors
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|DescribeAlarmContributors|2xx->4xx|$.AlarmContributors
Issue-Key: monitoring|DescribeAlarmContributors|2xx->4xx|status
Issue-Commit: 0771e25bba8212ca0e4339575fc1faa7cfb6765d

# CloudWatch DescribeAlarmContributors refuses a contributor read with HTTP 400 instead of answering 200 with an empty AlarmContributors list

> _Note: this issue was identified against the code base at commit `0771e25bba8212ca0e4339575fc1faa7cfb6765d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlarmContributors

## Description
One net-new wire divergence on CloudWatch: a DescribeAlarmContributors read that AWS answers successfully is refused by the emulator with HTTP 400.

Cause 1 of 1: DescribeAlarmContributors on an existing alarm that currently has no contributors answers HTTP 400 instead of HTTP 200 with an empty contributor list.
AWS: Smithy: DescribeAlarmContributors takes AlarmName (@required, @length min 1 max 255) plus NextToken, and its output member AlarmContributors is @required (a list) alongside an optional NextToken; the only modeled errors are InvalidNextToken (400, awsQueryError InvalidNextToken) and ResourceNotFoundException (404, awsQueryError ResourceNotFoundException). Because AlarmContributors is a required output list, a successful call with nothing to report must still serialize the member as an empty list rather than omit it; there is no modeled 400 for 'this alarm has no contributors'. The recorded response is HTTP 200 with body {"AlarmContributors": []} (no NextToken). Recorded behavior only for the precondition the emulator is applying: nothing in the model refuses this request.
Emulator: answers HTTP 400 for the same request and emits no AlarmContributors member at all (replay reports `status: expected 200, got 400` and `$.AlarmContributors: missing (expected [], got None)`). Hypothesis: the handler's alarm-type/contributor precondition is inverted or over-applied — the same handler wrongly SUCCEEDS on a plain metric alarm that AWS refuses (the already-open issue 20261108T134822Z-monitoring-cloudwatch-read-and-write-fidelity-describealarmcontributors-skips-its.md, cause 1, covers that opposite direction on task#4 of this very fixture), so the gate appears to be keyed on whether the contributor projection is non-empty rather than on the alarm's type. That open issue does NOT carry this cluster's replay key (the replay report lists this cluster with owned_by=[]), and its cause 1 asserts the reverse expectation (400 where the emulator gives 200), so fixing it as written would not make this step pass.
Why: a client enumerating contributors for an alarm it is allowed to read gets a hard client-error refusal instead of an empty page. Because the refusal is a 4xx, SDK retry logic does not retry it and the caller surfaces it as a configuration/validation failure: contributor dashboards, alarm-triage tooling and any paginator that loops until NextToken is absent abort on an alarm that simply has no contributors yet, instead of rendering an empty result. It also hides the real refusal contract — a caller cannot distinguish 'no contributors' from the modeled ResourceNotFoundException (404) or InvalidNextToken (400).
Fix: the DescribeAlarmContributors handler's input validation / response projection — stop refusing the request when the contributor projection is empty, and serialize the required AlarmContributors member as [] with HTTP 200; keep 400 only for InvalidNextToken and 404 only for a missing alarm (the alarm-type ValidationException belongs on the plain-alarm path tracked by the related issue above).
Traces: cloudwatch-metric-alarm-history-and-contributors#task#5.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (clone branch issues/finder-wi-3217d22cbdf742998f0f4ec5cb82bf1c-1794854092153). Summary: steps_total 859, steps_passed 746, steps_mismatched 89, score_pct 86.8, bootstrap_signal false, emulator_up true. This cluster reports stub_path='' and owned_by=[] — i.e. net-new, owned by no open issue.

Cause 1 — fixture cloudwatch-metric-alarm-history-and-contributors, step task#5, operation DescribeAlarmContributors, verdict mismatch, affected_count 1:
  status: expected 200, got 400
  $.AlarmContributors: missing (expected [], got None)

divergence_key: service=monitoring|DescribeAlarmContributors|status=200->400|keys=$.AlarmContributors,status

Requirement: HTTP 200 and a body whose required AlarmContributors member is present and empty ([]) when the alarm has no contributors; NextToken omitted. The empty list is the whole contract here — no error code or message is expected, so any 4xx body on this path is a defect regardless of its __type.

Model citation: smithy_query query=operation operation=DescribeAlarmContributors on services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json — input {AlarmName required, length 1..255; NextToken}, output {AlarmContributors required list; NextToken}, errors [InvalidNextToken 400, ResourceNotFoundException 404].

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Cause 1: fixture cloudwatch-metric-alarm-history-and-contributors, step task#5 — DescribeAlarmContributors for the alarm created earlier in that fixture must answer HTTP 200 with body {"AlarmContributors": []} (required member present and empty, no NextToken), not HTTP 400 with the member absent.
4. Re-replay cloudwatch-metric-alarm-history-and-contributors#task#5 after the fix; it must pass. Note that task#4 of the same fixture asserts the OPPOSITE direction for a plain metric alarm (400 ValidationException) and is tracked by 20261108T134822Z-monitoring-cloudwatch-read-and-write-fidelity-describealarmcontributors-skips-its.md — both steps must pass together, so gate on the alarm's type rather than on whether the contributor list is empty.

## Disposition

Cause 1 — FIXED, and now pinned by a test. The checkout drifted since the report was written:
`DescribeAlarmContributors` gates on the ALARM's TYPE (whether its configuration grades a vector
of series) and not on whether the contributor projection is empty
(`src/ops/describe_alarm_contributors.rs`), so an alarm that has contributors to report but none
in `ALARM` right now is answered HTTP 200 with the required `AlarmContributors` member present and
empty and no `NextToken`, while the two modelled refusals stay where the model puts them
(`InvalidNextToken` 400, `ResourceNotFoundException` 404) and the alarm-type refusal stays on the
plain-alarm path of the related issue. Both replay keys of this cluster pass, and every step of
the fixture — task#4's refusal and task#5's empty page together — replays green.

`tests/all/empty_contributor_page.rs` is the new suite that holds it: a table over the form the
query is stated in and over WHY nothing is in `ALARM` (as configured, each of the other states a
caller can force, after leaving `ALARM`, and with every resolved entry graded under the
threshold, which is the case where the store holds contributors and none is in `ALARM`), each row
asserting the 200, the present and empty required member, the absent token, and that the read
moves neither the page nor the alarm; plus one alarm read over each protocol the model declares
(`awsJson1_0`, `awsQuery`, `rpcv2Cbor`), since "present and empty" is a claim about the wire.
