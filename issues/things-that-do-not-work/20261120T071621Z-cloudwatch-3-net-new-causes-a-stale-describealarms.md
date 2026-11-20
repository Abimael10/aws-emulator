Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.describe-alarms-invalid-next-token-is-ignored cloudwatch.describe-alarms-state-filter-and-invalid-next-token-ignored cloudwatch.op-contract-describeAlarms-dup cloudwatch.get-metric-statistics-and-toomany-datapoints-rejected cloudwatch.get-metric-statistics-too-many-datapoints-rejected cloudwatch.op-contract-getMetricStatistics-dup cloudwatch.metric-data-sub-minute-resolution-3-hour-expiry-boundary cloudwatch.metrics-math-and-emf-period-buckets-anchor-at-minute-rounded-start-time cloudwatch.hourly-buckets-from-a-start-time-over-15-days-old-anchor-on-the-5-minute-clock
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.describe-alarms-invalid-next-token-is-ignored|run#10|AssertionError|assert token_names == baseline_names, (
Issue-Key: cloudwatch.describe-alarms-state-filter-and-invalid-next-token-ignored|run#18|AssertionError|assert names_token == {alarm_ok, alarm_in_alarm}, (
Issue-Key: cloudwatch.op-contract-describeAlarms-dup|run#18|AssertionError|assert names_token == {alarm_ok, alarm_in_alarm}, (
Issue-Key: cloudwatch.get-metric-statistics-and-toomany-datapoints-rejected|run#15|AssertionError|assert message == expected, f"expected the exact message {expected!r}, got {message!r}"
Issue-Key: cloudwatch.get-metric-statistics-too-many-datapoints-rejected|run#10|AssertionError|assert message == expected, f"expected the exact message {expected!r}, got {message!r}"
Issue-Key: cloudwatch.op-contract-getMetricStatistics-dup|run#17|AssertionError|assert message == expected, f"expected the exact message {expected!r}, got {message!r}"
Issue-Key: cloudwatch.metric-data-sub-minute-resolution-3-hour-expiry-boundary|run#13|AssertionError|assert error == ("InvalidParameterCombination", (
Issue-Key: cloudwatch.metrics-math-and-emf-period-buckets-anchor-at-minute-rounded-start-time|run#9|AssertionError|assert gms == want, (
Issue-Key: cloudwatch.hourly-buckets-from-a-start-time-over-15-days-old-anchor-on-the-5-minute-clock|run#16|AssertionError|assert got == expected(recent_anchor), (
Issue-Commit: 2f24e314505ec85ef2a85cab010fe998301170cd

# cloudwatch: 3 net-new causes — a stale DescribeAlarms NextToken empties the page, the 1,440-datapoint refusal over-counts, and period buckets anchor on the clock instead of the rounded StartTime

