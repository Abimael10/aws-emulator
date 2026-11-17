Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.batch-write-item-capacity-per-table-entries-include-delete-requests dynamodb.batch-write-item-index-capacity-arms-mixed-sparse-lsi-item dynamodb.batch-write-item-oversized-gsi-partition-key-value-rejected-whole dynamodb.update-item-creates-and-grows-items-across-the-1kb-write-capacity-boundary dynamodb.update-item-gsi-key-move-recharges-an-all-projected-lsi dynamodb.update-item-gsi-partition-key-value-over-2048-bytes-rejected dynamodb.batch-write-item-two-number-attributes-byte-cost-sum-pinned-at-400kb-boundary dynamodb.number-byte-cost-trailing-zero-trim-depends-on-fraction-presence dynamodb.number-byte-cost-zero-trim-and-mid-fraction-literals-at-409600-boundary dynamodb.update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence dynamodb.update-item-arithmetic-clause-overhead-ignores-operand-name-length-and-alias dynamodb.update-item-clause-overhead-ignores-operand-value-and-removed-members dynamodb.update-item-list-index-set-target-charge-below-plain-set dynamodb.update-item-nested-path-set-target-charge-below-plain-set dynamodb.update-item-size-floor-excludes-short-sort-key dynamodb.lsi-non-projected-query-charges-base-item-on-top-of-the-keys-only-index-read dynamodb.lsi-select-all-attributes-query-bills-base-table-per-item-across-multiple-items dynamodb.partiql-select-keys-only-gsi-and-projected-attribute-list-lsi-charge-no-table-arm dynamodb.delete-item-write-capacity-billed-on-mixed-number-binary-set-item-size dynamodb.transact-write-cancellation-omits-capacity-and-collection-metrics dynamodb.scan-return-consumed-capacity-invalid-enum-rejected dynamodb.validation-before-table-lookup-deleteitem-and-getitem-rejected dynamodb.scan-1mb-page-boundary-counts-attribute-name-bytes-not-only-values dynamodb.scan-1mb-page-boundary-on-a-hash-only-table-with-small-attributes dynamodb.scan-1mb-page-is-the-minimal-byte-prefix-crossing-the-limit
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.batch-write-item-capacity-per-table-entries-include-delete-requests|_wait_active#9|AssertionError|raise AssertionError(f"table {table} did not reach ACTIVE before the deadline")
Issue-Key: dynamodb.batch-write-item-index-capacity-arms-mixed-sparse-lsi-item|_wait_active#6|AssertionError|raise AssertionError(f"table {table} did not reach ACTIVE with its GSI before the deadline")
Issue-Key: dynamodb.batch-write-item-oversized-gsi-partition-key-value-rejected-whole|_wait_active#6|AssertionError|raise AssertionError(f"table {table} did not reach ACTIVE with its GSI before the deadline")
Issue-Key: dynamodb.update-item-creates-and-grows-items-across-the-1kb-write-capacity-boundary|_wait_active#6|AssertionError|raise AssertionError(f"table {table} not ACTIVE with its GSI before the deadline: {desc!r}")
Issue-Key: dynamodb.update-item-gsi-key-move-recharges-an-all-projected-lsi|_wait_active#6|AssertionError|raise AssertionError(f"table {table} did not reach ACTIVE with its GSI before the deadline: {desc!r}")
Issue-Key: dynamodb.update-item-gsi-partition-key-value-over-2048-bytes-rejected|_wait_active#6|AssertionError|raise AssertionError(f"table {table} not ACTIVE with its GSI before the deadline: {desc!r}")
Issue-Key: dynamodb.batch-write-item-two-number-attributes-byte-cost-sum-pinned-at-400kb-boundary|expect_error#15|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb.number-byte-cost-trailing-zero-trim-depends-on-fraction-presence|expect_error#6|AssertionError|raise AssertionError(f'expected the call to be refused, but it succeeded: {result!r}')
Issue-Key: dynamodb.number-byte-cost-zero-trim-and-mid-fraction-literals-at-409600-boundary|expect_error#6|AssertionError|raise AssertionError(f'expected the call to be refused, but it succeeded: {result!r}')
Issue-Key: dynamodb.update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence|run#8|AssertionError|assert lo_plain_2digit > lo_plain_3digit, (
Issue-Key: dynamodb.update-item-arithmetic-clause-overhead-ignores-operand-name-length-and-alias|_code_and_message#29|AssertionError|raise AssertionError(f'expected a refusal, but the call succeeded: {result!r}')
Issue-Key: dynamodb.update-item-clause-overhead-ignores-operand-value-and-removed-members|_code_and_message#24|AssertionError|raise AssertionError(f'expected a refusal, but the call succeeded: {result!r}')
Issue-Key: dynamodb.update-item-list-index-set-target-charge-below-plain-set|run#9|AssertionError|assert plain_lo - idx_lo == _DELTA_FROM_PLAIN_SET, (
Issue-Key: dynamodb.update-item-nested-path-set-target-charge-below-plain-set|run#9|AssertionError|assert plain_lo - nested_lo == _DELTA_FROM_PLAIN_SET, (
Issue-Key: dynamodb.update-item-size-floor-excludes-short-sort-key|_error#10|AssertionError|raise AssertionError(f"expected UpdateItem to be rejected, but it succeeded: {result!r}")
Issue-Key: dynamodb.lsi-select-all-attributes-query-bills-base-table-per-item-across-multiple-items|run#22|AssertionError|assert fetched_cc['Table']['CapacityUnits'] == _TABLE_RCU, (
Issue-Key: dynamodb.partiql-select-keys-only-gsi-and-projected-attribute-list-lsi-charge-no-table-arm|run#12|AssertionError|assert cc['Table']['CapacityUnits'] == 0, (
Issue-Key: dynamodb.delete-item-write-capacity-billed-on-mixed-number-binary-set-item-size|run#14|AssertionError|assert cc_over["CapacityUnits"] == 2.0, (
Issue-Key: dynamodb.transact-write-cancellation-omits-capacity-and-collection-metrics|run#26|AssertionError|assert consumed.get('WriteCapacityUnits') == 3.0, (
Issue-Key: dynamodb.scan-return-consumed-capacity-invalid-enum-rejected|run#11|AssertionError|assert set(control["ConsumedCapacity"]) == {"TableName", "CapacityUnits"}, (
Issue-Key: dynamodb.validation-before-table-lookup-deleteitem-and-getitem-rejected|run#17|AssertionError|assert capacity == {'TableName': table, 'CapacityUnits': 1.0}, (
Issue-Key: dynamodb.scan-1mb-page-boundary-counts-attribute-name-bytes-not-only-values|run#17|AssertionError|assert resp2["Count"] == resp2["ScannedCount"] == 12, (
Issue-Key: dynamodb.scan-1mb-page-boundary-on-a-hash-only-table-with-small-attributes|run#16|AssertionError|assert total > _ONE_MB, (
Issue-Key: dynamodb.scan-1mb-page-is-the-minimal-byte-prefix-crossing-the-limit|run#20|AssertionError|assert resp2["Count"] == resp2["ScannedCount"] == 12, (
Issue-Related: 20261111T170613Z-dynamodb-consumedcapacity-numbers-diverge-on-index-reads-and.md
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# DynamoDB sizing and capacity: 7 net-new causes — a table with a secondary index never reaches ACTIVE, the 400 KB and 1 MB byte models are wrong, and index/total capacity arms are mis-billed

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateItem

