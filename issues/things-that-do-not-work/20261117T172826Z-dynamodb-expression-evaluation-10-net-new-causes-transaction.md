Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.transact-update-operand-type-mismatch-with-stored-value-cancels-rejected dynamodb.transact-update-path-invalid-for-stored-item-cancels-rejected dynamodb.transact-write-items-condition-evaluated-before-update-runtime-validation dynamodb.transact-write-items-delete-operand-type-and-condition-check-reasons-track-item-identity dynamodb.transact-write-items-key-schema-mismatch-suppresses-sibling-cancellation-reason dynamodb.transact-write-items-update-child-of-list-scalar-cancels-with-validation-error dynamodb.transact-write-items-update-runtime-arithmetic-error-reports-validation-error-reason dynamodb.execute-transaction-update-path-invalid-for-stored-item-cancels-rejected dynamodb.update-item-add-delete-on-map-attribute-and-nested-arithmetic-type-mismatch-rejected dynamodb.update-item-set-arithmetic-wrong-type-source-into-different-target-rejected dynamodb.legacy-attribute-updates-add-binary-scalar-rejected dynamodb.nested-placeholder-path-missing-segment-rejected dynamodb.update-remove-add-delete-through-missing-key-under-an-existing-map-rejected dynamodb.update-remove-add-delete-through-missing-parent-rejected dynamodb.update-remove-through-missing-nested-parent-rejected dynamodb.update-through-bracket-index-on-non-list-rejected dynamodb.update-item-nested-function-operand-rejected dynamodb.update-item-add-arithmetic-result-exceeding-precision-rejected dynamodb.update-item-set-arithmetic-result-over-38-significant-digits-rejected dynamodb.delete-item-condition-expression-non-distinct-operands-rejected dynamodb.scan-filter-expression-non-distinct-comparator-and-aliased-operands-rejected dynamodb.scan-filter-expression-non-distinct-function-nested-path-operands-rejected dynamodb.scan-filterexpression-key-path-through-function-or-alias-rejected dynamodb.gsi-query-filter-on-base-table-pk-and-sk-narrows-to-one-item dynamodb.gsi-query-filter-on-its-own-key-attributes-rejected dynamodb.query-gsi-and-lsi-filter-on-base-table-sort-key-accepted dynamodb.scan-filter-size-counts-utf16-code-units-not-bytes
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.transact-update-operand-type-mismatch-with-stored-value-cancels-rejected|run#11|AssertionError|assert err['Error']['Code'] == CANCELED, f'{expression}: expected {CANCELED}, got {err["Error"]!r}'
Issue-Key: dynamodb.transact-update-path-invalid-for-stored-item-cancels-rejected|run#10|AssertionError|assert err['Error']['Code'] == CANCELED, f'expected {CANCELED}, got {err["Error"]!r}'
Issue-Key: dynamodb.transact-write-items-condition-evaluated-before-update-runtime-validation|run#7|AssertionError|assert err["Error"]["Code"] == _ERR_CANCELED, f"expected {_ERR_CANCELED}, got {err['Error']!r}"
Issue-Key: dynamodb.transact-write-items-delete-operand-type-and-condition-check-reasons-track-item-identity|run#13|AssertionError|assert err1["Code"] == ERR_CANCELLED, f"expected {ERR_CANCELLED}, got {err1['Code']!r}"
Issue-Key: dynamodb.transact-write-items-key-schema-mismatch-suppresses-sibling-cancellation-reason|run#19|AssertionError|assert code == "TransactionCanceledException", f"baseline: expected TransactionCanceledException, got {code!r}"
Issue-Key: dynamodb.transact-write-items-update-child-of-list-scalar-cancels-with-validation-error|run#11|AssertionError|assert err["Error"]["Code"] == _ERR_CANCELED, f"expected {_ERR_CANCELED}, got {err['Error']!r}"
Issue-Key: dynamodb.transact-write-items-update-runtime-arithmetic-error-reports-validation-error-reason|run#13|AssertionError|assert err["Error"]["Code"] == _ERR_CANCELED, f"expected {_ERR_CANCELED}, got {err['Error']!r}"
Issue-Key: dynamodb.execute-transaction-update-path-invalid-for-stored-item-cancels-rejected|run#10|AssertionError|assert err['Error']['Code'] == CANCELED, f'expected {CANCELED}, got {err["Error"]!r}'
Issue-Key: dynamodb.update-item-add-delete-on-map-attribute-and-nested-arithmetic-type-mismatch-rejected|run#17|AssertionError|assert message == _TYPE_MISMATCH, (
Issue-Key: dynamodb.update-item-set-arithmetic-wrong-type-source-into-different-target-rejected|expect_error#6|AssertionError|assert got_message == message, f"expected message {message!r}, got {got_message!r}"
Issue-Key: dynamodb.legacy-attribute-updates-add-binary-scalar-rejected|run#10|AssertionError|assert (code, msg) == _ADD_ON_B_REJECTED, (
Issue-Key: dynamodb.nested-placeholder-path-missing-segment-rejected|expect_error#12|AssertionError|raise AssertionError(f"real AWS rejects this call, but it SUCCEEDED and returned {result!r}")
Issue-Key: dynamodb.update-remove-add-delete-through-missing-key-under-an-existing-map-rejected|expect_error#9|AssertionError|raise AssertionError(f"expected {code!r}, but the call succeeded: {result!r}")
Issue-Key: dynamodb.update-remove-add-delete-through-missing-parent-rejected|_error#6|AssertionError|raise AssertionError(f"call unexpectedly SUCCEEDED where an error was required: {resp!r}")
Issue-Key: dynamodb.update-remove-through-missing-nested-parent-rejected|_error#6|AssertionError|raise AssertionError(f"call unexpectedly SUCCEEDED where an error was required: {resp!r}")
Issue-Key: dynamodb.update-through-bracket-index-on-non-list-rejected|_error#6|AssertionError|raise AssertionError(f"call unexpectedly SUCCEEDED where an error was required: {resp!r}")
Issue-Key: dynamodb.update-item-nested-function-operand-rejected|_error#8|AssertionError|raise AssertionError(f"real AWS rejects this call, but it SUCCEEDED and returned {result!r}")
Issue-Key: dynamodb.update-item-add-arithmetic-result-exceeding-precision-rejected|expect_error#9|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb.update-item-set-arithmetic-result-over-38-significant-digits-rejected|_error#9|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb.delete-item-condition-expression-non-distinct-operands-rejected|_error#13|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb.scan-filter-expression-non-distinct-comparator-and-aliased-operands-rejected|_error#8|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: dynamodb.scan-filter-expression-non-distinct-function-nested-path-operands-rejected|_error#7|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: dynamodb.scan-filterexpression-key-path-through-function-or-alias-rejected|_error#14|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb|Query|ValidationException
Issue-Key: dynamodb.scan-filter-size-counts-utf16-code-units-not-bytes|run#9|AssertionError|assert three == {"bmp"}, f"size(s) = 3 selected {sorted(three)!r}, expected ['bmp']"
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# DynamoDB expression evaluation: 10 net-new causes — transaction-internal runtime errors are not cancellations, and Condition/Filter/Update expression validation accepts or refuses the wrong calls

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TransactWriteItems

