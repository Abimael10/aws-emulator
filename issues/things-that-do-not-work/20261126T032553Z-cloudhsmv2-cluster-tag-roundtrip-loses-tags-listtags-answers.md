Issue-Source: trace-sweep-divergence
Issue-Trace: cloudhsmv2-cluster-lifecycle-tag-roundtrip-and-modify-rejected
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudhsmv2-cluster-lifecycle-tag-roundtrip-and-modify-rejected
Issue-Shape-Id: com.amazonaws.cloudhsmv2#BaldrApiService
Issue-Key: cloudhsmv2|ListTags|2xx->2xx|$.TagList[Key="environment"]
Issue-Commit: 5b5c0bdcf53ebb03a5a8067f7321f26cc20710e1

# cloudhsmv2 cluster tag roundtrip loses tags: ListTags answers 200 with the applied tag absent from TagList

> _Note: this issue was identified against the code base at commit `5b5c0bdcf53ebb03a5a8067f7321f26cc20710e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTags

## Description
Cause 1 of 1: a tag applied to a CloudHSM cluster is not observable through ListTags — the operation answers HTTP 200 but its required TagList omits the tag entirely.

AWS: Smithy (services/service-com-amazonaws-cloudhsmv2-baldrapiservice/smithy-model/com-amazonaws-cloudhsmv2-baldrapiservice.json, awsJson1_1, sdkId "CloudHSM V2"): TagResource documents "Adds or overwrites one or more tags for the specified CloudHSM cluster" (input ResourceId pattern ^(?:cluster|backup)-[2-7a-zA-Z]{11,16}$ required, TagList @length 1..50 required) and ListTags documents "Gets a list of tags for the specified CloudHSM cluster", with output member TagList (TagList, @length 1..50) **required**. CreateCluster also accepts TagList ("Tags to apply to the CloudHSM cluster during creation"). So on AWS a tag put on a cluster — whether at CreateCluster or by a later TagResource — is part of that cluster's tag set until UntagResource removes it, and the very next ListTags on that cluster id returns it, with the tag's Key and Value exactly as the caller supplied them (both are request values, not service-generated); the status is 200 and no error code is involved. Only a genuinely unknown ResourceId is refused, with CloudHsmResourceNotFoundException (HTTP 400).

Emulator: replaying the cluster tag-roundtrip fixture, the ListTags call at task#4 answered HTTP 200 (the expected status) but the response body's TagList did not contain the tag the fixture had applied to that cluster: `$.TagList[Key="environment"]: missing (expected {'Key': 'environment', 'Value': 'probe'}, got None)`. The remaining tags of that resource matched, so the listing is served and shaped correctly — exactly one applied tag never made it into the cluster's stored tag set. Hypothesis: the tag was applied by the fixture's TagResource call, and services/service-com-amazonaws-cloudhsmv2-baldrapiservice/src/ops/tag_resource.rs is still an unmodified scaffold that returns Err(crate::scaffolded::Scaffolded::not_implemented(..)) (HTTP 501) and writes nothing, so nothing ever adds the tag to ClusterRecord.tags, which is the only place ListTags reads (state.rs resource_tags -> cluster.tags). ListTags' own read path (ordering by key, MaxResults/NextToken paging, CloudHsmResourceNotFoundException for an unknown id) looks correct; the gap is the missing write. The open issue 20261125T083049Z-cloudhsmv2-tagresource-stub.md covers the TagResource stub for the BEHAVIORAL corpus but carries no wire replay key, so this wire step's divergence is not gated by it — that issue is named here deliberately, and fixing it is expected to close this step too.

Why: a client that tags a cluster and then reads its tags back — the normal pattern for cost allocation, ownership and tag-based access policies, and what every tag-driven tool does after a create — is told HTTP 200 and a tag set that silently lacks the tag it just applied. The write looks accepted-or-retryable while the read reports the resource as untagged for that key, so the client either loops re-tagging forever or concludes the tag is absent and takes the wrong branch (re-creating resources, skipping policy checks). Because TagList is a required output member, a consumer has no way to distinguish "not yet applied" from "lost".

Fix: the service's tag write path and the cluster state it feeds — implement services/service-com-amazonaws-cloudhsmv2-baldrapiservice/src/ops/tag_resource.rs so it resolves ResourceId through crate::state (resource lookup as ListTags does), inserts-or-overwrites each supplied Tag by Key into the cluster record's tag collection, and refuses an unknown resource with CloudHsmResourceNotFoundException; UntagResource must remove from the same collection, so that ListTags (already reading that collection) reports the full set. The scaffold marker line in that handler must be deleted as part of implementing it.

Traces: cloudhsmv2-cluster-lifecycle-tag-roundtrip-and-modify-rejected#task#4 (ListTags, expected 200 / actual 200, TagList missing Key="environment").

## Evidence
Fixture cloudhsmv2-cluster-lifecycle-tag-roundtrip-and-modify-rejected, step task#4, operation ListTags, verdict mismatch.
Expected: HTTP 200 with TagList containing {"Key": "environment", "Value": "probe"} (the Key/Value the request supplied) among the cluster's tags.
Actual: HTTP 200, but $.TagList[Key="environment"] missing — got None; the other tags of the resource matched.
Replay divergence_key: service=cloudhsmv2|ListTags|status=200->200|keys=$.TagList[Key="environment"] (affected_count 1).
Code: services/service-com-amazonaws-cloudhsmv2-baldrapiservice/src/ops/tag_resource.rs lines 3-6 still carry the `//! SCAFFOLDED, NOT IMPLEMENTED:` marker and the handler answers HTTP 501, persisting nothing; services/service-com-amazonaws-cloudhsmv2-baldrapiservice/src/ops/list_tags.rs lines 203-243 serve the listing solely from crate::state::resource_tags, and state.rs lines 405-408 resolve that to ClusterRecord.tags, which only CreateCluster (create_cluster.rs line 296) ever populates.
Related open issue (does NOT carry this wire replay key): 20261125T083049Z-cloudhsmv2-tagresource-stub.md.

## Steps to Reproduce
1. Boot the cloudhsmv2 emulator from this checkout.
2. Replay fixture cloudhsmv2-cluster-lifecycle-tag-roundtrip-and-modify-rejected and stop at step task#4.
3. Equivalently, by hand: CreateCluster (HsmType hsm2m.medium, one subnet) and note the returned ClusterId; call TagResource on that ClusterId with TagList [{Key: environment, Value: probe}]; then call ListTags on the same ClusterId.
4. Observe ListTags answer HTTP 200 with a TagList that does not contain the environment/probe tag (AWS returns it).
