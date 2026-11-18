Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.batch-write-item-two-number-attributes-byte-cost-sum-pinned-at-400kb-boundary dynamodb.lsi-non-projected-query-charges-base-item-on-top-of-the-keys-only-index-read dynamodb.lsi-select-all-attributes-query-bills-base-table-per-item-across-multiple-items dynamodb.delete-item-write-capacity-billed-on-mixed-number-binary-set-item-size dynamodb.scan-return-consumed-capacity-invalid-enum-rejected dynamodb.validation-before-table-lookup-deleteitem-and-getitem-rejected dynamodb.scan-1mb-page-boundary-counts-attribute-name-bytes-not-only-values dynamodb.scan-1mb-page-boundary-on-a-hash-only-table-with-small-attributes dynamodb.scan-1mb-page-is-the-minimal-byte-prefix-crossing-the-limit
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.batch-write-item-two-number-attributes-byte-cost-sum-pinned-at-400kb-boundary|expect_error#15|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb.lsi-select-all-attributes-query-bills-base-table-per-item-across-multiple-items|run#22|AssertionError|assert fetched_cc['Table']['CapacityUnits'] == _TABLE_RCU, (
Issue-Key: dynamodb.delete-item-write-capacity-billed-on-mixed-number-binary-set-item-size|run#14|AssertionError|assert cc_over["CapacityUnits"] == 2.0, (
Issue-Key: dynamodb.scan-return-consumed-capacity-invalid-enum-rejected|run#11|AssertionError|assert set(control["ConsumedCapacity"]) == {"TableName", "CapacityUnits"}, (
Issue-Key: dynamodb.validation-before-table-lookup-deleteitem-and-getitem-rejected|run#17|AssertionError|assert capacity == {'TableName': table, 'CapacityUnits': 1.0}, (
Issue-Key: dynamodb.scan-1mb-page-boundary-counts-attribute-name-bytes-not-only-values|run#17|AssertionError|assert resp2["Count"] == resp2["ScannedCount"] == 12, (
Issue-Key: dynamodb.scan-1mb-page-boundary-on-a-hash-only-table-with-small-attributes|run#16|AssertionError|assert total > _ONE_MB, (
Issue-Key: dynamodb.scan-1mb-page-is-the-minimal-byte-prefix-crossing-the-limit|run#20|AssertionError|assert resp2["Count"] == resp2["ScannedCount"] == 12, (
Issue-Related: 20261111T170613Z-dynamodb-consumedcapacity-numbers-diverge-on-index-reads-and.md
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# DynamoDB sizing and capacity: a Number's byte width, the base-table arm of an index read, the members a TOTAL answer carries and the 1 MB page boundary

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Resolution: this report was filed as seven causes. Four of them are closed here — the Number byte width that the 400 KB ceiling, the 1 KB write unit and the 4 KB read unit are all measured in (cause 2's item-ceiling half), the arms of a read served from a secondary index (cause 4), the members a `ReturnConsumedCapacity` level includes (cause 6) and the 1 MB page boundary (cause 7) — together with the set-valued half of cause 5, which the Number rule carried. The nine checks above pass. The causes that remain — cause 1 (a table with a secondary index never reaching ACTIVE inside a client's wait), cause 3 (UpdateItem's per-clause size overhead), two Number boundary checks that disagree in opposite directions, the PartiQL index arm's own figure and the transactional half of cause 5 — are re-filed under `issues/things-that-do-not-work/20261118T031500Z-dynamodb-a-secondary-index-table-never-reaches-active-and-updateitem-clause-overhead.md`, with their `Issue-Key` and `Issue-Trace-Ids` lines and with what this fix established about each, so they are not recorded as working._

## Operation
UpdateItem

## Description
Four root causes in DynamoDB's size accounting and capacity accounting, all grounded in the failing checks listed above.

Cause 1 of 4: the byte cost of a Number attribute was not modelled, so items over the 400 KB item limit were accepted.
AWS: an item may not exceed 409,600 bytes, and an `N` value is stored at 1 byte per two significant digits ROUNDED UP plus 1 byte, with one more byte for a negative number — which is what makes an `N` at the documented 38-digit precision ceiling the documented 21 bytes wide at most. The significant digits are the ones the value is spelled with, leading zeros left out: the zeros a whole number carries before the decimal point are digits of it, the zeros a fraction ends with are not, a zero between two significant digits always is, and a magnitude past the precision ceiling is spelled with an exponent rather than that run of zeros. A write pinned one byte over the boundary is refused HTTP 400 `ValidationException` 'Item size has exceeded the maximum allowed size' and nothing is stored.
Emulator: `state.rs::number_size` divided the digit count by two with truncation and added no sign byte, so every odd digit count cost what the even one below it costs and `PutItem`/`BatchWriteItem` answered HTTP 200 for items AWS refuses.
Fix: `NumberKey::significant_digits` counts the digits the value is spelled with, and the new `NumberKey::stored_bytes` rounds up and adds the sign byte. One function sizes an `N` wherever it is measured, so the item ceiling, the 1 KB write unit (a 1,025-byte mixed N/B/BS item costs 2.0 WCU), the 4 KB read unit, the 1 MB page budget and the index-entry sizes moved together.
Traces: dynamodb.batch-write-item-two-number-attributes-byte-cost-sum-pinned-at-400kb-boundary, dynamodb.delete-item-write-capacity-billed-on-mixed-number-binary-set-item-size

Cause 2 of 4: a read through a secondary index did not bill the base-table arm for the item fetch it performed, and billed it for reads that performed none.
AWS: a read served from a secondary index spends its units ON THAT INDEX, over what the index HOLDS for each row; the table's own arm carries what the read spent on the TABLE — nothing for a read the index answered alone ("reads only the index and not the table"), and each fetched item's own size rounded up to the 4 KB read unit where the read had to go on to the table for attributes the index does not project ("DynamoDB fetches each of these attributes from the parent table"). The aggregate is the two together.
Emulator: the `Table` arm was hard-coded to zero for every index read, the index arm was billed on the whole fetched item rather than on the index entry, and a PartiQL `SELECT` from an index billed the table rather than the index.
Fix: `consumed_index_capacity` takes the fetch arm, `Query`/`Scan` sum it per fetched item and bill the index on what it holds, and the PartiQL `SELECT` bills through the same pair.
Traces: dynamodb.lsi-non-projected-query-charges-base-item-on-top-of-the-keys-only-index-read, dynamodb.lsi-select-all-attributes-query-bills-base-table-per-item-across-multiple-items

Cause 3 of 4: `ReturnConsumedCapacity=TOTAL` emitted per-direction capacity members AWS omits.
AWS: `TOTAL` asks for "only the aggregate ConsumedCapacity for the operation", so that answer carries `TableName` and `CapacityUnits`; the per-direction figures itemise the aggregate and belong to the level that asks for the itemisation, beside the `Table` and per-index lines. A transactional call names its kind at either level, because which kind it billed is part of its answer: a same-token `TransactWriteItems` replay reports READ capacity where the call that committed reported write.
Emulator: every answer carried the per-direction member, so a strict comparison of the `ConsumedCapacity` structure saw a shape AWS never sends.
Fix: `CapacityKind::consumed` takes the level and a `KindReport`; the transactional operations name their kind, every other data-plane operation reports the aggregate at `TOTAL`.
Traces: dynamodb.scan-return-consumed-capacity-invalid-enum-rejected, dynamodb.validation-before-table-lookup-deleteitem-and-getitem-rejected

Cause 4 of 4: the 1 MB page budget stopped a page one item early and under the limit.
AWS: a page is the SHORTEST run of items whose sizes CROSS 1 MB — "if the total size of scanned items exceeds the maximum dataset size limit of 1 MB, the scan completes" — so the item that carries the page past the limit is part of that page, a page carrying a `LastEvaluatedKey` because the budget stopped it always exceeds 1,048,576 bytes, and each item is measured as the store holds it, attribute names as well as values.
Emulator: `Scan` stopped BEFORE the crossing item, and a page whose budget was spent reported no stop key when no row was left over.
Fix: the rule lives once in `state::page_budget_spent`, used by `Scan`, `Query`, the PartiQL `SELECT` and `SearchVectors`, and a page the budget stopped reports where it stopped whether or not a row remains.
Traces: dynamodb.scan-1mb-page-boundary-counts-attribute-name-bytes-not-only-values, dynamodb.scan-1mb-page-boundary-on-a-hash-only-table-with-small-attributes, dynamodb.scan-1mb-page-is-the-minimal-byte-prefix-crossing-the-limit

## Evidence
Expected vs actual, verbatim from the replay that filed this report:
- Number byte cost: 'expected the call to be refused, but it succeeded: {'UnprocessedItems': {}, ... 'HTTPStatusCode': 200 ...}'.
- Write capacity: 'deleting the 1025-byte mixed N/B/BS item must cost 2.0 write capacity units, got 1.0'.
- Index read arms: 'the base-table fetch of a 8019-byte item must bill exactly 2 RCU on the Table arm, got {'ReadCapacityUnits': 0.0, 'CapacityUnits': 0.0}'; 'the base-table fetch of 3 items sized (1024, 5120, 9216) raw bytes must bill the SUM of each item's own 4 KB-rounded size on the Table arm, 1 + 2 + 3 = 6 RCU, got {'ReadCapacityUnits': 0.0, 'CapacityUnits': 0.0}'.
- TOTAL shape: 'ReturnConsumedCapacity=TOTAL must report table-level capacity only, got {'TableName': ..., 'CapacityUnits': 1.0, 'ReadCapacityUnits': 1.0}'.
- Scan paging: 'Count=11 ScannedCount=11, expected 12 and 12'; 'page 1 carries a LastEvaluatedKey, so it was truncated by the 1 MB budget and must exceed it; got 1024240 bytes over 20 items'.
(The item sizes, counts and byte totals are that run's; the per-digit, per-4 KB, per-1 KB and per-1 MB rules are the requirement.)

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the check ids in trace_ids; each reproduces one cause.
3. PutItem an item pinned one byte over 409,600 bytes using two Number attributes with trailing zeros — AWS refuses with ValidationException, the emulator used to answer HTTP 200.
4. Query a KEYS_ONLY LSI for a non-projected attribute with ReturnConsumedCapacity=INDEXES — AWS bills the Table arm each item's 4 KB-rounded size, the emulator used to bill 0.
5. Scan with ReturnConsumedCapacity=TOTAL and compare the member set of ConsumedCapacity.
6. Scan a table whose items total just over 1 MB — compare Count/ScannedCount and the page's byte total with AWS.
