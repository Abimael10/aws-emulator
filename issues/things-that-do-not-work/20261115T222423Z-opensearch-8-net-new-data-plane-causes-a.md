Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.custom-analyzer-token-stream-drives-search-match opensearch.osm-refresh-gates-search-while-get-by-id-is-realtime opensearch.osq-deep-paging-past-result-window-rejected opensearch.osq-terms-aggregation-and-sorted-pagination opensearch.fgac-field-level-security-hides-field-from-hit-and-from-query opensearch.osx-deep-opensearch-3-cross-cluster-search-propagation opensearch.query-dsl-malformed-queries-rejected-with-parse-errors opensearch.search-sort-missing-last-multi-value-mode-and-exact-long-order
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch.custom-analyzer-token-stream-drives-search-match|run#6|AssertionError|assert status == 200, f"creating index {index!r} returned HTTP {status!r}: {created!r}"
Issue-Key: opensearch.osm-refresh-gates-search-while-get-by-id-is-realtime|run#11|AssertionError|assert interval == "-1", (
Issue-Key: opensearch.osq-deep-paging-past-result-window-rejected|run#22|AssertionError|assert body["error"]["type"] == "search_phase_execution_exception", (
Issue-Key: opensearch.osq-terms-aggregation-and-sorted-pagination|run#10|KeyError|agg = body["aggregations"]["by_category"]
Issue-Key: opensearch.fgac-field-level-security-hides-field-from-hit-and-from-query|run#41|AssertionError|assert buckets == [], (
Issue-Key: opensearch.osx-deep-opensearch-3-cross-cluster-search-propagation|run#39|AssertionError|assert bogus_status == 404, (
Issue-Key: opensearch.query-dsl-malformed-queries-rejected-with-parse-errors|run#8|AssertionError|assert got == want, f"{label}: error {got!r}, expected {want!r}; body {body!r}"
Issue-Key: opensearch.search-sort-missing-last-multi-value-mode-and-exact-long-order|run#7|AssertionError|assert got == expected, f"{label}: sort {sort!r} returned (id, sort) {got!r}, expected {expected!r}"
Issue-Commit: 0cbfc5d1238fe0c8ea024ec9002d1615d4c3a764

# opensearch: 8 net-new data-plane causes — a built-in token filter is unregistered, flat_settings is ignored, aggregations are dropped, two refusals are misclassified, and field sort loses the missing-value sentinel and long precision

> _Note: this issue was identified against the code base at commit `0cbfc5d1238fe0c8ea024ec9002d1615d4c3a764`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Search

## Description
Eight net-new root causes on the OpenSearch domain REST data plane, all reached over the domain's published endpoint with SigV4 (signing name `es`). Related open issues 20261111T041122Z-opensearch-ten-search-engine-divergences-on-the-domain.md, 20261111T041122Z-opensearch-ten-domain-rest-data-plane-endpoints-are.md, 20261111T045944Z-opensearch-a-terms-aggregation-over-the-keyword-sub.md and 20261114T222138Z-opensearch-4-net-new-causes-createindex-accepts-a.md describe neighbouring data-plane gaps but none of them carries any of the check ids below, so none covers these causes.

Cause 1 of 8: the built-in `asciifolding` token filter is not registered, so CreateIndex is refused for any custom analyzer that names it.
AWS: recorded behavior only (the index REST API is not in the Smithy model; the control plane only exposes the domain). OpenSearch ships `asciifolding` as a built-in token filter, so `PUT /<index>` with settings.analysis.analyzer.<name> = {type: custom, tokenizer: standard, filter: [lowercase, asciifolding, <custom stop filter>]} answers HTTP 200 {"acknowledged": true, ...} and the analyzer is then usable from `_analyze` and from a `text` field's `analyzer`.
Emulator: HTTP 400 {"error": {"root_cause": [{"type": "illegal_argument_exception", "reason": "failed to find token filter under [asciifolding]"}], "type": "illegal_argument_exception", "reason": "failed to find token filter under [asciifolding]"}, "status": 400}. The fixed text of that message is OpenSearch's wording for a genuinely unknown filter; the variable part is the filter name taken from the request — but `asciifolding` is a built-in, so the refusal itself is the defect.
Why: no index that folds accents can be created, so accent-insensitive search — the standard way to serve `cafe` matching `café` — cannot be built at all, and the failure arrives at index-create time rather than as a search difference.
Fix: the index-settings analysis registry of the domain data plane in the opensearch service crate — register the built-in token filters (asciifolding at minimum) alongside the already-accepted `lowercase` and `stop`.
Traces: opensearch.custom-analyzer-token-stream-drives-search-match.

