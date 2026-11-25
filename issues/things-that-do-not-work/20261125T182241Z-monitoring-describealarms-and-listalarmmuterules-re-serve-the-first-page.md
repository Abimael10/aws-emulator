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
Issue-Commit: 6aa2a86c3a95f1aaa5bab53f9c9597e382f3838c

# DescribeAlarms and ListAlarmMuteRules re-serve the first page for an exhausted NextToken

> _Note: this issue was identified against the code base at commit `6aa2a86c3a95f1aaa5bab53f9c9597e382f3838c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlarms

## Description
2 causes, one shared fault: when a paginated CloudWatch list read is continued with a NextToken that has reached the end of the result set, the emulator does not return the empty remainder — it answers 200 with the FIRST page's items again, so a client walking the pages re-receives items it has already processed and the walk never terminates.

Cause 1 of 2: DescribeAlarms continued with the NextToken issued for the last page of an AlarmNamePrefix walk (fixture cwp-describe-alarms-prefix-pagination, step task#7).
AWS: Smithy: DescribeAlarms is @paginated(inputToken=NextToken, outputToken=NextToken, pageSize=MaxRecords) with output members CompositeAlarms, MetricAlarms, LogAlarms and error InvalidNextToken (httpError 400, awsQueryError InvalidNextToken). A token that denotes a position at or past the end of the listing is valid and resolves to the REMAINDER of the listing, which is empty: the recorded response is 200 with all three lists present and empty ([] for CompositeAlarms, LogAlarms and MetricAlarms) and no further NextToken. Each alarm must be returned at most once across the walk.
Emulator: 200 with MetricAlarms carrying the two alarms the earlier pages already returned (the prefix's '-a' and '-b' alarms), i.e. the continuation token is accepted but not used to advance — the full prefix-filtered list is served again from the start.
Why: a client paging DescribeAlarms (CLI --no-paginate loops, SDK paginators, inventory sweeps) either loops forever or processes every alarm twice, and an operator reading the last page concludes alarms exist that were already enumerated. The duplicates carry the same AlarmArn, so de-duplication by identity is the only way a client can survive, which it does not need on AWS.
Fix: DescribeAlarms' pagination in the CloudWatch crate — the handler must resolve the continuation token to an OFFSET into the filtered, ordered alarm list and return only the items after it (an exhausted token yielding empty CompositeAlarms/MetricAlarms/LogAlarms and no NextToken), instead of falling back to the unpaged list when the token points past the end.

Cause 2 of 2: ListAlarmMuteRules continued with the NextToken issued for the last page of a mute-rule walk (fixture cwp-list-alarm-mute-rules-pagination, step task#9).
AWS: Smithy: ListAlarmMuteRules is @paginated(inputToken=NextToken, outputToken=NextToken, items=AlarmMuteRuleSummaries, pageSize=MaxRecords) with errors InvalidNextToken (400) and ResourceNotFoundException (404). Same rule: an exhausted token resolves to the empty remainder — the recorded response carries no AlarmMuteRuleSummaries entries — and each mute rule appears at most once across the walk.
Emulator: 200 with AlarmMuteRuleSummaries carrying the two summaries the earlier pages already returned (the walk's '-mr-a' and '-mr-b' rules), i.e. the token is again ignored and the full list is re-served. (Aside, not asserted by this step: those summaries report MuteType 'recurring', while AlarmMuteRuleSummary$MuteType documents the valid values as ONE_TIME or RECURRING.)
Why: identical to Cause 1 for mute rules — a paging client re-reads rules it has handled, and an exhausted page looks like a fresh one, so suppression tooling that enumerates mute rules double-counts active windows.
Fix: ListAlarmMuteRules' pagination in the CloudWatch crate, the same token-to-offset resolution as Cause 1; both read paths share the defect, so fixing one without the other leaves the walk broken.

Relation to an open issue: 20261029T203422Z-monitoring-cloudwatch-alarm-writes-and-pagination-skip-validation-or.md, Cause 6, describes this same step (cwp-describe-alarms-prefix-pagination#task#7) when the emulator answered HTTP 400 and returned none of the three lists. The emulator now answers 200, so the refusal that issue names is gone and the replay no longer attributes this cluster to it (owned_by is empty): what remains is a different, net-new defect — the token is accepted and then ignored. It must be filed and fixed on its own; closing the 400 does not close this.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, 882 steps, score 94.1%). 2 net-new clusters, both service=monitoring, both with empty stub_path and empty owned_by.

1. DescribeAlarms, cwp-describe-alarms-prefix-pagination#task#7 — status expected 200, got 200; divergence keys $.MetricAlarms[0], $.MetricAlarms[1]: 'extra (expected None, got ...)'. The emulator returned two MetricAlarms — AlarmName 'sim2real-probe-cwp-alpg-<suffix>-a' and '...-b', AlarmArn 'arn:aws:cloudwatch:us-west-2:000000000000:alarm:sim2real-probe-cwp-alpg-<suffix>-a' / '-b', StateValue INSUFFICIENT_DATA, Namespace 'Sim2Real/Probe', MetricName 'cwp-alpg' — where the recorded response carries no alarm entries at all. (Names, ARNs, CreationIds and timestamps are per-run values; the requirement is that the exhausted page carries no entries.)

2. ListAlarmMuteRules, cwp-list-alarm-mute-rules-pagination#task#9 — status expected 200, got 200; divergence keys $.AlarmMuteRuleSummaries[0], $.AlarmMuteRuleSummaries[1]: 'extra (expected None, got ...)'. The emulator returned two summaries — AlarmMuteRuleArn 'arn:aws:cloudwatch:us-west-2:000000000000:alarm-mute-rule:sim2real-probe-cwp-mrpg-<suffix>-mr-a' / '-mr-b', Status 'SCHEDULED', MuteType 'recurring' — where the recorded response carries no summaries.

In both fixtures the preceding page-by-page steps pass, so the token round trip works until the listing is exhausted; the failure is confined to the continuation that should yield the empty remainder.

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
2. Cause 1: replay cwp-describe-alarms-prefix-pagination — create several alarms sharing an AlarmNamePrefix, then call DescribeAlarms with that AlarmNamePrefix and a small MaxRecords, feeding each response's NextToken into the next call. At task#7 (the continuation after the last item has been returned) the emulator answers 200 with the prefix's alarms again instead of empty CompositeAlarms/MetricAlarms/LogAlarms.
3. Cause 2: replay cwp-list-alarm-mute-rules-pagination — create several alarm mute rules, walk ListAlarmMuteRules with MaxRecords plus NextToken. At task#9 the emulator answers 200 with the first page's AlarmMuteRuleSummaries again instead of an empty list.