## Description
Ten net-new root causes in the DynamoDB expression engine (UpdateExpression evaluation, ConditionExpression/FilterExpression validation). All are grounded in failing behavioral checks listed under '### dynamodb'. Related open issues that do NOT cover these checks: 20261115T010844Z-dynamodb-9-net-new-causes-updateitem-s-over.md, 20261111T170613Z-dynamodb-filter-projection-update-expression-validation-accepts-malformed.md, 20261108T092824Z-dynamodb-5-net-new-causes-transactwriteitems-refuses-placeholders.md.

Cause 1 of 10: a runtime update error inside a transaction is returned as a top-level ValidationException instead of cancelling the transaction.
AWS: Smithy — TransactWriteItems/ExecuteTransaction model TransactionCanceledException (client, HTTP 400) carrying CancellationReasons, a per-item list ordered like TransactItems whose entries carry Code/Message/Item; the shape's own doc lists 'There is a user error, such as an invalid data format' as a cancellation circumstance. An operand-type mismatch against the STORED value, a document path invalid for the stored item, a key-schema mismatch in one slot, or an arithmetic error discovered while applying an update are evaluated per item, so the call answers HTTP 400 TransactionCanceledException with message 'Transaction cancelled, please refer cancellation reasons for specific reasons [...]' and the offending slot's CancellationReasons[i].Code = 'ValidationError' (siblings carry Code 'None'); nothing is written.
Emulator: the call answers HTTP 400 ValidationException with the single-item message for that slot, e.g. {'Code': 'ValidationException', 'Message': 'An operand in the update expression has an incorrect data type'}, {'Code': 'ValidationException', 'Message': 'The document path provided in the update expression is invalid for update'} and {'Code': 'ValidationException', 'Message': 'Invalid UpdateExpression: Incorrect operand type for operator or function; operator or function: +, operand type: S'}. No CancellationReasons list is produced at all, so a sibling slot's reason is also lost.
Why: a client cannot tell WHICH item of the transaction failed or why — the per-item reason list is the documented way to find that, and SDK retry/branching keyed on TransactionCanceledException never fires.
Fix: the TransactWriteItems / ExecuteTransaction handler must run per-item update evaluation inside the transaction's cancellation path and map every runtime item error to a CancellationReason, not to a top-level refusal.
Traces: dynamodb.transact-update-operand-type-mismatch-with-stored-value-cancels-rejected, dynamodb.transact-update-path-invalid-for-stored-item-cancels-rejected, dynamodb.transact-write-items-condition-evaluated-before-update-runtime-validation, dynamodb.transact-write-items-delete-operand-type-and-condition-check-reasons-track-item-identity, dynamodb.transact-write-items-key-schema-mismatch-suppresses-sibling-cancellation-reason, dynamodb.transact-write-items-update-child-of-list-scalar-cancels-with-validation-error, dynamodb.transact-write-items-update-runtime-arithmetic-error-reports-validation-error-reason, dynamodb.execute-transaction-update-path-invalid-for-stored-item-cancels-rejected