Cause 2 of 8: `GET /<index>/_settings?flat_settings=true` ignores the flag and answers the nested settings tree.
AWS: recorded behavior only. With `flat_settings=true` the response keys the settings map by DOTTED paths: {"<index>": {"settings": {"index.refresh_interval": "-1", "index.number_of_shards": "1", "index.number_of_replicas": "0", "index.provided_name": "<index>", "index.uuid": "<uuid>"}}}. Without the flag the same values come back nested under settings.index.
Emulator: returns the nested form regardless — {"<index>": {"settings": {"index": {"refresh_interval": "-1", "number_of_shards": "1", ...}}}} — so a client reading `settings["index.refresh_interval"]` gets nothing (the check observed None for a setting the index really does hold as "-1").
Why: every client and tool that asks for flat settings (the documented way to read one setting without walking the tree) sees every setting as absent, and code that then writes a default back silently changes index configuration.
Fix: the `_settings` read path of the domain data plane — honour the `flat_settings` query parameter when serializing index settings.
Traces: opensearch.osm-refresh-gates-search-while-get-by-id-is-realtime.

Cause 3 of 8: a search past `index.max_result_window` is refused with the bare `illegal_argument_exception` instead of the `search_phase_execution_exception` wrapper.
AWS: recorded behavior only. HTTP 400 with body {"error": {"type": "search_phase_execution_exception", "root_cause": [{"type": "illegal_argument_exception", "reason": "Result window is too large, from + size must be less than or equal to: [<index.max_result_window>] but was [<from+size>]. ..."}], ...}, "status": 400} and no `hits` section. The fixed text is "Result window is too large, from + size must be less than or equal to:"; the bracketed numbers are the index's configured window and the request's from+size. from+size exactly equal to the window is still accepted with 200.
Emulator: HTTP 400 with error.type "illegal_argument_exception" at the top level (reason text correct) and root_cause carrying the same illegal_argument_exception — the search-phase wrapper is missing.
Why: clients and SDK retry/classification logic branch on `error.type`; a deep-paging guard that reports itself as a plain argument error is indistinguishable from a malformed-request error, so pagination code that should switch to search_after/PIT does not.
Fix: the search error serializer of the domain data plane — raise shard-execution failures through the search_phase_execution_exception envelope with the underlying cause in root_cause.
Traces: opensearch.osq-deep-paging-past-result-window-rejected.

Cause 4 of 8: a `_search` whose terms aggregation carries sub-aggregations returns no `aggregations` section at all.
AWS: recorded behavior only. POST /<index>/_search with {"size": 0, "aggs": {"<name>": {"terms": {"field": "<keyword field>", "size": N}, "aggs": {"<avg name>": {"avg": {"field": "<numeric>"}}, "<sum name>": {"sum": {"field": "<numeric>"}}}}}} answers 200 with hits.hits == [] and an `aggregations` object holding <name>.buckets (each bucket with key, doc_count and the sub-metric objects), doc_count_error_upper_bound and sum_other_doc_count.
Emulator: the response body has no `aggregations` key (the check raised KeyError: 'aggregations' on the first such request), so bucket counts, sub-metric values and the sum_other_doc_count accounting are all unobservable.
Why: facet/dashboard/report workloads — the primary reason to run size:0 searches — come back empty with HTTP 200, so a client reports "no data" rather than an error.
Fix: aggregation execution and response serialization in the domain data plane — compute terms buckets with nested sub-aggregations and always emit the `aggregations` section when `aggs` is present.
Traces: opensearch.osq-terms-aggregation-and-sorted-pagination.

Cause 5 of 8: a terms aggregation over a field hidden by field-level security answers 200 with the buckets array ABSENT instead of empty.
AWS: recorded behavior only (FGAC field-level security, OpenSearch security plugin). A user whose role excludes field F runs {"size": 0, "aggs": {"<name>": {"terms": {"field": "F"}}}} and gets HTTP 200 with aggregations.<name>.buckets == [] — the field is readable by no path, so the aggregation yields zero buckets rather than erroring or being omitted.
Emulator: HTTP 200, but aggregations.<name>.buckets resolves to nothing (observed None), i.e. the aggregation result for the excluded field is not serialized at all.
Why: a client cannot distinguish "this field is masked for me" (empty buckets) from "this response has no aggregations", and FLS-aware UIs that render an empty facet instead break on a missing key.
Fix: the FLS filter applied to aggregation results in the domain data plane — an excluded field must produce an empty bucket list in the normal aggregations envelope.
Traces: opensearch.fgac-field-level-security-hides-field-from-hit-and-from-query.

