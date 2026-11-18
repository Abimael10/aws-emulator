Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__logs.emf-log-to-metric-extraction-exactness
Issue-Key: cloudwatch__logs.emf-log-to-metric-extraction-exactness|_one#60|AssertionError|assert len(dps) == 1, (
Issue-Commit: 9254f34ecb4a771a65aae1ae3508a84017f71601

# cloudwatch__logs: a regular-resolution metric datapoint is invisible to any sub-minute GetMetricStatistics read instead of being reported at its stored minute boundary

> _Note: this issue was identified against the code base at commit `9254f34ecb4a771a65aae1ae3508a84017f71601`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
One net-new root cause, grounded in the single failing check listed under `### cloudwatch__logs` that no ALREADY FILED / OWNED marker claims. Cause count: 1 — it must be closed before this issue is submitted.

RELATED OPEN ISSUE (it carries this check id in its Issue-Trace-Ids but NOT its current replay key, and it describes a different, now-superseded state of the emulator): 20261104T143156Z-cloudwatch-logs-cloudwatch-emf-putlogevents-of-embedded-metric-format-documents.md says the EMF extractor never runs at all, so ListMetrics and GetMetricStatistics stay permanently empty. That is no longer what the replay shows: EMF extraction DOES run now — the check gets all the way past the Period=60 aggregates for both extracted metrics, past both declared dimension sets, past the metric catalogue, and past the twelve one-second high-resolution OrderLatency buckets. The surviving divergence is in the metric READ path, not in EMF ingest, and it reproduces for any regular-resolution series regardless of how it was published (PutMetricData without StorageResolution, or EMF extraction of a metric declared without StorageResolution).