## Description
Seven net-new root causes in DynamoDB's size accounting, capacity accounting and table/index state machine. All grounded in failing checks listed under '### dynamodb'. Related open issues that do NOT cover these checks: 20261111T170613Z-dynamodb-item-size-batch-size-and-request-size.md, 20261111T170613Z-dynamodb-consumedcapacity-numbers-diverge-on-index-reads-and.md, 20261117T045808Z-dynamodb-updateitem-s-item-size-ceiling-is-not.md.

Cause 1 of 7: a table created with a secondary index never settles — it does not reach ACTIVE with its index inside the suite's wait.
AWS: CreateTable answers TableStatus 'CREATING' and the table becomes ACTIVE with each GlobalSecondaryIndexes[].IndexStatus 'ACTIVE' shortly after; DescribeTable is the documented way to wait, and the standard table_exists waiter completes.
Emulator: DescribeTable never reports the table ACTIVE together with its GSI inside the wait, so every check that seeds such a table fails before it reaches its assertion ('table <name> did not reach ACTIVE with its GSI before the deadline'; one case never reaches ACTIVE at all).
Why: no client can use a table with a secondary index — the universal create-then-wait-then-write sequence hangs, and six behavioral checks are blocked from grading anything.
Fix: the CreateTable/UpdateTable control-plane state machine must advance table TableStatus and each index IndexStatus to ACTIVE, and DescribeTable must report it.
Traces: dynamodb.batch-write-item-capacity-per-table-entries-include-delete-requests, dynamodb.batch-write-item-index-capacity-arms-mixed-sparse-lsi-item, dynamodb.batch-write-item-oversized-gsi-partition-key-value-rejected-whole, dynamodb.update-item-creates-and-grows-items-across-the-1kb-write-capacity-boundary, dynamodb.update-item-gsi-key-move-recharges-an-all-projected-lsi, dynamodb.update-item-gsi-partition-key-value-over-2048-bytes-rejected

