Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.osq-terms-aggregation-and-sorted-pagination opensearch.domain-maintenance-lifecycle
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.osq-terms-aggregation-and-sorted-pagination|run#24|KeyError|assert hit["fields"]["rank_doubled"] == [rank * 2], (
Issue-Key: opensearch.domain-maintenance-lifecycle|_wait_maintenance_terminal#10|Timeout|time.sleep(interval_s)
Issue-Commit: 6c50300ce49aa91ae59100e19f70ea48f974519f

# opensearch: 2 net-new causes — the domain data plane ignores `script_fields` (hits carry no `fields` object), and the wall-clock domain/maintenance transient windows cost a polling client a full interval so a maintenance action is never observed terminal

> _Note: this issue was identified against the code base at commit `6c50300ce49aa91ae59100e19f70ea48f974519f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Search (domain data plane POST /{index}/_search); GetDomainMaintenanceStatus

## Description
Two net-new root causes on the opensearch emulator, both grounded in failing behavioral checks that the replay report did not mark ALREADY FILED or OWNED.

Cause 1 of 2: a `_search` body carrying `script_fields` is served without the computed fields, so every hit comes back with no `fields` member.

AWS: the domain data plane is the OpenSearch REST API and is NOT part of this service's Smithy model (the model covers the control plane only — there is no `Search` operation to cite), so this is recorded/documented REST behavior only. OpenSearch's search API answers a request whose body contains `script_fields` by adding, to EACH element of `hits.hits`, a sibling object `fields` keyed by each script-field name, whose value is an ARRAY of the values the script produced for that hit — e.g. a `painless` script `doc['<numeric-field>'].value * 2` on a hit whose field value is N yields `"fields": {"<script-field-name>": [2N]}`. `script_fields` is independent of `_source` filtering: a request that both restricts `_source` to a field list and declares `script_fields` returns the restricted `_source` AND the `fields` object on the same hit. It is also independent of `sort`/`from`/`size`: every page of a sorted walk carries the script fields for the hits on that page. The recording succeeded (HTTP 200), so this is not a refusal — the status and the hit envelope are otherwise right.

Emulator: the search response is served 200 with correct `hits.total`, correct `_source` projection and correct sort order, but each hit object has NO `fields` key at all — the `script_fields` block of the request body is dropped. The replay fails with `KeyError: 'fields'` at `hit["fields"]["rank_doubled"]`, i.e. the key is absent rather than holding a wrong value. The same check's terms-aggregation assertions (exact `doc_count`, `avg`/`sum` sub-metrics, `sum_other_doc_count` truncation accounting, `doc_count_error_upper_bound`) all passed first, so the divergence is isolated to script-field evaluation and serialization, not to aggregation or paging.

Why: a client that asks for a derived value per hit — the normal way to return a computed column without reindexing, and the normal way an OpenSearch Dashboards/scripted-field consumer reads one — gets a hit object with no `fields` member. Idiomatic client code indexes `hit["fields"][name]` directly and raises `KeyError`/`NullPointerException` instead of receiving the value, so the request appears to succeed while the data the caller asked for is silently missing. Nothing in the response signals that `script_fields` was ignored.

Fix: the domain data plane's search path in the opensearch service crate — the `_search` request-body parser (which must accept and retain `script_fields`), the script evaluator that computes each named script per matching hit, and the hit serializer that must emit the `fields` object (array-valued per name) beside `_source` on every hit of every page.

Traces: opensearch.osq-terms-aggregation-and-sorted-pagination.

Note: open issue 20261111T041122Z-opensearch-ten-search-engine-divergences-on-the-domain.md and 20261119T031228Z-opensearch-3-net-new-domain-data-plane-causes.md describe other search-engine gaps (scoring, scroll ids, nested `aggs`); neither names `script_fields` and neither carries this check's replay key, so this cause is net-new.

Cause 2 of 2: the transient windows the control plane paces in WALL-CLOCK seconds (domain provisioning, and each maintenance status step) cost a polling client one full poll interval per state change, so a `RESTART_SEARCH_PROCESS` maintenance action is never observed reaching its terminal status.

AWS: `StartDomainMaintenance` accepts an action on a listed data node and returns a `MaintenanceId`; `GetDomainMaintenanceStatus` then walks that action `PENDING` -> `IN_PROGRESS` -> a terminal status, and real AWS reports a finished action with `Status` = `SUCCESS` (a value outside the modelled `MaintenanceStatus` enum, which lists COMPLETED | FAILED | IN_PROGRESS | PENDING | TIMED_OUT — Smithy: GetDomainMaintenanceStatus output member `Status` targets `MaintenanceStatus`). The listing `ListDomainMaintenances` must then report the same action with the same terminal status. On AWS the walk takes minutes and a client polls it on a minute-scale interval; the contract a client depends on is that each state is OBSERVABLE by a poller, i.e. the service does not make a client burn a whole poll interval per transition.

Emulator: every step is a fixed wall-clock span, so a client that polls on its own (coarser) interval loses a full interval to each transition. In the replayed check the first `DescribeDomainNodes` after `CreateDomain` is refused because the domain still has work in flight, costing one whole poll interval before the node census appears; the maintenance record is then written `PENDING` and only becomes terminal two further wall-clock spans later, so the poller's next read lands past the check's budget and the action is never read as `SUCCESS`. The check ends with `Check timed out after 60.0s` — the terminal status assertion (`expected the maintenance action to SUCCEED`) is never reached, and `ListDomainMaintenances` is never re-read after completion.

Why: a client that starts a node maintenance action and polls for its outcome never sees the outcome within the time real AWS would have given it; the action appears to hang in a non-terminal status. Any workflow gated on "maintenance finished" (a restart-then-verify loop, a rolling node restart) blocks indefinitely even though the emulator's own record does eventually settle.

Fix: how the control plane paces transient state in the domain/maintenance state module — the provisioning window a newly created domain holds before it publishes its node census, and the per-step maintenance windows `StartDomainMaintenance`/`GetDomainMaintenanceStatus` settle against — so that a state a client must observe is observable on the poll it next issues rather than one interval later.

Traces: opensearch.domain-maintenance-lifecycle.

Note: open issue 20261121T121321Z-opensearch-wall-clock-transient-windows-cost-a-client.md describes the same FAMILY of wall-clock window cost for six other chained-wait checks; it does not carry this check's replay key and does not cover the maintenance-status walk, so this cause is filed separately and named here for the fixer to land together.

## Evidence
Cause 1 — opensearch.osq-terms-aggregation-and-sorted-pagination (verdict: error)
  Request: POST /<index>/_search with body
    {"size": 5, "from": <offset>, "sort": [{"rank": {"order": "asc"}}],
     "_source": ["category", "rank"],
     "script_fields": {"rank_doubled": {"script": {"lang": "painless",
                                                   "source": "doc['rank'].value * 2"}}}}
  Expected (AWS / OpenSearch REST): HTTP 200 and, for each element of hits.hits,
    {"_source": {"category": ..., "rank": N}, "fields": {"rank_doubled": [2N]}}
    — fixed text of the template: the member name "fields" and the script-field name the
      REQUEST declared ("rank_doubled" here); the variable part is the per-hit computed
      value, an ARRAY, derived from that hit's indexed field value. The literal N/2N of any
      one run are not part of the requirement.
  Actual (emulator): HTTP 200, hits.total correct (12, relation "eq"), _source projection and
    ascending rank ordering correct, but the hit object contains NO "fields" member, so the
    client raises:
      File "check_opensearch_osq_terms_aggregation_and_sorted_pagination.py", line 120, in run
        assert hit["fields"]["rank_doubled"] == [rank * 2]
      KeyError: 'fields'
  Preceding assertions in the same check PASSED (terms buckets with exact doc_count/avg/sum,
  size:2 truncation with sum_other_doc_count == 3, filtered aggregation counts), which isolates
  the defect to script-field evaluation/serialization.

Cause 2 — opensearch.domain-maintenance-lifecycle (verdict: mismatch)
  Sequence: CreateDomain (3 data nodes, t3.small.search) -> poll DescribeDomainNodes ->
    StartDomainMaintenance(Action=RESTART_SEARCH_PROCESS, NodeId=<a listed data node>) ->
    poll GetDomainMaintenanceStatus for a terminal Status -> ListDomainMaintenances.
  Expected (AWS): GetDomainMaintenanceStatus reports Action=RESTART_SEARCH_PROCESS and the
    NodeId the request named, the action walks PENDING -> IN_PROGRESS -> Status "SUCCESS"
    (fixed text; AWS sends SUCCESS, outside the modelled MaintenanceStatus enum), and
    ListDomainMaintenances lists the same MaintenanceId with that status both before and
    after completion.
  Actual (emulator): the first DescribeDomainNodes after CreateDomain is refused while the
    domain has work in flight, costing the poller a whole interval; the maintenance record is
    then returned Status="PENDING" and only settles to its terminal status two further
    wall-clock spans later, so the terminal read never lands and the check ends:
      Error: Check timed out after 60.0s
    The assertion `expected the maintenance action to SUCCEED` and the post-completion
    ListDomainMaintenances read are never reached.

## Steps to Reproduce
Cause 1:
1. Create an OpenSearch domain and wait for it to publish an endpoint.
2. Create an index with mappings {category: keyword, price: integer, rank: integer} and bulk-index a dozen documents with distinct `rank` values.
3. POST /<index>/_search with:
   {"size": 5, "from": 0, "sort": [{"rank": {"order": "asc"}}], "_source": ["category","rank"],
    "script_fields": {"rank_doubled": {"script": {"lang":"painless","source":"doc['rank'].value * 2"}}}}
4. Inspect hits.hits[0]. AWS returns a "fields" object {"rank_doubled": [<2 * rank>]} beside the
   filtered "_source"; the emulator returns the hit with no "fields" key.
5. Replay: opensearch.osq-terms-aggregation-and-sorted-pagination.

Cause 2:
1. CreateDomain with ClusterConfig {InstanceType: t3.small.search, InstanceCount: 3} (three data
   nodes, so RESTART_SEARCH_PROCESS is not refused as a disabled operation).
2. Poll DescribeDomainNodes on a coarse interval (e.g. 30 s) until a NodeId is listed; note the
   first call is refused because the domain still has work in flight, so a whole interval is lost.
3. Call StartDomainMaintenance(DomainName, Action="RESTART_SEARCH_PROCESS", NodeId=<listed node>)
   and keep the MaintenanceId.
4. Poll GetDomainMaintenanceStatus on the same interval for a terminal Status. The first read is
   "PENDING"; the action only settles to its terminal status after two further wall-clock spans, so
   a poller on a realistic interval never reads SUCCESS before its budget expires.
5. Replay: opensearch.domain-maintenance-lifecycle.