Cause 2 of 10: single-item UpdateItem reports a RUNTIME operand-type mismatch with the STATIC-validation wording.
AWS: an operand type that is only discoverable against the stored item (e.g. SET word = word + :one where word holds S, or a nested arithmetic clause over a stored string) is refused with ValidationException and the runtime message 'An operand in the update expression has an incorrect data type' — no '1 validation error detected:' prefix and no operator/operand detail, because the expression parsed cleanly.
Emulator: answers ValidationException '1 validation error detected: Invalid UpdateExpression: Incorrect operand type for operator or function; operator or function: +, operand type: S'. Fixed text and prefix both diverge; the operator/operand values are emulator-authored detail AWS does not emit for this class.
Why: clients and tests that match on AWS's runtime wording to distinguish a malformed expression from a type clash against stored data classify the error wrongly.
Fix: the UpdateExpression evaluator must emit the runtime operand-type message, keeping the '1 validation error detected: Invalid UpdateExpression: ...' form only for defects found during static parsing.
Traces: dynamodb.update-item-add-delete-on-map-attribute-and-nested-arithmetic-type-mismatch-rejected, dynamodb.update-item-set-arithmetic-wrong-type-source-into-different-target-rejected

Cause 3 of 10: legacy AttributeUpdates ADD onto a Binary attribute gives the runtime message instead of the parameter-value message.
AWS: ADD is defined only for Number and set types; an ADD action whose target attribute is type B is refused with ValidationException '1 validation error detected: One or more parameter values were invalid: ADD action is not supported for the type B' — the fixed text, with the type letter taken from the request's attribute.
Emulator: answers ValidationException 'An operand in the update expression has an incorrect data type'.
Why: the refusal no longer names the unsupported type, so a caller cannot see which AttributeUpdates entry is at fault.
Fix: the legacy AttributeUpdates path's ADD validation.
Traces: dynamodb.legacy-attribute-updates-add-binary-scalar-rejected