Cause 6 of 8: a cross-cluster search on an UNKNOWN remote alias is refused 400 no_such_remote_cluster_exception instead of 404 index_not_found_exception.
AWS: recorded behavior only. On a managed domain with an ACTIVE outbound connection, `POST /<alias>:<index>/_search` for an alias that was never connected is refused with HTTP 404 and error.type "index_not_found_exception"; the same refusal is what the query gets after DeleteOutboundConnection, while the live alias keeps serving the remote documents.
Emulator: HTTP 400 {"error": {"root_cause": [{"type": "no_such_remote_cluster_exception", "reason": "no such remote cluster: [<alias>]"}], "type": "no_such_remote_cluster_exception", "reason": "no such remote cluster: [<alias>]"}, "status": 400} — the alias name is the variable part, the type and status are the divergence.
Why: clients treat 404/index_not_found_exception as "target missing, stop retrying" and 400 as a malformed request; a connection-revoked or mistyped alias is therefore misclassified, and code that catches index_not_found_exception to fall back to a local index never fires.
Fix: cross-cluster target resolution in the domain data plane search path — an unresolvable `<alias>:<index>` target must be refused as a missing index (404 index_not_found_exception).
Traces: opensearch.osx-deep-opensearch-3-cross-cluster-search-propagation.

Cause 7 of 8: the parsing_exception for an unknown query type uses the wrong reason wording and omits its caused_by.
AWS: recorded behavior only. {"query": {"<unknown>": {...}}} is refused HTTP 400 with error.type "parsing_exception", reason "unknown query [<unknown>]" and caused_by {"type": "named_object_not_found_exception", "reason": "[<line>:<col>] unknown field [<unknown>]"}. Fixed text: "unknown query [" ... "]" and "unknown field [" ... "]"; the variable parts are the query name from the request and the line:col offset of that name in the request body as sent.
Emulator: error.type is correct but reason is "no [query] registered for [<unknown>]" and there is no caused_by; root_cause repeats the same wrong reason.
Why: the message is the contract for a 400 on a malformed query — clients log it, match on it and surface it to users; a different wording means parse-error handling and golden-output tests written against AWS fail against the emulator.
Fix: query-DSL parse error construction in the domain data plane — emit AWS's reason wording and attach the named_object_not_found_exception cause with the body position.
Traces: opensearch.query-dsl-malformed-queries-rejected-with-parse-errors.

Cause 8 of 8: field sort drops the missing-value sentinel and compares longs as float64, so sort keys and order are both wrong.
AWS: recorded behavior only. For `sort: [{"<long field>": {"order": "asc"}}]` a document MISSING the field sorts last and its echoed sort value is the long sentinel 9223372036854775807 (Long.MAX_VALUE); descending it sorts last with -9223372036854775808, and `"missing": "_first"` moves it to the front with the opposite sentinel. Long values are compared exactly, so values straddling 2**53 order 9007199254740992 < 9007199254740993 < 9007199254740995.
Emulator: the missing document's sort value is serialized as null (observed [None]) rather than the sentinel, and the exact-long ordering is wrong — the returned order was (d5 -9007199254740993), (d1 9007199254740993), (d2 9007199254740992), (d4 9007199254740995), (d3 None) where AWS returns (d5 -9007199254740993), (d2 9007199254740992), (d1 9007199254740993), (d4 9007199254740995), (d3 9223372036854775807): the two values either side of 2**53 are transposed, which is the signature of a float64 comparison.
Why: search_after/live pagination feeds the echoed sort values back as the cursor — a null cursor value cannot be resumed from, and a sort key that ties or transposes large longs makes a paged walk skip or repeat documents. Any id- or timestamp-in-nanoseconds sort over 2**53 is silently mis-ordered.
Fix: sort key computation and serialization in the domain data plane — compare numeric sort fields at long precision and substitute the direction's long sentinel for a missing field (honouring the `missing` option).
Traces: opensearch.search-sort-missing-last-multi-value-mode-and-exact-long-order.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1), `### opensearch` section (205/269 passed, 64 failed; the 8 causes below are the failures not marked OWNED).

[mismatch] opensearch.custom-analyzer-token-stream-drives-search-match
  AssertionError: creating index 'sim2real-behav-analyzer' returned HTTP 400: {'error': {'root_cause': [{'type': 'illegal_argument_exception', 'reason': 'failed to find token filter under [asciifolding]'}], 'type': 'illegal_argument_exception', 'reason': 'failed to find token filter under [asciifolding]'}, 'status': 400}
  Expected: HTTP 200 for a custom analyzer {tokenizer: standard, filter: [lowercase, asciifolding, <stop>]}.

