Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.batch-write-item-capacity-per-table-entries-include-delete-requests dynamodb.batch-write-item-index-capacity-arms-mixed-sparse-lsi-item dynamodb.batch-write-item-oversized-gsi-partition-key-value-rejected-whole dynamodb.update-item-creates-and-grows-items-across-the-1kb-write-capacity-boundary dynamodb.update-item-gsi-key-move-recharges-an-all-projected-lsi dynamodb.update-item-gsi-partition-key-value-over-2048-bytes-rejected dynamodb.number-byte-cost-trailing-zero-trim-depends-on-fraction-presence dynamodb.number-byte-cost-zero-trim-and-mid-fraction-literals-at-409600-boundary dynamodb.update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence dynamodb.update-item-arithmetic-clause-overhead-ignores-operand-name-length-and-alias dynamodb.update-item-clause-overhead-ignores-operand-value-and-removed-members dynamodb.update-item-list-index-set-target-charge-below-plain-set dynamodb.update-item-nested-path-set-target-charge-below-plain-set dynamodb.update-item-size-floor-excludes-short-sort-key dynamodb.partiql-select-keys-only-gsi-and-projected-attribute-list-lsi-charge-no-table-arm dynamodb.transact-write-cancellation-omits-capacity-and-collection-metrics
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.batch-write-item-capacity-per-table-entries-include-delete-requests|_wait_active#9|AssertionError|raise AssertionError(f"table {table} did not reach ACTIVE before the deadline")
Issue-Key: dynamodb.batch-write-item-index-capacity-arms-mixed-sparse-lsi-item|_wait_active#6|AssertionError|raise AssertionError(f"table {table} did not reach ACTIVE with its GSI before the deadline")
Issue-Key: dynamodb.batch-write-item-oversized-gsi-partition-key-value-rejected-whole|_wait_active#6|AssertionError|raise AssertionError(f"table {table} did not reach ACTIVE with its GSI before the deadline")
Issue-Key: dynamodb.update-item-creates-and-grows-items-across-the-1kb-write-capacity-boundary|_wait_active#6|AssertionError|raise AssertionError(f"table {table} not ACTIVE with its GSI before the deadline: {desc!r}")
Issue-Key: dynamodb.update-item-gsi-key-move-recharges-an-all-projected-lsi|_wait_active#6|AssertionError|raise AssertionError(f"table {table} did not reach ACTIVE with its GSI before the deadline: {desc!r}")
Issue-Key: dynamodb.update-item-gsi-partition-key-value-over-2048-bytes-rejected|_wait_active#6|AssertionError|raise AssertionError(f"table {table} not ACTIVE with its GSI before the deadline: {desc!r}")
Issue-Key: dynamodb.number-byte-cost-trailing-zero-trim-depends-on-fraction-presence|expect_error#6|AssertionError|raise AssertionError(f'expected the call to be refused, but it succeeded: {result!r}')
Issue-Key: dynamodb.number-byte-cost-zero-trim-and-mid-fraction-literals-at-409600-boundary|expect_error#6|AssertionError|raise AssertionError(f'expected the call to be refused, but it succeeded: {result!r}')
Issue-Key: dynamodb.update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence|run#8|AssertionError|assert lo_plain_2digit > lo_plain_3digit, (
Issue-Key: dynamodb.update-item-arithmetic-clause-overhead-ignores-operand-name-length-and-alias|_code_and_message#29|AssertionError|raise AssertionError(f'expected a refusal, but the call succeeded: {result!r}')
Issue-Key: dynamodb.update-item-clause-overhead-ignores-operand-value-and-removed-members|_code_and_message#24|AssertionError|raise AssertionError(f'expected a refusal, but the call succeeded: {result!r}')
Issue-Key: dynamodb.update-item-list-index-set-target-charge-below-plain-set|run#9|AssertionError|assert plain_lo - idx_lo == _DELTA_FROM_PLAIN_SET, (
Issue-Key: dynamodb.update-item-nested-path-set-target-charge-below-plain-set|run#9|AssertionError|assert plain_lo - nested_lo == _DELTA_FROM_PLAIN_SET, (
Issue-Key: dynamodb.update-item-size-floor-excludes-short-sort-key|_error#10|AssertionError|raise AssertionError(f"expected UpdateItem to be rejected, but it succeeded: {result!r}")
Issue-Key: dynamodb.partiql-select-keys-only-gsi-and-projected-attribute-list-lsi-charge-no-table-arm|run#12|AssertionError|assert cc['Table']['CapacityUnits'] == 0, (
Issue-Key: dynamodb.transact-write-cancellation-omits-capacity-and-collection-metrics|run#26|AssertionError|assert consumed.get('WriteCapacityUnits') == 3.0, (
Issue-Related: 20261111T170613Z-dynamodb-consumedcapacity-numbers-diverge-on-index-reads-and.md
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# DynamoDB: a table with a secondary index still never reaches ACTIVE inside a client's wait, UpdateItem's per-clause size overhead is still a flat item count, and two Number boundaries and two capacity arms are still off