Cause 2 of 7: the byte cost of a Number attribute is not modeled, so items over the 400 KB item limit are accepted.
AWS: an item may not exceed 409,600 bytes; a Number's contribution depends on its decimal encoding (trailing zeros are trimmed only when a fraction is present, mid-fraction literals cost their own digits), so a write pinned one byte over the boundary is refused with HTTP 400 ValidationException 'Item size has exceeded the maximum allowed size' (PutItem/BatchWriteItem) and nothing is stored.
Emulator: PutItem and BatchWriteItem answer HTTP 200 (BatchWriteItem with 'UnprocessedItems': {}) for the same items.
Why: an item that real AWS refuses is stored, so the application's size guard is untested and the same code fails only in production.
Fix: the item-size accounting used by PutItem/BatchWriteItem must compute the Number byte width from its canonical decimal form.
Traces: dynamodb.batch-write-item-two-number-attributes-byte-cost-sum-pinned-at-400kb-boundary, dynamodb.number-byte-cost-trailing-zero-trim-depends-on-fraction-presence, dynamodb.number-byte-cost-zero-trim-and-mid-fraction-literals-at-409600-boundary

Cause 3 of 7: UpdateItem's 400 KB ceiling ignores the update expression's own clause overhead and the key bytes.
AWS: the size an UpdateItem is checked against includes the resulting item's attributes plus per-clause overhead that varies by clause kind — a plain SET costs more room than a list-index or nested-path SET target, an arithmetic clause's overhead follows the operand's NAME length (an alias does not shrink it), removed members and the operand value count, if_not_exists over an absent path charges differently from growth, and the key's own name and value bytes count — so the write at one byte over the ceiling is refused with HTTP 400 ValidationException.
Emulator: the ceiling is a flat item-byte count: the same request succeeds with HTTP 200, the measured ceilings for a plain SET and a 3-digit SET are identical (409548 vs 409548), and a list-index or nested-path SET ceiling comes out 2 bytes BELOW the plain SET ceiling where AWS puts it 5 bytes above.
Why: the exact boundary at which a growing item starts failing differs from AWS, so size-sensitive writes that AWS refuses are applied (and vice-versa near the edge).
Fix: the UpdateItem size-ceiling calculation (per-clause overhead model and key-byte inclusion).
Traces: dynamodb.update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence, dynamodb.update-item-arithmetic-clause-overhead-ignores-operand-name-length-and-alias, dynamodb.update-item-clause-overhead-ignores-operand-value-and-removed-members, dynamodb.update-item-list-index-set-target-charge-below-plain-set, dynamodb.update-item-nested-path-set-target-charge-below-plain-set, dynamodb.update-item-size-floor-excludes-short-sort-key