[mismatch] opensearch.osm-refresh-gates-search-while-get-by-id-is-realtime
  AssertionError: index 'sim2real-osm-refresh' reports index.refresh_interval None, expected '-1': {'sim2real-osm-refresh': {'settings': {'index': {'refresh_interval': '-1', 'number_of_shards': '1', 'number_of_replicas': '0', ...}}}}
  The request was GET /<index>/_settings?flat_settings=true; the value is right, the flattening is missing.

[mismatch] opensearch.osq-deep-paging-past-result-window-rejected
  AssertionError: expected search_phase_execution_exception, got 'illegal_argument_exception': {'error': {'root_cause': [{'type': 'illegal_argument_exception', 'reason': 'Result window is too large, from + size must be less than or equal to: [10000] but was [10001].'}], 'type': 'illegal_argument_exception', ...}, 'status': 400}

[error] opensearch.osq-terms-aggregation-and-sorted-pagination
  KeyError: 'aggregations'  (body["aggregations"]["by_category"] on a size:0 terms agg with avg/sum sub-aggs)

[mismatch] opensearch.fgac-field-level-security-hides-field-from-hit-and-from-query
  AssertionError: the restricted user's terms aggregation over 'salary' returned buckets None, expected [] -- an aggregation is a read of the field by another name

[mismatch] opensearch.osx-deep-opensearch-3-cross-cluster-search-propagation
  AssertionError: a cross-cluster query on an unknown alias must be refused with 404, got HTTP 400: {'error': {... 'type': 'no_such_remote_cluster_exception', 'reason': 'no such remote cluster: [sim2realnope...]'}, 'status': 400}
  Expected 404 with error.type index_not_found_exception. The positive cross-cluster leg succeeded, so only the refusal diverges.

[mismatch] opensearch.query-dsl-malformed-queries-rejected-with-parse-errors
  AssertionError: unknown query type: error {'type': 'parsing_exception', 'reason': 'no [query] registered for [no_such_query]'}, expected {'type': 'parsing_exception', 'reason': 'unknown query [no_such_query]', 'caused_by': {'type': 'named_object_not_found_exception', 'reason': '[1:29] unknown field [no_such_query]'}}

[mismatch] opensearch.search-sort-missing-last-multi-value-mode-and-exact-long-order
  AssertionError: big asc, missing last by default: returned (id, sort) [('d5', [-9007199254740993]), ('d1', [9007199254740993]), ('d2', [9007199254740992]), ('d4', [9007199254740995]), ('d3', [None])], expected [('d5', [-9007199254740993]), ('d2', [9007199254740992]), ('d1', [9007199254740993]), ('d4', [9007199254740995]), ('d3', [9223372036854775807])]

## Steps to Reproduce
1. git_clone the repo; build and boot the emulator from the checkout.
2. CreateDomain with the minimum single-node topology (t3.small.search x1, no dedicated master, no zone awareness, gp3 10 GiB) and wait until it is ACTIVE with a published Endpoint; address the data plane over that endpoint with SigV4 (signing name `es`).
3. Cause 1: PUT /<index> with settings.analysis = {filter: {<stop>: {type: stop, stopwords: [the]}}, analyzer: {folded: {type: custom, tokenizer: standard, filter: [lowercase, asciifolding, <stop>]}}} — AWS answers 200, the emulator answers 400 "failed to find token filter under [asciifolding]".
4. Cause 2: create an index with index.refresh_interval "-1", then GET /<index>/_settings?flat_settings=true — AWS keys the map "index.refresh_interval", the emulator returns the nested tree.
5. Cause 3: index a dozen documents, then POST /<index>/_search {"size": 10, "from": 9991, "sort": [{"rank": "asc"}]} — both refuse with 400, but check error.type: AWS search_phase_execution_exception, emulator illegal_argument_exception.
6. Cause 4: POST /<index>/_search {"size": 0, "aggs": {"by_category": {"terms": {"field": "category", "size": 10}, "aggs": {"avg_price": {"avg": {"field": "price"}}, "sum_price": {"sum": {"field": "price"}}}}}} — the emulator's body has no "aggregations" key.
7. Cause 5: on an FGAC domain, give a user a role whose field_security excludes a field, then run a terms aggregation over that field as that user — AWS 200 with buckets [], emulator 200 with no buckets.
8. Cause 6: with an ACTIVE outbound connection, POST /<unknown-alias>:<index>/_search — AWS 404 index_not_found_exception, emulator 400 no_such_remote_cluster_exception.
9. Cause 7: POST /<index>/_search {"query": {"no_such_query": {"name": "alpha"}}} — compare error.reason and caused_by.
10. Cause 8: index documents where one lacks the sort field and two long values straddle 2**53, then sort ascending and read both the order and the echoed sort values.
11. Replay the eight check ids in trace_ids.