Cause 4 of 10: UpdateExpression document paths that do not exist in the stored item are APPLIED instead of refused.
AWS: REMOVE/ADD/DELETE (and SET through a missing parent) addressed at a path whose parent is absent, at a key missing under an existing map, through a missing nested parent, or with a bracket index applied to a non-list attribute, is refused with HTTP 400 ValidationException 'The document path provided in the update expression is invalid for update'; the item is unchanged.
Emulator: the call returns HTTP 200 with an empty body ({}), i.e. it silently succeeds.
Why: a client that relies on the refusal to detect a stale or wrong path writes nothing and is told it succeeded — a silent data-integrity divergence, and the matching ConditionalCheckFailed/ValidationException branch in caller code never runs.
Fix: the UpdateExpression path resolver must refuse an unresolvable path at apply time for every clause kind, not only for the forms already covered.
Traces: dynamodb.nested-placeholder-path-missing-segment-rejected, dynamodb.update-remove-add-delete-through-missing-key-under-an-existing-map-rejected, dynamodb.update-remove-add-delete-through-missing-parent-rejected, dynamodb.update-remove-through-missing-nested-parent-rejected, dynamodb.update-through-bracket-index-on-non-list-rejected

Cause 5 of 10: a nested function operand in an UpdateExpression is accepted.
AWS: refuses the call with HTTP 400 ValidationException; the update does not apply.
Emulator: returns HTTP 200 with an empty body.
Why: an expression AWS rejects as malformed is executed, so the item diverges from what the same code produces against AWS.
Fix: the UpdateExpression parser's operand grammar (function nesting).
Traces: dynamodb.update-item-nested-function-operand-rejected

Cause 6 of 10: an arithmetic result wider than DynamoDB's number precision is stored instead of refused.
AWS: a Number has at most 38 significant digits; a SET/ADD whose computed result exceeds that is refused with HTTP 400 ValidationException and the item is unchanged.
Emulator: returns HTTP 200 with an empty body and keeps the over-precision result.
Why: the stored value cannot round-trip through AWS, so a later read against real AWS differs and arithmetic silently loses the overflow signal.
Fix: the UpdateExpression arithmetic evaluator's result-precision check.
Traces: dynamodb.update-item-add-arithmetic-result-exceeding-precision-rejected, dynamodb.update-item-set-arithmetic-result-over-38-significant-digits-rejected

Cause 7 of 10: ConditionExpression/FilterExpression operands that are not distinct are accepted.
AWS: for a comparator or function whose first operand must differ from the rest (=, <>, >, begins_with, attribute_type), naming the same path on both sides is refused with HTTP 400 ValidationException. The message's fixed text is 'Invalid <ExpressionName>: The first operand must be distinct from the remaining operands for this operator or function; operator: <op>, first operand: [<path segments>]' — the operand is reported as its resolved document path (aliases resolved to attribute names, a nested path as comma-separated segments), and on ConditionExpression the message additionally carries the '1 validation error detected: ' prefix.
Emulator: the call succeeds — DeleteItem returns HTTP 200 and Scan returns HTTP 200 with Items/Count.
Why: a programming error AWS surfaces at once is evaluated as a predicate, so the call returns a different result set (or deletes an item) than against AWS.
Fix: the ConditionExpression/FilterExpression static validator must add the distinct-operand rule for comparators and for begins_with/attribute_type.
Traces: dynamodb.delete-item-condition-expression-non-distinct-operands-rejected, dynamodb.scan-filter-expression-non-distinct-comparator-and-aliased-operands-rejected, dynamodb.scan-filter-expression-non-distinct-function-nested-path-operands-rejected

Cause 8 of 10: a Scan FilterExpression naming a key path through a function or an alias is accepted.
AWS: refuses the call with HTTP 400 ValidationException; no items are returned.
Emulator: returns HTTP 200 with Count/ScannedCount.
Why: the caller gets a filtered page where AWS gives a refusal.
Fix: the FilterExpression validator's key-attribute rule must resolve aliases and function arguments before deciding.
Traces: dynamodb.scan-filterexpression-key-path-through-function-or-alias-rejected