Cause 4 of 7: a read through a secondary index does not bill the base-table arm for the item fetch it performs.
AWS: Smithy — ConsumedCapacity carries Table / LocalSecondaryIndexes / GlobalSecondaryIndexes arms. When a query through a KEYS_ONLY or partially-projected index must fetch non-projected attributes from the base table, the Table arm is billed each item's own 4 KB-rounded size (an 8,019-byte item costs 2 RCU; three items of 1,024/5,120/9,216 bytes cost 1+2+3 = 6 RCU). Conversely a keyed SELECT that is satisfied ENTIRELY from the index (SELECT * on a KEYS_ONLY GSI, or a projected-attribute list on an LSI) bills the Table arm nothing.
Emulator: the Table arm reads {'ReadCapacityUnits': 0.0, 'CapacityUnits': 0.0} when a base-table fetch did happen, and {'ReadCapacityUnits': 1.5, 'CapacityUnits': 1.5} when no fetch was needed — the two cases are inverted.
Why: cost and throttling models built on ConsumedCapacity are wrong in both directions for every index read.
Fix: the Query/Scan/PartiQL-SELECT capacity accounting must decide the base-table arm from whether the projection required a base-table fetch.
Traces: dynamodb.lsi-non-projected-query-charges-base-item-on-top-of-the-keys-only-index-read, dynamodb.lsi-select-all-attributes-query-bills-base-table-per-item-across-multiple-items, dynamodb.partiql-select-keys-only-gsi-and-projected-attribute-list-lsi-charge-no-table-arm

Cause 5 of 7: write capacity is sized from the wrong item bytes for set-valued items and LSI-projected transactional writes.
AWS: a write costs ceil(size/1 KB) WCU (doubled for a transactional write), with the size taken from the item's full attribute encoding including N/B/BS members — so deleting a 1,025-byte mixed N/B/BS item costs 2.0 WCU, and a committed TransactWriteItems Put projected into one LSI costs 3.0 WCU.
Emulator: reports 1.0 WCU for the delete and 4.0 WCU for the transactional Put.
Why: capacity totals a client bills or alarms on are wrong.
Fix: the write-capacity sizing shared by DeleteItem and TransactWriteItems (set-member byte accounting and LSI-projection arms).
Traces: dynamodb.delete-item-write-capacity-billed-on-mixed-number-binary-set-item-size, dynamodb.transact-write-cancellation-omits-capacity-and-collection-metrics