=== Cause 1 of 1: a read whose Period is finer than 60s drops regular-resolution datapoints entirely ===
AWS: Smithy — `GetMetricStatisticsInput$Period` (range 1..) documents "For metrics with regular resolution, a period can be as short as one minute (60 seconds) and must be a multiple of 60. For high-resolution metrics that are collected at intervals of less than one minute, the period can be 1, 5, 10, 20, 30, 60, or any multiple of 60", and `MetricDatum$StorageResolution` documents that a regular-resolution metric is stored "at 1-minute resolution". A regular-resolution series therefore holds exactly one aggregated datapoint per whole minute, stamped on that minute's boundary. Recorded behavior for the read: GetMetricStatistics with Period=1 over a window containing that minute returns HTTP 200 with exactly ONE Datapoint for the series, whose Timestamp is the minute boundary (not any sub-minute instant inside it) and whose Sum/SampleCount are the full minute's aggregate — AWS answers a sub-minute read of a regular-resolution series with the minute-granularity datapoint it actually holds, it does not suppress the series. Period=1 is not refused (the model's range is 1.., and the sibling high-resolution read in the same check is served at Period=1), and the StartTime here is well inside the 3-hour sub-minute retention band, so no retention tier is in play.
Emulator: the same read returns HTTP 200 with `Datapoints: []` — zero datapoints — for the regular-resolution series, while the high-resolution sibling series in the identical window is returned correctly as twelve one-second buckets. The regular-resolution datapoint is not merely mis-stamped or mis-aggregated; it is selected out of the result set altogether, so the series looks like it has no data at all for a window in which it demonstrably does (the same series, read at Period=60, returns its single bucket with the correct Sum, SampleCount and Unit).
Why: a client cannot tell "this series has no data in this window" from "you asked at a period my store will not answer at". Any caller that reads a mixed-resolution namespace at a single sub-minute period — a dashboard or console widget whose refresh interval resolves to Period<60, an alarm or autoscaling policy evaluated at a high-resolution period over a namespace that also carries regular-resolution metrics, a metric-math/GetMetricData expression spanning both kinds — silently sees the regular-resolution half of its data as missing. Downstream that is indistinguishable from no traffic: alarms sit in INSUFFICIENT_DATA, scaling never triggers, and totals are wrong rather than absent.
Fix: the storage-granularity gate in the CloudWatch metric read path — the window fold shared by the metric readers (`windows_by_unit`, and therefore `windows_of`) in services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state.rs, reached from the GetMetricStatistics handler at services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/get_metric_statistics.rs. It currently SKIPS any datum whose stored aggregated span exceeds the requested period; instead such a datum must still be reported, in the window its own stored span starts in, i.e. at its minute boundary, so a sub-minute read of a regular-resolution series yields one minute-stamped bucket rather than nothing. Because the same helper backs GetMetricData, fix it once in the shared fold rather than per operation.
Traces: cloudwatch__logs.emf-log-to-metric-extraction-exactness.

## Evidence
[mismatch] cloudwatch__logs.emf-log-to-metric-extraction-exactness
  AssertionError: OrderCount under [{'Name': 'Service', 'Value': 'checkout'}, {'Name': 'Operation', 'Value': 'PlaceOrder'}] at Period=1 returned 0 datapoints, expected exactly 1: []

What had already PASSED before this assertion, which is what isolates the divergence to the sub-minute read and not to EMF extraction:
  * PutLogEvents of the EMF documents returned 200 with no `rejectedLogEventsInfo`.
  * GetMetricStatistics(MetricName=OrderCount, Dimensions=[Service,Operation], Period=60) returned exactly one datapoint with Sum == SampleCount == <N documents> and Unit 'Count'.
  * GetMetricStatistics(MetricName=OrderLatency, same dimensions, Period=60) returned the correct Sum/Minimum/Maximum/SampleCount and Unit 'Milliseconds'.
  * Both declared dimension sets ([Service,Operation] and [Service]) materialised as independent series, and ListMetrics for the namespace returned exactly the declared projections.
  * GetMetricStatistics(MetricName=OrderLatency, Period=1) — the HIGH-resolution series (declared StorageResolution: 1) — returned <N> one-second buckets at the expected one-second boundaries with SampleCount 1.0 each.

Expected (AWS) for the failing read: GetMetricStatistics(Namespace=<ns>, MetricName=OrderCount, Dimensions=[{Service:<svc>},{Operation:<op>}], Period=1, StartTime=<a minute before the data's minute>, EndTime=<now+5m>, Statistics=[Sum,SampleCount,Minimum,Maximum]) -> 200 with exactly ONE Datapoint, Timestamp == the whole-minute boundary the documents fell in, Sum == SampleCount == the number of published documents, Unit as declared. Actual (emulator): 200 with `Datapoints: []`.

The metric name, namespace, dimension names/values, document count and the particular minute are values supplied by the request and the published data; the requirement is the shape of the answer — one datapoint, stamped at the stored minute boundary, carrying the whole minute's aggregate — for ANY regular-resolution series read at a period finer than 60 seconds.

## Steps to Reproduce
1. Publish a REGULAR-resolution datapoint and a HIGH-resolution datapoint into the same namespace, inside one whole minute. Either publisher reproduces it; the check uses EMF: logs.CreateLogGroup + logs.CreateLogStream, then logs.PutLogEvents of documents whose message is {"_aws":{"Timestamp":<ms>,"CloudWatchMetrics":[{"Namespace":<ns>,"Dimensions":[["Service","Operation"],["Service"]],"Metrics":[{"Name":"OrderLatency","Unit":"Milliseconds","StorageResolution":1},{"Name":"OrderCount","Unit":"Count"}]}]},"Service":"checkout","Operation":"PlaceOrder","OrderLatency":<v>,"OrderCount":1} at distinct one-second offsets inside one minute. Equivalently: cloudwatch.PutMetricData with one MetricDatum carrying StorageResolution=1 and one carrying no StorageResolution.
2. Confirm the series exist: cloudwatch.GetMetricStatistics(Namespace=<ns>, MetricName='OrderCount', Dimensions=[{Name:'Service',Value:'checkout'},{Name:'Operation',Value:'PlaceOrder'}], Period=60, StartTime=<minute-60s>, EndTime=<now+5m>, Statistics=['Sum','SampleCount','Minimum','Maximum']) -> one datapoint with the full aggregate. (Passes today.)
3. Confirm the high-resolution read works at a sub-minute period: the same call with MetricName='OrderLatency' and Period=1 -> one bucket per second. (Passes today.)
4. Now repeat step 2 with Period=1 instead of 60, everything else identical -> OBSERVE `Datapoints: []`; EXPECT exactly one datapoint, Timestamp == the minute boundary, Sum == SampleCount == the number of published samples, Unit == the declared unit.