Cause 9 of 10: a Query on a secondary index REFUSES a FilterExpression on base-table key attributes that AWS allows.
AWS: FilterExpression may not reference the key attributes OF THE INDEX BEING QUERIED, but a base-table key attribute that is not an index key is a non-key attribute for that query and is allowed; the Query returns the narrowed items.
Emulator: answers HTTP 400 ValidationException 'Filter Expression can only contain non-primary key attributes: Primary key attribute: sk' — it applies the base table's key schema instead of the queried index's, so a legal filter is refused (and the symmetric case, a filter on the index's own key attributes, is refused naming the wrong attribute).
Why: the common 'query the GSI, narrow by the base-table sort key' pattern fails outright against the emulator.
Fix: the Query handler's filter validation must take the key set from the index named by IndexName.
Traces: dynamodb.gsi-query-filter-on-base-table-pk-and-sk-narrows-to-one-item, dynamodb.gsi-query-filter-on-its-own-key-attributes-rejected, dynamodb.query-gsi-and-lsi-filter-on-base-table-sort-key-accepted

Cause 10 of 10: the size() function in a FilterExpression measures the wrong unit for strings.
AWS: size() on a String returns the number of UTF-16 code units, so a 3-code-unit BMP string matches size(s) = 3.
Emulator: the same comparison selects no items, i.e. size() is computed over another unit (UTF-8 bytes).
Why: every size() predicate over non-ASCII strings selects a different set than against AWS.
Fix: the expression engine's size() implementation for S (and the same unit rule wherever string length is compared).
Traces: dynamodb.scan-filter-size-counts-utf16-code-units-not-bytes

## Evidence
From the behavioral replay, per cause:
- Transactions: expected TransactionCanceledException, got {'Code': 'ValidationException', 'Message': 'An operand in the update expression has an incorrect data type'} / '... The document path provided in the update expression is invalid for update' / '... Invalid UpdateExpression: Incorrect operand type for operator or function; operator or function: +, operand type: S'.
- UpdateItem runtime wording: expected 'An operand in the update expression has an incorrect data type', got '1 validation error detected: Invalid UpdateExpression: Incorrect operand type for operator or function; operator or function: +, operand type: S'.
- Legacy ADD on B: expected ('ValidationException', '1 validation error detected: One or more parameter values were invalid: ADD action is not supported for the type B'), got ('ValidationException', 'An operand in the update expression has an incorrect data type').
- Invalid paths / nested function operand / over-precision arithmetic: 'call unexpectedly SUCCEEDED where an error was required: {... 'HTTPStatusCode': 200 ...}' (empty body, content-length 2).
- Non-distinct operands: 'expected the call to be rejected, but it succeeded: {'Items': [...], 'Count': 1, 'ScannedCount': 1 ...}'. Expected messages from the checks: "1 validation error detected: Invalid ConditionExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: =, first operand: [score]" and "Invalid FilterExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: begins_with, first operand: [info, label]".
- Index Query filter: botocore ClientError (ValidationException) 'Filter Expression can only contain non-primary key attributes: Primary key attribute: sk' where AWS returns the narrowed items.
- size(): 'size(s) = 3 selected [], expected ['bmp']'.

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the check ids in trace_ids; each reproduces one cause.
3. Representative manual repro: create a PAY_PER_REQUEST table with pk (S); PutItem {pk: 'doc', s: 'text'}; call TransactWriteItems with one Update 'ADD s :one' and one well-formed sibling Update — AWS answers TransactionCanceledException with CancellationReasons [{'Code':'ValidationError',...},{'Code':'None'}], the emulator answers a top-level ValidationException.
4. Call UpdateItem with 'REMOVE a.b.c' where attribute a is absent — AWS refuses with ValidationException 'The document path provided in the update expression is invalid for update', the emulator returns HTTP 200.
5. Call Scan with FilterExpression 'qty <> qty' — AWS refuses with ValidationException, the emulator returns Items.
6. Create a table with a GSI and Query the GSI with FilterExpression on the base table's sort key — AWS returns the items, the emulator refuses.