> _Note: these causes were identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Provenance: these are causes 1, 3 and the open halves of causes 2, 4 and 5 of `20261117T172826Z-dynamodb-sizing-and-capacity-7-net-new-causes.md`. Causes 6 (the `TOTAL` answer's member set) and 7 (the 1 MB page budget), the item-ceiling half of cause 2 (a Number's byte width), the base-table arm of cause 4 and the set-valued half of cause 5 are fixed and that report now sits under `issues/things-that-now-work/`; these causes are unfixed and are re-filed here so their sixteen remaining checks are not recorded as working. Their `Issue-Key` and `Issue-Trace-Ids` lines moved with them._

## Operation
UpdateItem

## Description

Cause 1 of 5 — a table created with a secondary index never settles inside the suite's wait: `DescribeTable` never reports it ACTIVE together with its GSI, so six checks fail before they reach their first assertion.
AWS: `CreateTable` answers `TableStatus` `CREATING` and the table becomes `ACTIVE` with each `GlobalSecondaryIndexes[].IndexStatus` `ACTIVE` shortly after; `DescribeTable` is the documented way to wait, and the standard `table_exists` waiter completes.
Emulator: the wait expires (`table <name> did not reach ACTIVE with its GSI before the deadline`; one case never reaches ACTIVE at all).
What the fix attempt established, each built as a diagnostic in the handler and replayed, so whoever picks this up need not repeat it:
* it is NOT the statuses. With `Table::status` and `GlobalSecondaryIndex::status` forced to `ACTIVE` unconditionally, all six checks still fail at the same wait with the same message.
* it is NOT the table being absent. With `DescribeTable` answering an unknown table as a settled table carrying one `ACTIVE` index, nothing changed — so the table the checks wait for EXISTS in the store, and the create was not refused.
* it is NOT `DescribeTable` blocking out the creation span. Removing the sleep that waits for `Table::creation_settles_in` changed nothing.
* it is NOT the 501 that `CreateTable`/`UpdateTable` answer a secondary index carrying `OnDemandThroughput`. Accepting that member changed nothing.
* locally, over the wire, a create with one GSI, with an LSI, with both, over `PAY_PER_REQUEST` and `PROVISIONED` and over `ALL`/`KEYS_ONLY`/`INCLUDE` projections, settles on the FIRST `DescribeTable`; an index added by `UpdateTable` settles one transition span later, which a client must poll through because the reader waits out only the table's creation.
So what the six checks wait for is something the replay cannot distinguish from the above: the next attempt needs a way to see the requests those checks actually send (which table shape, and whether the index arrives with the create or through a later `UpdateTable`) before a rule can be stated.
Traces: dynamodb.batch-write-item-capacity-per-table-entries-include-delete-requests, dynamodb.batch-write-item-index-capacity-arms-mixed-sparse-lsi-item, dynamodb.batch-write-item-oversized-gsi-partition-key-value-rejected-whole, dynamodb.update-item-creates-and-grows-items-across-the-1kb-write-capacity-boundary, dynamodb.update-item-gsi-key-move-recharges-an-all-projected-lsi, dynamodb.update-item-gsi-partition-key-value-over-2048-bytes-rejected

Cause 2 of 5 — UpdateItem's 400 KB ceiling ignores the update expression's own clause overhead and the key bytes.
AWS: the size an `UpdateItem` is checked against includes the resulting item's attributes plus per-clause overhead that varies by clause kind — a plain `SET` costs more room than a list-index or nested-path `SET` target (which AWS puts 5 bytes ABOVE the plain ceiling), an arithmetic clause's overhead follows the operand's NAME length (an alias does not shrink it), an `ADD` or an arithmetic `SET` does not charge the result's digit GROWTH (`"49 + 49"` and `"99 + 99"` must share one ceiling), removed members and the operand value count, `if_not_exists` over an absent path charges differently from growth, and the key's own name and value bytes count — so the write at one byte over the ceiling is refused HTTP 400 `ValidationException`.
Emulator: the ceiling is a flat item-byte count plus the per-clause frames `crate::state` already models; the measured deltas are wrong by a few bytes in each direction (`list-index SET ceiling 409566 was -2 byte(s) below the plain SET ceiling 409564, expected 5`; `"49 + 49" (no digit growth) accepted up to 409517 bytes but "99 + 99" (one extra digit in the result) accepted up to 409516`).
What the fix attempt established: the digit-PARITY half of this is now right (a 3-digit number costs one byte more than a 2-digit one, where the two ceilings used to be identical — see the Number rule in the fixed report), which is what moved `update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence` from `run#8` to `run#11`. The remaining deltas are byte-exact overheads that neither the model nor the Developer Guide states, so they cannot be derived from the replay without fitting the measured ceilings, which is why they were left.
Traces: dynamodb.update-item-addition-and-if-not-exists-overhead-ignores-growth-and-path-absence, dynamodb.update-item-arithmetic-clause-overhead-ignores-operand-name-length-and-alias, dynamodb.update-item-clause-overhead-ignores-operand-value-and-removed-members, dynamodb.update-item-list-index-set-target-charge-below-plain-set, dynamodb.update-item-nested-path-set-target-charge-below-plain-set, dynamodb.update-item-size-floor-excludes-short-sort-key

