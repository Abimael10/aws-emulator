Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__opensearch.obs-opensearch-1-domain-metrics-track-cluster-health-and-documents
Issue-Key: cloudwatch__opensearch.obs-opensearch-1-domain-metrics-track-cluster-health-and-documents|run#73|TimeoutError|raise _not_graded(rate_start, POLL_DEADLINE_S, "SearchRate to count the searches")
Issue-Commit: 634fb37e35f51bdb636290ffeec5191f736cf22e

# cloudwatch__opensearch: AWS/ES SearchRate is never published for a domain — 20 completed signed _search requests surface as no SearchRate datapoints

> _Note: this issue was identified against the code base at commit `634fb37e35f51bdb636290ffeec5191f736cf22e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricData

## Description
Cause 1 of 1: an active OpenSearch domain that served data-plane _search requests publishes no AWS/ES SearchRate series, so a caller summing SearchRate over the minutes the searches ran sees nothing.

AWS: recorded behavior only — metric emission is not expressible in the Smithy model (CloudWatch's read side is: GetMetricData with MetricStat {Metric {Namespace 'AWS/ES', MetricName 'SearchRate', Dimensions ClientId+DomainName}, Period 60, Stat 'Sum'}). A domain created with EngineVersion OpenSearch_2.11 on one t3.small.search node with gp3 EBS counts its data-plane search requests into AWS/ES SearchRate, a per-minute count dimensioned by {ClientId, DomainName}. SearchRate is not zero at rest (the service's own traffic reads a few per minute), so the requirement is that M client searches are accounted for: the per-minute Sum values over the window from the minute the searches began to two minutes past the minute they ended total at least M. The recording sees that total well inside a 600 s wait.

Emulator: the earlier legs of the same check now resolve (ClusterStatus.green/yellow/red, Nodes and SearchableDocuments datapoints are served, including the SearchableDocuments delta after a _bulk of 25 documents), but SearchRate never accumulates. 20 signed POST /<index>/_search requests all answered HTTP 200 with the expected 25 hits, and GetMetricData for SearchHRate — Namespace 'AWS/ES', MetricName 'SearchRate', Stat 'Sum', Period 60, dimensions {ClientId, DomainName} — returned no values summing to 20 over the bounded window across 302.9 s of polling at 2 s intervals. Nothing is refused: the call answers 200 with an empty/short series, so the wait cannot end; the run budget then cut it short and the check raised TimeoutError: "the check's time budget ran out after 302.9s, before the 600s wait for SearchRate to count the searches ended; not graded". The check self-labels that point "not graded", so the graded evidence here is the 302.9 s of polling in which 20 completed searches produced no SearchRate at all, while the same check's other AWS/ES series did return datapoints.

Why: a client monitoring query load on an OpenSearch domain — SearchRate dashboards, search-throughput alarms, capacity sizing off search volume — gets an empty series while the domain is demonstrably serving searches, and cannot tell "no queries" from "metric not emitted". A poller waiting for the count hangs instead of getting an answer or a modeled error.

Fix: the OpenSearch (es) domain metric emitter that feeds the CloudWatch metric store — the data-plane request path must count _search requests into an AWS/ES SearchRate series under the {ClientId, DomainName} dimension set at 60 s resolution, alongside the ClusterStatus/Nodes/SearchableDocuments series it already emits. CloudWatch's own GetMetricData read path serves those other series correctly.

Traces: cloudwatch__opensearch.obs-opensearch-1-domain-metrics-track-cluster-health-and-documents.

Related open issue that does NOT carry this check's replay key (it is keyed on the earlier 'the AWS/ES metric catalogue' wait, which this replay now gets past) and therefore does not cover this cause: 20261111T120957Z-cloudwatch-opensearch-the-aws-es-metric-catalogue-never.md. A fixer should merge with it.

## Evidence
Check cloudwatch__opensearch.obs-opensearch-1-domain-metrics-track-cluster-health-and-documents [error]:

  File "check_cloudwatch__opensearch_obs_opensearch_1_domain_metrics_track_cluster_health_and_documents.py", line 468, in run
    raise _not_graded(rate_start, POLL_DEADLINE_S, "SearchRate to count the searches")
  TimeoutError: the check's time budget ran out after 302.9s, before the 600s wait for SearchRate to count the searches ended; not graded

Expected: with 20 searches issued between t_search and t_search_end, GetMetricData(Namespace='AWS/ES', MetricName='SearchRate', Dimensions=[{ClientId},{DomainName}], Period=60, Stat='Sum') over [minute(t_search), minute(t_search_end)+2min] sums to at least 20.
Actual: the windowed Sum stays below 20 for the whole 302.9 s of polling — no SearchRate datapoint is ever attributable to the 20 searches; the call is never refused, the series is simply absent, so the wait runs out. The preceding legs of the same check did receive AWS/ES ClusterStatus.*, Nodes and SearchableDocuments datapoints.

## Steps to Reproduce
1. opensearch CreateDomain with EngineVersion 'OpenSearch_2.11', ClusterConfig {InstanceType 't3.small.search', InstanceCount 1}, EBSOptions {EBSEnabled true, VolumeType 'gp3', VolumeSize 10} and an access policy allowing the caller account root; wait until DescribeDomain reports Processing=false with an Endpoint.
2. SigV4-sign ('es') a POST <endpoint>/_bulk?refresh=true indexing 25 documents into an index, and confirm GET <endpoint>/<index>/_count reads 25.
3. Issue 20 signed POST <endpoint>/<index>/_search requests (term query matching all 25 docs); each answers HTTP 200 with hits.total {value: 25, relation: 'eq'}.
4. cloudwatch GetMetricData with MetricStat {Metric {Namespace 'AWS/ES', MetricName 'SearchRate', Dimensions [{Name 'ClientId', Value <account>}, {Name 'DomainName', Value <domain>}]}, Period 60, Stat 'Sum'}, ScanBy TimestampAscending, over the minute the searches began through two minutes past the minute they ended.
5. Expect the per-minute Sums to total at least 20; observe no datapoints accounting for the searches for 300 s of polling.