Cause 6 of 7: ReturnConsumedCapacity=TOTAL emits per-direction capacity members AWS omits.
AWS: with TOTAL the response's ConsumedCapacity carries table-level capacity only — {'TableName': <name>, 'CapacityUnits': <n>}; ReadCapacityUnits/WriteCapacityUnits (Smithy: ConsumedCapacity.ReadCapacityUnits, .WriteCapacityUnits) are not part of the TOTAL answer.
Emulator: returns {'TableName': <name>, 'CapacityUnits': 1.0, 'ReadCapacityUnits': 1.0} for Scan and the same extra member for a strongly consistent GetItem/DeleteItem.
Why: a strict comparison of the ConsumedCapacity structure (or a client that switches on which members are present to infer the request's ReturnConsumedCapacity level) sees a shape AWS never sends.
Fix: the ConsumedCapacity serializer must emit only the members the requested level includes.
Traces: dynamodb.scan-return-consumed-capacity-invalid-enum-rejected, dynamodb.validation-before-table-lookup-deleteitem-and-getitem-rejected

Cause 7 of 7: the Scan 1 MB page budget is computed over the wrong byte model, so a page stops one item early and under the limit.
AWS: a Scan page is the minimal prefix of items whose cumulative size CROSSES 1 MB — the per-item size counts attribute NAME bytes as well as values — so a truncated page (one carrying LastEvaluatedKey) always exceeds 1,048,576 bytes, and the boundary cases page at Count/ScannedCount 12.
Emulator: pages at Count=11/ScannedCount=11 where AWS gives 12, and returns a LastEvaluatedKey on a page totalling only 1,024,240 bytes — under the limit.
Why: pagination boundaries differ from AWS, so a client's page count, LastEvaluatedKey sequence and per-page item sets all diverge.
Fix: the Scan paginator's page-budget accounting (include attribute-name bytes; stop on the first item that crosses, not before it).
Traces: dynamodb.scan-1mb-page-boundary-counts-attribute-name-bytes-not-only-values, dynamodb.scan-1mb-page-boundary-on-a-hash-only-table-with-small-attributes, dynamodb.scan-1mb-page-is-the-minimal-byte-prefix-crossing-the-limit

## Evidence
From the behavioral replay, per cause:
- Table/index never ACTIVE: 'AssertionError: table sim2real-behav-ddbmix-<id> did not reach ACTIVE with its GSI before the deadline' (and 'table sim2real-behav-ddbbw-a-<id> did not reach ACTIVE before the deadline').
- Number byte cost: 'expected the call to be refused, but it succeeded: {'UnprocessedItems': {}, ... 'HTTPStatusCode': 200 ...}' and 'expected the call to be refused, but it succeeded: {... 'HTTPStatusCode': 200, 'content-length': '2' ...}'.
- UpdateItem clause overhead: 'a plain SET to the 3-digit number did not cost more room than the 2-digit one: 409548 vs 409548'; 'list-index SET ceiling 409566 was -2 byte(s) below the plain SET ceiling 409564, expected 5'; 'a standalone nested-path SET ceiling was 409565, -2 byte(s) below the plain whole-attribute SET ceiling 409563, expected 5'; 'expected UpdateItem to be rejected, but it succeeded'.
- Index read arms: 'the base-table fetch of a 8019-byte item must bill exactly 2 RCU on the Table arm, got {'ReadCapacityUnits': 0.0, 'CapacityUnits': 0.0}'; 'the base-table fetch of 3 items sized (1024, 5120, 9216) raw bytes must bill the SUM of each item's own 4 KB-rounded size on the Table arm, 1 + 2 + 3 = 6 RCU, got {'ReadCapacityUnits': 0.0, 'CapacityUnits': 0.0}'; 'SELECT * on a KEYS_ONLY GSI must charge the Table arm nothing, got {'ReadCapacityUnits': 1.5, 'CapacityUnits': 1.5}'.
- Write capacity: 'deleting the 1025-byte mixed N/B/BS item must cost 2.0 write capacity units, got 1.0'; 'a committed TransactWriteItems Put projected into one LSI reported WriteCapacityUnits 4.0, expected 3.0'.
- TOTAL shape: 'ReturnConsumedCapacity=TOTAL must report table-level capacity only, got {'TableName': ..., 'CapacityUnits': 1.0, 'ReadCapacityUnits': 1.0}'.
- Scan paging: 'Count=11 ScannedCount=11, expected 12 and 12'; 'page 1 carries a LastEvaluatedKey, so it was truncated by the 1 MB budget and must exceed it; got 1024240 bytes over 20 items'.

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the check ids in trace_ids; each reproduces one cause.
3. Representative manual repro: CreateTable with one GlobalSecondaryIndex, then poll DescribeTable — the table/index never reports ACTIVE.
4. PutItem an item pinned one byte over 409,600 bytes using two Number attributes with trailing zeros — AWS refuses with ValidationException, the emulator answers HTTP 200.
5. Query a KEYS_ONLY LSI for a non-projected attribute with ReturnConsumedCapacity=INDEXES — AWS bills the Table arm the item's 4 KB-rounded size, the emulator bills 0.
6. Scan a table whose items total just over 1 MB — compare Count/ScannedCount and the page's byte total with AWS.