Cause 3 of 5 — two Number byte-cost boundaries still disagree, in OPPOSITE directions.
AWS: a Number's contribution depends on its decimal encoding, so a write pinned one byte over 409,600 bytes is refused and the write at the boundary is stored.
Emulator: with the width rule the fixed report implements (1 byte per two significant digits rounded up, plus 1, plus 1 for a negative; a whole number's trailing zeros counted, a fraction's trimmed), `number-byte-cost-trailing-zero-trim-depends-on-fraction-presence` now REFUSES a `PutItem` that AWS stores (the emulator counts at least one byte too many for the form it uses) while `number-byte-cost-zero-trim-and-mid-fraction-literals-at-409600-boundary` still STORES one AWS refuses (at least one byte too few). The two cannot both be satisfied by any single digit-counting rule the fix attempt could state, so the forms those two checks use have to be identified before the rule can be corrected; the sibling boundary check over two Number attributes (`batch-write-item-two-number-attributes-byte-cost-sum-pinned-at-400kb-boundary`) is pinned to the byte and PASSES under the current rule.
Traces: dynamodb.number-byte-cost-trailing-zero-trim-depends-on-fraction-presence, dynamodb.number-byte-cost-zero-trim-and-mid-fraction-literals-at-409600-boundary

Cause 4 of 5 — a PartiQL `SELECT` from an index reports the wrong figure on the index arm.
AWS: a keyed `SELECT` satisfied entirely from a `KEYS_ONLY` GSI charges the `Table` arm nothing and the index arm what reading the index cost.
Emulator: the `Table` arm is right now (the fix bills the index rather than the table), and the GSI arm's own figure is not: the check fails one assertion later, at `cc['GlobalSecondaryIndexes'][<index>]['CapacityUnits'] == _INDEX_RCU_EVENTUAL`. The statement's plan rounds per row where the expected figure appears to be one rounding over the page (or the reverse); which of the two a `SELECT` follows, and whether it follows its own plan's native reader (`Query` rounds once over the page, `Scan` once per item), is the open question.
Traces: dynamodb.partiql-select-keys-only-gsi-and-projected-attribute-list-lsi-charge-no-table-arm

Cause 5 of 5 — a committed TransactWriteItems Put projected into one LSI reports the wrong doubled total.
AWS: the transactional write costs 3.0 write capacity units for that item and that projection.
Emulator: 4.0 — the table's arm and the index's arm are each doubled. Which arm AWS does not double (or what size it takes the index arm over) cannot be read off the replay, and the fix attempt declined to guess a divisor.
Traces: dynamodb.transact-write-cancellation-omits-capacity-and-collection-metrics

## Evidence
From the behavioral replay, after the fix that closed the other causes (16 of the original 25 checks still fail):
- Table/index never ACTIVE: 'AssertionError: table sim2real-behav-ddbmix-<id> did not reach ACTIVE with its GSI before the deadline' (and 'table sim2real-behav-ddbbw-a-<id> did not reach ACTIVE before the deadline').
- UpdateItem clause overhead: '"49 + 49" (no digit growth) accepted up to 409517 bytes but "99 + 99" (one extra digit in the result) accepted up to 409516'; 'list-index SET ceiling 409566 was -2 byte(s) below the plain SET ceiling 409564, expected 5'; 'expected UpdateItem to be rejected, but it succeeded'.
- Number boundaries: 'ClientError: An error occurred (ValidationException) when calling the PutItem operation: Item size has exceeded the maximum allowed size' for the call that must be stored, and 'expected the call to be refused, but it succeeded' for the other.
- PartiQL index arm: "assert cc['GlobalSecondaryIndexes'][_GSI]['CapacityUnits'] == _INDEX_RCU_EVENTUAL".
- Transactional write: 'a committed TransactWriteItems Put projected into one LSI reported WriteCapacityUnits 4.0, expected 3.0'.
(The table names, the 409,517/409,516 figures and the item sizes are this run's; the per-clause overhead model, the digit rule and the doubling are the requirement.)

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the check ids in trace_ids; each reproduces one cause.
3. Representative manual repro for cause 1: CreateTable with one GlobalSecondaryIndex, then poll DescribeTable — locally this settles on the first poll, so the next attempt needs the suite's own request sequence to reproduce the hang.
4. For cause 2: UpdateItem with a plain `SET`, a list-index `SET` and a nested-path `SET` at the item ceiling and compare the three boundaries.
5. For cause 4: `SELECT *` through a KEYS_ONLY GSI with ReturnConsumedCapacity='INDEXES' and compare the index arm.
6. For cause 5: TransactWriteItems a Put into a table with one LSI and compare WriteCapacityUnits.
