Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__opensearch.xsvc-opensearch-3-cmk-domain-echoes-key-grants-and-serves-search
Issue-Key: kms__opensearch.xsvc-opensearch-3-cmk-domain-echoes-key-grants-and-serves-search|run#42|AssertionError|assert [h.get("_source", {}).get("marker") for h in hits] == [marker], (
Issue-Commit: 34779bb431f14dc6a8db9e00edffcb539add901f

# kms__opensearch: a term query on the implicit `.keyword` sub-field of a dynamically mapped string answers HTTP 200 with zero hits, so an indexed and refreshed document is unfindable on a CMK-encrypted domain (1 cause)

> _Note: this issue was identified against the code base at commit `34779bb431f14dc6a8db9e00edffcb539add901f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
OpenSearch domain REST data plane POST /<index>/_search (es:ESHttpPost)

## Description
One cause.

Cause 1 of 1: a `term` query targeting the implicit `keyword` sub-field of a dynamically mapped string field (`{"query": {"term": {"marker.keyword": "<value>"}}}`, where `marker` was never declared in a mapping and was introduced by the indexed document itself) is accepted with HTTP 200 but `hits.hits` comes back EMPTY, even though the document was indexed moments before (HTTP 201 from PUT /<index>/_doc/1) and the index was explicitly refreshed (HTTP 200 from POST /<index>/_refresh). The whole preceding trajectory of this check passed: the CMK is echoed by CreateDomain and DescribeDomain, OpenSearch's regional service principal holds the Decrypt/GenerateDataKey grant on the key scoped to the domain ARN, the domain becomes searchable, and a signed GET / on the published endpoint answers 200. The only divergence is that the engine cannot find the document it just accepted.
Failing check id: kms__opensearch.xsvc-opensearch-3-cmk-domain-echoes-key-grants-and-serves-search.

AWS: recorded behavior only — the domain's REST data plane is outside the opensearch Smithy model, which covers the control plane (CreateDomain/DescribeDomain and the `Endpoint` they publish) and says nothing about _search. Recorded behavior: when a document introduces a string field no mapping or template covers, dynamic mapping maps it as `text` with an implicit `keyword` sub-field named `keyword`, so `<field>.keyword` holds the exact unanalysed term. After the write is made visible (POST /<index>/_refresh, HTTP 200), POST /<index>/_search with a `term` query on `<field>.keyword` whose value equals that document's field value answers HTTP 200 with exactly the matching documents in `hits.hits` — here one hit, carrying `_source` with the indexed field value, and `hits.total.value` 1.
Emulator: HTTP 200 (the status assertion passed) with `hits.hits` an empty array. The replay reads `AssertionError: _search for marker '<value>' returned [], expected exactly the one indexed doc`. The marker string itself is a per-run generated value and is not part of the requirement; the requirement is that an exact-term query on the dynamic keyword sub-field returns the document that was indexed and refreshed.
Why: a client reads HTTP 200 with zero hits as 'no such document', not as 'this query form is unsupported' — nothing in the response reveals that the sub-field was never queryable. Index-then-read-back is the most basic data-plane contract, and `<field>.keyword` is the standard way to do an exact-value lookup on a string field that was not explicitly mapped (it is what dynamic mapping produces). Every ingest-and-verify pipeline, de-duplication check and exact-id lookup built this way silently reports the document missing, and the failure is indistinguishable from data loss. It also masks the cross-service assertion this check exists to prove — that a CMK-encrypted-at-rest domain still serves reads — by making the data plane look empty rather than refused.
Fix: the domain data plane's search handler, in two parts: the document write path's dynamic-mapping component must register an implicit `keyword` sub-field for a newly seen string field so its exact term is indexed, and the search handler's query-DSL evaluator must resolve a dotted `field` name to that sub-field and match `term` against its indexed terms over the refreshed document set.
Related open issues, neither of which covers this cause (the replay marks this check as neither already-filed nor owned by either): 20261111T041122Z-opensearch-ten-search-engine-divergences-on-the-domain.md — its Cause 9 reports that a dynamically introduced field gets no mapping entry in GET /<index>/_mapping, which is the same dynamic-mapping gap observed through the mapping serializer rather than through a query, and its Cause 2 reports empty `terms` AGGREGATION buckets; 20261111T045944Z-opensearch-a-terms-aggregation-over-the-keyword-sub.md — an empty terms aggregation over the keyword sub-field of an EXPLICIT text multi-field. Both are aggregation/mapping-read symptoms; this cause is a `term` QUERY over an IMPLICIT (dynamic) keyword sub-field returning no hits. If one dotted-sub-field resolver or one dynamic-mapping writer serves all of them, closing those must be verified against this check's id as well.
Traces: kms__opensearch.xsvc-opensearch-3-cmk-domain-echoes-key-grants-and-serves-search.

## Evidence
Expected (recorded AWS): on a domain encrypted at rest with a customer-managed CMK, PUT /<index>/_doc/1 with body {"title": "encrypted at rest", "marker": "<marker>"} answers HTTP 201; POST /<index>/_refresh answers HTTP 200; POST /<index>/_search with body {"query": {"term": {"marker.keyword": "<marker>"}}} answers HTTP 200 with exactly one entry in hits.hits whose _source.marker equals <marker>.

Actual (emulator), verbatim from the replay:
  #### [mismatch] kms__opensearch.xsvc-opensearch-3-cmk-domain-echoes-key-grants-and-serves-search
  File "check_kms__opensearch_xsvc_opensearch_3_cmk_domain_echoes_key_grants_and_serves_search.py", line 223, in run
  assert [h.get("_source", {}).get("marker") for h in hits] == [marker], (
  AssertionError: _search for marker 'cmk-7716ac54d1f3' returned [], expected exactly the one indexed doc

All three HTTP statuses in that sequence were as AWS records them — 201 for the index, 200 for the _refresh, 200 for the _search (each is asserted separately earlier in the check and each passed). The divergence is purely the empty hits.hits array. Controls that passed in the same run: CreateDomain and DescribeDomain echoed EncryptionAtRestOptions.KmsKeyId as the created CMK ARN, a KMS grant to es.<region>.amazonaws.com constrained to EncryptionContextSubset.domainARN = the domain ARN carried Decrypt and GenerateDataKey with the service principal as RetiringPrincipal, the domain reached Processing=false with an Endpoint, and a signed GET / on that endpoint answered HTTP 200. The marker value 'cmk-7716ac54d1f3' and the domain/index names are per-run generated values, not requirements.

## Steps to Reproduce
1. kms CreateKey (symmetric, customer-managed); note its Arn.
2. opensearch CreateDomain with EngineVersion OpenSearch_2.11, ClusterConfig {InstanceType t3.small.search, InstanceCount 1}, EBSOptions {EBSEnabled true, gp3, 10 GiB}, EncryptionAtRestOptions {Enabled true, KmsKeyId <the key ARN>} and an AccessPolicies document allowing es:* to arn:aws:iam::<account>:root on the domain's resources.
3. Poll DescribeDomain until DomainStatus.Processing is false and DomainStatus.Endpoint is present; take that endpoint as the base URL.
4. Send a SigV4 ('es') signed GET / to the endpoint — it answers HTTP 200.
5. PUT <base>/<index>/_doc/1 (SigV4 'es', Content-Type application/json) with body {"title": "encrypted at rest", "marker": "<a unique marker string>"} — answers HTTP 201. No index or mapping is created beforehand, so `marker` is dynamically mapped.
6. POST <base>/<index>/_refresh — answers HTTP 200.
7. POST <base>/<index>/_search with body {"query": {"term": {"marker.keyword": "<the same marker string>"}}} — expect HTTP 200 with exactly one hit whose _source.marker is the marker; observe HTTP 200 with hits.hits == [].
