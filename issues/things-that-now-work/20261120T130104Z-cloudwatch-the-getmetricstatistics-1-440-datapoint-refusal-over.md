Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.metric-data-1-minute-resolution-retention-boundary
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.metric-data-1-minute-resolution-retention-boundary|run#14|AssertionError|assert error == ("InvalidParameterCombination", (
Issue-Commit: 459f7f80ec551516a30300bc9566395bea63dc1e

# cloudwatch: the GetMetricStatistics 1,440-datapoint refusal over-counts the requested datapoints by one period

> _Note: this issue was identified against the code base at commit `459f7f80ec551516a30300bc9566395bea63dc1e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
Cause 1 of 1: the datapoint count rendered into the InvalidParameterCombination refusal of GetMetricStatistics is one period too large, so the refusal names a datapoint count the request never asked for. Failing check ids: cloudwatch.metric-data-1-minute-resolution-retention-boundary.

NOTE: open issue 20261120T071621Z-cloudwatch-3-net-new-causes-a-stale-describealarms.md describes the same over-counting defect as its Cause 2, but its Issue-Trace-Ids / Issue-Key list does NOT carry this check's replay key (it carries cloudwatch.get-metric-statistics-and-toomany-datapoints-rejected, cloudwatch.get-metric-statistics-too-many-datapoints-rejected, cloudwatch.op-contract-getMetricStatistics-dup, cloudwatch.metric-data-sub-minute-resolution-3-hour-expiry-boundary), and the suite did not mark this check as owned by it. It is filed here so this check is re-replayed; a fixer closing that issue's Cause 2 should verify this check too. This failure also shows the over-count is NOT only wall-clock drift as that issue hypothesises: here it is exactly one extra period for a window whose end lies in the future, i.e. an inclusive/ceil-plus-one bucket count.

AWS: Smithy (cloudwatch, GetMetricStatistics) models the required members StartTime, EndTime and Period (Period range min 1) and the client error InvalidParameterCombinationException (errorKind client, httpError 400, awsQueryError code InvalidParameterCombination, httpResponseCode 400). GetMetricStatistics returns at most 1,440 datapoints per call and refuses a request that would need more. The message is fixed text with exactly one variable part, the requested datapoint count, which AWS derives ONLY from the request's StartTime, EndTime and Period as the number of Period-long buckets spanning [rounded StartTime, EndTime): "You have requested up to <count> datapoints, which exceeds the limit of 1440. You may reduce the datapoints requested by increasing Period, or decreasing the time range." StartTime is rounded down per the documented tiers (Smithy GetMetricStatisticsInput$StartTime: "Start time less than 15 days ago - Round down to the nearest whole minute"). For a window of StartTime = now-14 days (whole minute) to EndTime = now + 1 minute at Period=60, the span is 14*1440+1 = 20161 minutes, so <count> is 20161.

Emulator: the status (400), the error code (InvalidParameterCombination) and the fixed text all match, but <count> is one period more than the request implies: the replay reads <count> = 20162 where the request spans 20161 whole minutes at Period=60. Hypothesis: the budget is computed as an inclusive bucket count (floor(span/Period)+1, or ceil of a span rounded up by a whole extra period) and/or the handler clamps EndTime to its own current time rather than using the request's EndTime, instead of the exclusive bucket count AWS reports.

Why: the count is a requirement of the message, and clients parse it to decide how much to raise Period or how far to narrow the window before retrying. A count larger than the request implies makes that retry arithmetic wrong, makes the refusal non-reproducible for anything asserting the exact message, and — at the boundary — can report a count over the 1,440 cap for a request that is exactly at the cap.

Fix: the datapoint-budget validation in the GetMetricStatistics handler of the cloudwatch service crate — compute the count as the number of Period buckets over the request's own rounded StartTime and EndTime (exclusive of the end bucket), and render that value into the fixed message text. The same budget computation must be used for both the accept/refuse decision and the message.

Traces: cloudwatch.metric-data-1-minute-resolution-retention-boundary (behavioral suite, service heading `cloudwatch`).

## Evidence
Check cloudwatch.metric-data-1-minute-resolution-retention-boundary (mismatch):
  request: PutMetricData one point, then GetMetricStatistics(Namespace=<unique ns>, MetricName='StandardResMetric', Dimensions=[{Name:'Case',Value:<unique>}], StartTime=now-14d, EndTime=now+1m, Period=60, Statistics=['Average'])
  expected (AWS): HTTP 400, Code 'InvalidParameterCombination', Message 'You have requested up to 20161 datapoints, which exceeds the limit of 1440. You may reduce the datapoints requested by increasing Period, or decreasing the time range.'
  actual (emulator): HTTP 400, Code 'InvalidParameterCombination', Message 'You have requested up to 20162 datapoints, which exceeds the limit of 1440. You may reduce the datapoints requested by increasing Period, or decreasing the time range.'
  AssertionError: expected the InvalidParameterCombination rejection for a 60s-Period query spanning 14 days, got ('InvalidParameterCombination', 'You have requested up to 20162 datapoints, ...')

Message anatomy — fixed text: 'You have requested up to ', ' datapoints, which exceeds the limit of 1440. You may reduce the datapoints requested by increasing Period, or decreasing the time range.' and the literal limit 1440. Variable part: the single datapoint count, computed from the request's StartTime, EndTime and Period. 20161 / 20162 are this run's values for a 14-day + 1-minute window at Period=60; they are not themselves the requirement — the requirement is that the count equals the request's own bucket count.

## Steps to Reproduce
1. PutMetricData one datapoint (Value=333.0, Unit=Count) into a fresh namespace with MetricName='StandardResMetric' and Dimensions=[{Name:'Case',Value:<unique>}], Timestamp = now-10s.
2. Confirm the narrow query works: GetMetricStatistics with StartTime=now-5m, EndTime=now+1m, Period=60, Statistics=['Average'] returns the Average 333.0 datapoint (this part passes).
3. Issue the over-cap query: GetMetricStatistics with the same namespace/metric/dimensions, StartTime=now-14d, EndTime=now+1m, Period=60, Statistics=['Average'].
4. Both AWS and the emulator refuse with HTTP 400 / InvalidParameterCombination. Compare the datapoint count in the message with the request's own bucket count (EndTime - rounded StartTime)/Period = 20161: AWS reports 20161, the emulator reports 20162.
5. Repeat with other spans that are an exact multiple of Period (e.g. StartTime=now-10d, EndTime=now, Period=60 -> 14400) to confirm the count is consistently one period high rather than drifting.
