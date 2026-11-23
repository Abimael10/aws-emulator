Issue-Source: trace-sweep-divergence
Issue-Trace: cwp-describe-alarms-prefix-pagination
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cwp-describe-alarms-prefix-pagination cwp-list-alarm-mute-rules-pagination
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|DescribeAlarms|2xx->2xx|$.MetricAlarms[*]
Issue-Key: monitoring|ListAlarmMuteRules|2xx->2xx|$.AlarmMuteRuleSummaries[*]
Issue-Commit: c9b7fc7795b284972b7c862846d545eef7a83f7a

# CloudWatch paginated alarm listings ignore the NextToken cursor and re-serve the first page

> _Note: this issue was identified against the code base at commit `c9b7fc7795b284972b7c862846d545eef7a83f7a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlarms

## Description
2 causes, one shared fault: CloudWatch's @paginated alarm listings mint a NextToken but do not resume from it. When the continuation token the previous page returned is passed back, the handler serves the listing from the START again, so the exhausted final page comes back carrying the very items the earlier page already delivered instead of an empty remainder.

Cause 1 of 2: DescribeAlarms continued with the NextToken issued by the previous page of a prefix-scoped walk. AWS: 200 with the REMAINDER of the selection — when the listing is exhausted the page carries no alarm entries (CompositeAlarms, MetricAlarms and LogAlarms empty) and no NextToken. Smithy: DescribeAlarms is @paginated(inputToken: NextToken, outputToken: NextToken, pageSize: MaxRecords); input NextToken, output members CompositeAlarms/MetricAlarms/LogAlarms + NextToken; its only modeled error is InvalidNextToken (400, awsQueryError InvalidNextToken), which applies to a token the service did not mint, not to its own. Emulator: 200 whose MetricAlarms repeats the alarms the walk already returned (the replay reports MetricAlarms[0] and MetricAlarms[1] as 'extra', expected absent) — the AlarmNamePrefix+MaxRecords selection is recomputed from offset zero and the cursor in the token is not applied. Why: a client that walks DescribeAlarms while a NextToken is present either never terminates (every page hands back a full page plus a token) or returns each alarm repeatedly, so any inventory, dashboard or autoscaling consumer that enumerates more than one page of alarms double-counts them. Fix: DescribeAlarms' pagination handling in the CloudWatch crate — the continuation path must decode the inbound NextToken into the selection cursor and serve only items after it, returning an exhausted page with the three alarm lists empty and NextToken omitted. Note: open issue 20261029T203422Z-monitoring-cloudwatch-alarm-writes-and-pagination-skip-validation-or.md (Cause 6) names this same fixture step for the earlier shape of this defect (the token was REFUSED with 400); the token is now accepted but the page content is wrong, which is a distinct divergence its replay keys (DescribeAlarms|2xx->4xx|$.MetricAlarms) do not cover. Traces: cwp-describe-alarms-prefix-pagination#task#7.

Cause 2 of 2: ListAlarmMuteRules continued with the NextToken issued by the previous page. AWS: 200 with the remainder of the mute-rule selection — an exhausted page carries an empty AlarmMuteRuleSummaries and no NextToken. Smithy: ListAlarmMuteRules is @paginated(inputToken: NextToken, outputToken: NextToken, items: AlarmMuteRuleSummaries, pageSize: MaxRecords); errors InvalidNextToken (400) and ResourceNotFoundException (404). Emulator: 200 whose AlarmMuteRuleSummaries repeats the summaries already served (the replay reports AlarmMuteRuleSummaries[0] and [1] as 'extra', expected absent), i.e. the inbound token is ignored and the listing restarts. Why: a caller paging mute rules for an alarm sees the same rule ARNs on every page and cannot tell the walk is finished, so mute-rule reconciliation loops or applies duplicate rules. Fix: ListAlarmMuteRules' pagination handling in the CloudWatch crate — same cursor decode/resume contract as the other paginated listings. Traces: cwp-list-alarm-mute-rules-pagination#task#9.

Neither cause is about a generated id, timestamp or host: the divergence is the PRESENCE of already-returned items on a continuation page that AWS serves empty.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up, score 91.5%), 2 net-new clusters, neither owned by an open issue and neither blocked by a stub.

Cause 1 — DescribeAlarms, fixture cwp-describe-alarms-prefix-pagination step task#7, verdict mismatch, expected status 200 / actual 200:
  $.MetricAlarms[0]: extra (expected None, got the alarm named '<prefix>-a')
  $.MetricAlarms[1]: extra (expected None, got the alarm named '<prefix>-b')
Expected: the continuation page carries no alarm entries at all (CompositeAlarms/MetricAlarms/LogAlarms empty). Actual: both alarms created earlier in the fixture are served again, with their AlarmArn, state and CreationId — i.e. the first page replayed.

Cause 2 — ListAlarmMuteRules, fixture cwp-list-alarm-mute-rules-pagination step task#9, verdict mismatch, expected status 200 / actual 200:
  $.AlarmMuteRuleSummaries[0]: extra (expected None, got the summary for mute rule '<prefix>-mr-a', Status SCHEDULED, MuteType recurring)
  $.AlarmMuteRuleSummaries[1]: extra (expected None, got the summary for mute rule '<prefix>-mr-b')
Expected: an empty AlarmMuteRuleSummaries on the exhausted page. Actual: both mute-rule summaries re-served.

In both clusters the status and the envelope are correct; only the page CONTENT diverges, which is the signature of an inbound NextToken that is parsed/accepted but never used as a cursor.

## Steps to Reproduce
1. git clone the repo, build/boot the emulator, and replay the simparity-wire-aws-v1 wire corpus.
2. Cause 1: create several metric alarms sharing one AlarmNamePrefix, then call DescribeAlarms with that AlarmNamePrefix and a MaxRecords smaller than the number of alarms. Take the NextToken from the response and call DescribeAlarms again with the same prefix plus that NextToken until the walk should be exhausted (fixture cwp-describe-alarms-prefix-pagination, step task#7). Observe 200 with MetricAlarms containing the alarms the first page already returned, instead of the empty CompositeAlarms/MetricAlarms/LogAlarms AWS serves for an exhausted page.
3. Cause 2: create several alarm mute rules, call ListAlarmMuteRules with a MaxRecords smaller than their count, then replay the returned NextToken (fixture cwp-list-alarm-mute-rules-pagination, step task#9). Observe 200 with AlarmMuteRuleSummaries repeating the already-returned summaries instead of an empty list.
4. Re-replay both fixtures after the fix: each continuation page must contain only items after the cursor, and the exhausted page must carry the empty list(s) with no NextToken.