> _Note: this issue was identified against the code base at commit `2f24e314505ec85ef2a85cab010fe998301170cd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlarms

## Description
Three independent defects in the cloudwatch metric read path, all grounded in failing behavioral checks of this run. Each cause is numbered and carries its own failing check ids; all three must be closed before this issue is submitted.

Cause 1 of 3: DescribeAlarms with an unrecognized NextToken answers 200 with an EMPTY alarm list instead of the first page.
AWS: Smithy (cloudwatch, DescribeAlarms) models NextToken: NextToken, @paginated{inputToken: NextToken, outputToken: NextToken, pageSize: MaxRecords}, and the single error InvalidNextToken (client, httpError 400, awsQueryError code InvalidNextToken). Recorded behavior: a token AWS cannot parse is NOT refused on this operation — the call succeeds (HTTP 200) and is answered as if no token had been sent, i.e. the first page, still scoped by the request's AlarmNames/StateValue filters. So a request naming alarms A and B with NextToken set to an arbitrary string returns exactly the alarms of A and B that match the other filters.
Emulator: the same request succeeds (HTTP 200, no error) but MetricAlarms is empty — the replay reads set() where the identical call without the token returns the named alarm(s). It neither honours the token nor ignores it: it drops every record.
Why: a client that resumes a listing with a token it kept from an earlier (possibly expired) page, or that passes through a token from another CloudWatch call, is told the account has no alarms. Reconciliation loops delete or re-create alarms they believe are gone, and paginators terminate after the first stale token with an incomplete result instead of erroring or restarting.
Fix: the DescribeAlarms handler's NextToken decoding/pagination in the cloudwatch service crate — an undecodable token must fall back to offset 0 rather than to an empty result set (and must not be allowed to bypass the AlarmNames/StateValue filtering).
Traces: cloudwatch.describe-alarms-invalid-next-token-is-ignored, cloudwatch.describe-alarms-state-filter-and-invalid-next-token-ignored, cloudwatch.op-contract-describeAlarms-dup.

Cause 2 of 3: the GetMetricStatistics 1,440-datapoint refusal names a datapoint count the request did not ask for.
AWS: Smithy (GetMetricStatistics) documents "The maximum number of data points returned from a single call is 1,440. If you request more than 1,440 data points, CloudWatch returns an error", and models InvalidParameterCombinationException (client, httpError 400, awsQueryError code InvalidParameterCombination). The message is fixed text with one variable part — the requested datapoint count, derived ONLY from the request's StartTime, EndTime and Period: "You have requested up to <count> datapoints, which exceeds the limit of 1440. You may reduce the datapoints requested by increasing Period, or decreasing the time range." For a 10-day window at Period=60 <count> is 14400; for a 90-day window at Period=60 it is 129600; for a 4 h 1 min window at Period=1 it is 14460.
Emulator: the status (400) and the code (InvalidParameterCombination) match, and the fixed text matches, but <count> is larger than the request implies and varies between runs of the same request shape: 14401 for the 10-day/Period=60 case, 129604 and 129608 for two 90-day/Period=60 cases, 14503 for the 4 h 1 min/Period=1 case. Hypothesis: the count is not computed from the request window alone — the over-count tracks how much wall-clock time elapsed inside the check before the call, so the handler appears to substitute its own current time for the request's EndTime (and/or to round the span up by a whole extra period).
Why: the count is a requirement of the message, and clients parse it to decide how far to narrow the window or how much to raise Period before retrying. A count that exceeds the request (and changes run to run for an identical request) makes that retry arithmetic wrong and makes the refusal non-deterministic for anything asserting on it.
Fix: the datapoint-budget check in the GetMetricStatistics handler/validation of the cloudwatch service crate — compute the requested count from the request's StartTime, EndTime and Period only, and render it into the fixed message text.
Traces: cloudwatch.get-metric-statistics-and-toomany-datapoints-rejected, cloudwatch.get-metric-statistics-too-many-datapoints-rejected, cloudwatch.op-contract-getMetricStatistics-dup, cloudwatch.metric-data-sub-minute-resolution-3-hour-expiry-boundary.

Cause 3 of 3: period buckets are anchored on absolute clock multiples instead of on the rounded StartTime, so both GetMetricStatistics and GetMetricData return the wrong bucket stamps and the wrong per-bucket aggregates.
AWS: Smithy (GetMetricStatisticsInput$StartTime) states the rounding rule: "CloudWatch rounds the specified time stamp as follows: Start time less than 15 days ago - Round down to the nearest whole minute ...; Start time between 15 and 63 days ago - Round down to the nearest 5-minute clock interval ...; Start time greater than 63 days ago - Round down to the nearest 1-hour clock interval". Buckets then run in Period-long steps FROM that rounded StartTime, so a Period=300 query starting at B+2m yields buckets stamped B+2m and B+7m, a Period=3600 query starting at 03:13:34 yields buckets stamped 03:13, 04:13, 05:13, 06:13, and a Period=120 query from B+1m yields odd-minute buckets that no 120 s clock grid contains. GetMetricData returns the same stamps and sums for the same window/period.
Emulator: buckets are anchored on epoch/clock multiples of Period, ignoring the StartTime anchor — the Period=300-from-B+2m query answers buckets stamped B+0m and B+5m (sums 15.0/40.0 instead of 25.0/27.0), and the Period=3600-from-03:13:34 query answers 04:00, 05:00, 06:00 (counts 48/60/52) instead of 03:13, 04:13, 05:13, 06:13 (counts 1/60/60/39). Both GetMetricStatistics and GetMetricData diverge the same way, so the samples are attributed to the wrong buckets as well as stamped wrongly.
Why: every consumer that windows a series by StartTime — alarm backfills, dashboards, metric math, rate derivations, batch exports — reads aggregates split across the wrong boundaries. Sums and SampleCounts per bucket are wrong (not merely shifted), so a client comparing two adjacent windows sees values that never existed, and the 15-day/63-day rounding tiers are untestable.
Fix: the period-bucketing/aggregation code shared by the GetMetricStatistics and GetMetricData handlers in the cloudwatch service crate — derive the bucket grid from StartTime rounded per the documented tier, not from clock multiples of Period.
Traces: cloudwatch.metrics-math-and-emf-period-buckets-anchor-at-minute-rounded-start-time, cloudwatch.hourly-buckets-from-a-start-time-over-15-days-old-anchor-on-the-5-minute-clock.

## Evidence
Cause 1 — cloudwatch.describe-alarms-invalid-next-token-is-ignored:
  expected: the garbage NextToken ignored and the same first page returned
  actual:   AssertionError: got set() vs baseline {'<the alarm the check created>'} (HTTP 200, MetricAlarms empty)
  cloudwatch.describe-alarms-state-filter-and-invalid-next-token-ignored / cloudwatch.op-contract-describeAlarms-dup:
  expected: both named alarms returned for DescribeAlarms(AlarmNames=[A,B], NextToken='garbage-not-a-real-token')
  actual:   AssertionError: expected the garbage NextToken to be ignored and both alarms returned, got set()

Cause 2 — identical status 400 / code InvalidParameterCombination, message count diverges:
  10 days @ Period=60:   expected '...up to 14400 datapoints...'   actual '...up to 14401 datapoints...'
  90 days @ Period=60:   expected '...up to 129600 datapoints...'  actual '...up to 129604 datapoints...' (one check) and '...up to 129608 datapoints...' (the op-contract duplicate of the same request shape)
  4 h 1 min @ Period=1:  expected '...up to 14460 datapoints...'   actual '...up to 14503 datapoints...'
  Fixed text (identical on both sides): 'You have requested up to <count> datapoints, which exceeds the limit of 1440. You may reduce the datapoints requested by increasing Period, or decreasing the time range.' The only variable part is <count>, which AWS derives from StartTime/EndTime/Period of the request.

Cause 3 — cloudwatch.metrics-math-and-emf-period-buckets-anchor-at-minute-rounded-start-time (B = a 10-minute clock mark, ten 1-minute samples 1..10 from B):
  GetMetricStatistics Period=300 from B+2m
  expected: [(B+2m, 25.0), (B+7m, 27.0)]
  actual:   [(B+0m, 15.0), (B+5m, 40.0)]
  cloudwatch.hourly-buckets-from-a-start-time-over-15-days-old-anchor-on-the-5-minute-clock (StartTime 03:13:34, Period=3600):
  expected: [(03:13, 1, 40.0), (04:13, 60, 2748.0), (05:13, 60, 2760.0), (06:13, 39, 1794.0)]
  actual:   [(04:00, 48, 2190.0), (05:00, 60, 2760.0), (06:00, 52, 2392.0)]
  (the hour/minute values above are one run's clock; the requirement is the anchor rule, not these stamps)

## Steps to Reproduce
Cause 1: PutMetricAlarm two alarms A and B (ActionsEnabled=false). Call DescribeAlarms(AlarmNames=[A,B]) and keep the result. Call DescribeAlarms(AlarmNames=[A,B], NextToken='garbage-not-a-real-token'). AWS returns the same alarms (HTTP 200); the emulator returns HTTP 200 with MetricAlarms empty.

Cause 2: With any namespace/metric, call GetMetricStatistics(StartTime=now-10d, EndTime=now, Period=60, Statistics=['Average']). The call is refused 400 InvalidParameterCombination on both sides; compare the datapoint count in the message with (EndTime-StartTime)/Period = 14400. Repeat with StartTime=now-90d (expect 129600) and with StartTime=now-4h, EndTime=now+1m, Period=1 (expect 14460). Insert a delay before the refused call and observe the emulator's reported count grow.

Cause 3: Pick B = a whole 10-minute clock mark ~20 minutes ago. PutMetricData ten points at B+0m..B+9m with values 1..10. Then call GetMetricStatistics(StartTime=B+2m, EndTime=B+12m, Period=300, Statistics=['Sum']) and GetMetricData with the same window and Period: AWS answers buckets stamped B+2m (Sum 25) and B+7m (Sum 27); the emulator answers B+0m (15) and B+5m (40). Repeat with Period=120 from B+1m (AWS: odd-minute buckets) and with a Period=3600 query whose StartTime is at MM:13:34 (AWS: buckets stamped at :13).
