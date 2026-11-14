Issue-Source: trace-sweep-divergence
Issue-Trace: s3tables-s3t-get-table-with-both-the-arn-and-the-triple-rejected
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3tables-s3t-get-table-with-both-the-arn-and-the-triple-rejected s3tables-s3t-tag-resource-round-trips-and-untag-removes-only-the-named-key s3tables-table-rename-and-tagging s3tables-table-metadata-location-update
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables|GetTable|4xx->2xx|$.message
Issue-Key: s3tables|GetTable|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: s3tables|GetTable|4xx->2xx|status
Issue-Key: s3tables|ListTagsForResource|2xx->2xx|$.tags
Issue-Key: s3tables|ListTagsForResource|2xx->2xx|$.tags.stage
Issue-Key: s3tables|UpdateTableMetadataLocation|2xx->2xx|$.versionToken
Issue-Commit: 0c97311e0491e6ff934cad5e77765048b7e1abe2

# s3tables: 3 net-new causes — GetTable accepts both identifier forms, UntagResource never removes a key, UpdateTableMetadataLocation rotates versionToken

> _Note: this issue was identified against the code base at commit `0c97311e0491e6ff934cad5e77765048b7e1abe2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTable

## Description
THREE net-new causes, all in s3tables, all independent of the divergences already owned by open issues. Every cause below must be closed before this issue is submitted.

Cause 1 of 3: GetTable does not refuse a request that carries BOTH addressing modes (fixture s3tables-s3t-get-table-with-both-the-arn-and-the-triple-rejected, step task#4).
AWS: GetTable models four optional @httpQuery members — tableArn, and the triple tableBucketARN + namespace + name (Smithy: GetTableRequest.tableArn / .tableBucketARN / .namespace / .name, all optional; errors include BadRequestException @httpError 400). The two modes are mutually exclusive: a request that supplies tableArn AND the full triple is refused with HTTP 400 BadRequestException, the x-amzn-errortype header set to the error shape, and the message "Invalid input: Must provide either table arn alone OR all of (table bucket arn, namespace, table name) together". That message is entirely FIXED text for this error class — it names no value from the request, so nothing in it is substituted.
Emulator: answers HTTP 200 with the table body. It silently prefers tableArn and ignores the triple, emits no $.message and no x-amzn-errortype header, so an over-specified (and therefore possibly self-contradictory) request succeeds.
Why: a client that builds a request from two sources, or that sends a stale triple beside a fresh ARN, gets a successful answer about whichever resource the ARN names instead of AWS's refusal. Inconsistent identifiers are never detected, so a caller addressing the wrong namespace/name silently reads a different table than it asked for, and request-validation tests that assert the 400 pass against AWS and fail against the emulator.
Fix: GetTable's handler input validation — the match on the four addressing members must reject the case where tableArn is present together with any of tableBucketARN/namespace/name, with the BadRequestException above, before it resolves anything. Today the first arm matches tableArn and discards the rest. Traces/code: services/service-com-amazonaws-s3tables-s3tablebuckets/src/ops/get_table.rs:208-230.
Note: the SAME fixture's step task#5 (neither mode complete) diverges only in the refusal TEXT and is already owned by 20261103T033559Z-s3tables-request-validation-diverges-generic-validator-text-replaces.md; that issue does not cover the over-specified request, which is accepted outright here.
Traces: s3tables-s3t-get-table-with-both-the-arn-and-the-triple-rejected#task#4.

Cause 2 of 3: UntagResource removes nothing, so a later ListTagsForResource still reports the removed key (fixture s3tables-s3t-tag-resource-round-trips-and-untag-removes-only-the-named-key, steps task#8 and task#10; and fixture s3tables-table-rename-and-tagging, step task#10). One root cause, observed in two fixtures — both are listed in Issue-Trace-Ids and both must come back clean.
AWS: Smithy UntagResource — DELETE /tag/{resourceArn} with required @httpQuery tagKeys (TagKeyList), success 204 and an empty body; "Removes the specified user-defined tags from an Amazon S3 Tables resource. You can pass one or more tag keys." After it succeeds, ListTagsForResource (GET /tag/{resourceArn}, 200, $.tags map) returns the resource's tags MINUS exactly the keys named — keys not named are untouched, and a re-read is the authoritative view of the tag set.
Emulator: UntagResource is still an unimplemented scaffold that answers HTTP 501 NotImplemented and mutates no state, so the keys stay resident and the following ListTagsForResource answers 200 with the removed key still present: expected $.tags {"sim2real": "yes"}, got {"purpose": "rewritten", "sim2real": "yes"}; and in the rename fixture $.tags.stage is extra (expected absent, got "probe").
Why: tags are the ABAC/cost-allocation surface of this service. A client that removes a tag and then reads the tag set back sees the tag still there, so a de-tagging workflow (revoking attribute-based access, dropping a cost-allocation key) appears to leave the grant/key in place; any flow that asserts a key is gone after removal cannot complete against the emulator.
Fix: implement the UntagResource handler (services/service-com-amazonaws-s3tables-s3tablebuckets/src/ops/untag_resource.rs, currently returning crate::scaffolded::Scaffolded::not_implemented) against the existing tag store — resolve the ARN with crate::state::resource_tags_mut (src/state/tagging.rs:283-298, which already distinguishes NotTaggable -> 400 from NoSuchTableBucket/NoSuchTable -> 404) and remove exactly the named keys, answering 204 with an empty body. What is filed HERE is the observable read divergence on ListTagsForResource; the 501 itself belongs to the operation's own stub issue.
Traces: s3tables-s3t-tag-resource-round-trips-and-untag-removes-only-the-named-key#task#8 (cluster also covers #task#10) and s3tables-table-rename-and-tagging#task#10.

Cause 3 of 3: UpdateTableMetadataLocation answers with a freshly minted versionToken instead of the table's current version token (fixture s3tables-table-metadata-location-update, step task#6).
AWS: Smithy UpdateTableMetadataLocation — PUT /tables/{tableBucketARN}/{namespace}/{name}/metadata-location, required input versionToken and metadataLocation, output versionToken required (VersionToken, @length 1..2048). Recorded behavior only for the token's lifecycle: the recording asserts the response's $.versionToken against the token the fixture had already captured for that table, i.e. the value AWS returns is the table's version token as the caller knows it, not a value unrelated to any token the caller holds.
Emulator: answers 200 with a brand-new random UUID in $.versionToken (got "91dd8003-ae07-4641-8db1-64bbb465304f" where the captured token was expected) — the token is re-minted on the write rather than carried from the table's recorded version.
Why: versionToken is this service's optimistic-concurrency precondition: callers pass it back on DeleteTable, RenameTable and UpdateTableMetadataLocation. If every write answers with an unrelated freshly generated token, a client cannot correlate the token it holds with the table's version, so it cannot tell a concurrent modification from its own write, and precondition handling built on AWS's semantics breaks.
Fix: the version-token handling in UpdateTableMetadataLocation's handler and the table record's version_token field in the service state — stop calling the fresh-token minter on this path (state.rs:1146-1149, mint_version_token -> mint_id) and answer with the token the table's version actually carries. Related but NOT covering this: open issue 20261109T141301Z-s3tables-renametable-rotates-the-table-s-versiontoken-so.md is the same rotation family on RenameTable, and 20261101T230800Z-s3tables-read-responses-diverge-timestamps-emitted-as-epoch.md covers only the token's FORM on CreateTable/GetTable.
Traces: s3tables-table-metadata-location-update#task#6.

Per-run values (UUIDs, instants, generated bucket/table names) quoted above are examples; the requirement is the behavior — the 400 refusal and its fixed message, the tag set after a removal, and a version token that is the table's rather than a fresh one.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (score 89.2%, 3 net-new clusters out of 17 divergences; the other 14 are owned by open issues or by stub issues):

- GetTable, s3tables-s3t-get-table-with-both-the-arn-and-the-triple-rejected#task#4: status expected 400, got 200; $.message missing (expected 'Invalid input: Must provide either table arn alone OR all of (table bucket arn, namespace, table name) together', got None); $headers.x-amzn-errortype missing (expected the error-shape value, got None). Request carried tableArn AND tableBucketARN+namespace+name.
- ListTagsForResource, s3tables-s3t-tag-resource-round-trips-and-untag-removes-only-the-named-key#task#8 (cluster also #task#10): status 200 both; $.tags expected {'sim2real': 'yes'}, got {'purpose': 'rewritten', 'sim2real': 'yes'} — the key removed by the preceding UntagResource is still present.
- ListTagsForResource, s3tables-table-rename-and-tagging#task#10: status 200 both; $.tags.stage extra (expected None, got 'probe') — again the removed key survives.
- Source of causes 2's mechanism: services/service-com-amazonaws-s3tables-s3tablebuckets/src/ops/untag_resource.rs:1-12,169 is still marked 'SCAFFOLDED, NOT IMPLEMENTED' and returns Scaffolded::not_implemented("UntagResource") (HTTP 501), so no tag is ever deleted.
- UpdateTableMetadataLocation, s3tables-table-metadata-location-update#task#6: status 200 both; $.versionToken expected the table's previously captured token, got '91dd8003-ae07-4641-8db1-64bbb465304f' (a fresh UUID minted on the write).
- Code for cause 1: get_table.rs:208-230 matches `(Some(table_arn), ..)` first and therefore ignores tableBucketARN/namespace/name whenever tableArn is present; the only 400 it can emit is the wrong-text 'Either tableArn, or tableBucketARN, namespace and name together, must be specified.' on the no-complete-mode arm.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.
2. Cause 1: fixture s3tables-s3t-get-table-with-both-the-arn-and-the-triple-rejected, step task#4 — GET /get-table with tableArn AND tableBucketARN+namespace+name set. Observe 200 with a table body instead of 400 BadRequestException carrying x-amzn-errortype and the message 'Invalid input: Must provide either table arn alone OR all of (table bucket arn, namespace, table name) together'.
3. Cause 2: fixture s3tables-s3t-tag-resource-round-trips-and-untag-removes-only-the-named-key — tag a table bucket with two keys, DELETE /tag/{resourceArn}?tagKeys=<one key> (observe 501 NotImplemented), then step task#8 GET /tag/{resourceArn} and observe the removed key still in $.tags; repeat with fixture s3tables-table-rename-and-tagging step task#10 and observe $.tags.stage still present.
4. Cause 3: fixture s3tables-table-metadata-location-update, step task#6 — PUT /tables/{tableBucketARN}/{namespace}/{name}/metadata-location with the table's current versionToken, and compare the response $.versionToken against the token held for that table: the emulator returns a new unrelated UUID.
