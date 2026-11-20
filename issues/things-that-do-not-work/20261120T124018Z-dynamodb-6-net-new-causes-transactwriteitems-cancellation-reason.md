Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.transact-write-items-condition-evaluated-before-update-runtime-validation dynamodb.transact-write-items-key-schema-mismatch-suppresses-sibling-cancellation-reason dynamodb.transact-write-items-update-grows-item-over-size-limit dynamodb.transact-write-items-update-over-size-limit-new-and-existing-item-both-regions dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key dynamodb.zero-with-out-of-range-exponent-rejected dynamodb.execute-statement-rejects-out-of-range-n-parameter dynamodb.batch-get-item-expression-and-legacy-parameter-rules dynamodb.null-false-batchget-mixed-projection-and-query-keyconditions-with-expressions
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.transact-write-items-condition-evaluated-before-update-runtime-validation|run#9|AssertionError|assert err["Error"].get("Message") == expected_message, (
Issue-Key: dynamodb.transact-write-items-key-schema-mismatch-suppresses-sibling-cancellation-reason|run#24|AssertionError|assert reasons[0] == {"Code": "None"}, (
Issue-Key: dynamodb.transact-write-items-update-grows-item-over-size-limit|run#14|AssertionError|assert code == ERR_VALIDATION, (
Issue-Key: dynamodb|TransactWriteItems|TransactionCanceledException
Issue-Key: dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key|expect_error#31|AssertionError|assert got_code == code, f"expected {code!r}, got {got_code!r} ({got_message!r})"
Issue-Key: dynamodb.zero-with-out-of-range-exponent-rejected|run#24|AssertionError|assert message == f'1 validation error detected: The parameter cannot be converted to a numeric value: {past_max_exp}', (
Issue-Key: dynamodb.execute-statement-rejects-out-of-range-n-parameter|run#7|AssertionError|assert message == expected, (
Issue-Key: dynamodb.batch-get-item-expression-and-legacy-parameter-rules|_error#8|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb.null-false-batchget-mixed-projection-and-query-keyconditions-with-expressions|_error#16|AssertionError|raise AssertionError(f'expected the call to be refused, but it succeeded: {result!r}')
Issue-Commit: 0139b649892491ec475f7b8e31445a257f779000

# dynamodb: 6 net-new causes — TransactWriteItems cancellation-reason ordering/suppression, over-size Update is cancelled instead of refused, unparsable number exponents get the overflow message, and BatchGetItem never enforces expression/legacy exclusivity

> _Note: this issue was identified against the code base at commit `0139b649892491ec475f7b8e31445a257f779000`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TransactWriteItems

## Description
Six net-new behavioral root causes in DynamoDB's transaction, number-parsing and BatchGetItem validation surfaces. Each cause below is independent and must be closed separately.

Cause 1 of 6: a TransactWriteItems Update evaluates the UpdateExpression's runtime validation BEFORE its ConditionExpression.
AWS: an Update member whose ConditionExpression is false and whose UpdateExpression would also fail at runtime (e.g. `SET n = n + :one` where the stored `n` is a String) reports only the condition failure: TransactionCanceledException, HTTP 400, message 'Transaction cancelled, please refer cancellation reasons for specific reasons [<per-slot codes>]' — fixed text whose variable part is the comma-separated list of per-slot codes, here '[ConditionalCheckFailed]' — with CancellationReasons[0] = {Code: 'ConditionalCheckFailed', Message: 'The conditional request failed'}. Only when the condition passes is the arithmetic reached and reported as that slot's own {Code: 'ValidationError', Message: 'An operand in the update expression has an incorrect data type'}. Smithy: TransactWriteItems errors include TransactionCanceledException (httpError 400) carrying CancellationReasons; CancellationReason.Code/Message are modeled, the texts are recorded behavior only.
Emulator: the false-condition leg answers 'Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]' — the runtime type error wins over the unmet condition, so the per-slot code and the bracketed code list in the message are both wrong.
Why: a client that branches on ConditionalCheckFailed (retry/refresh the item, and read ReturnValuesOnConditionCheckFailure=ALL_OLD) instead sees a non-retryable ValidationError and treats an ordinary optimistic-concurrency conflict as a bug in its own request.
Fix: the per-member evaluation order in the TransactWriteItems handler — evaluate ConditionExpression first and short-circuit the member's update-expression runtime validation when it fails.
Traces: dynamodb.transact-write-items-condition-evaluated-before-update-runtime-validation.

Cause 2 of 6: a key-schema ValidationError in one slot does not suppress its siblings' cancellation reasons.
AWS: when any slot carries a key that does not match the table's key schema, that slot reports {Code: 'ValidationError', Message: 'The provided key element does not match the schema'} and EVERY other slot reports {Code: 'None'} with no Message — regardless of slot order and even when a sibling would itself have failed its own ConditionCheck or update. Smithy: CancellationReason documents 'If no error occurred for the associated item an error with a Null code and Null message will be present'; the suppression of an otherwise-failing sibling is recorded behavior only.
Emulator: with [ConditionCheck that fails, Delete with a wrong-typed key], reasons[0] is {'Code': 'ConditionalCheckFailed', 'Message': 'The conditional request failed'} instead of {'Code': 'None'} — the emulator grades each slot independently rather than letting the key-schema defect blank the siblings.
Why: a client reading CancellationReasons to decide what to fix is told its condition failed when the only real defect is the malformed key, and so retries a request that can never succeed.
Fix: the cancellation-reason assembly in the TransactWriteItems handler — when a key-schema validation defect is present, emit that slot's reason and force every sibling to Code 'None'.
Traces: dynamodb.transact-write-items-key-schema-mismatch-suppresses-sibling-cancellation-reason.

Cause 3 of 6: a TransactWriteItems Update that would grow an item past the item-size limit is cancelled instead of refused at the top level.
AWS: an Update member whose result would exceed the 409,600-byte item size limit is refused as a TOP-LEVEL ValidationException, HTTP 400, message 'Item size to update has exceeded the maximum allowed size' (fixed text, no variable part), never a TransactionCanceledException, and the stored item is left unchanged. This holds for both a new and an existing target item, and in every Region. Smithy: TransactWriteItems models TransactionCanceledException and the exception's docs mention item-size growth as a cancel circumstance, but the recordings show AWS answering the Update-growth case as a top-level ValidationException — recorded behavior for the status/code/message.
Emulator: every such request answers 'TransactionCanceledException': 'Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]' — the wrong error code, the wrong message, and the size defect reported as a per-slot cancellation rather than a request-level refusal.
Why: a client that distinguishes a request it must shrink (ValidationException) from a transaction it may retry (TransactionCanceledException) retries forever on an unshrinkable write.
Fix: the size validation in the TransactWriteItems Update path — perform the projected-item-size check during request validation, before the transaction is staged, and raise ValidationException.
Traces: dynamodb.transact-write-items-update-grows-item-over-size-limit, dynamodb.transact-write-items-update-over-size-limit-new-and-existing-item-both-regions, dynamodb.transact-write-items-update-shares-update-items-floor-bound-ceiling-at-a-short-key.

Cause 4 of 6: a number whose EXPONENT cannot be parsed is reported as a magnitude overflow.
AWS: DynamoDB distinguishes a number that parses but is out of range from one whose exponent cannot be parsed at all. An exponent that still parses as a 64-bit integer (e.g. 1E9223372036854775807) is range-checked and refused with ValidationException '1 validation error detected: Number overflow. Attempting to store a number with magnitude larger than supported range'. An exponent one past that (e.g. 1E9223372036854775808) cannot be parsed and is refused with ValidationException '1 validation error detected: The parameter cannot be converted to a numeric value: <the numeric literal exactly as sent>' — fixed text plus the request's literal. On the PartiQL parameter path the same class is reported without the '1 validation error detected: ' prefix: 'The parameter cannot be converted to a numeric value: <literal>'. Smithy: ExecuteStatement.Parameters targets AttributeValue; the number range (1E-130 … 9.9999999999999999999999999999999999999E+125) and these texts are documented/recorded behavior.
Emulator: both paths collapse to the overflow message — PutItem with v={'N':'1E9223372036854775808'} answers '1 validation error detected: Number overflow. Attempting to store a number with magnitude larger than supported range', and ExecuteStatement with the N parameter '1E99999999999999999999999999999999999999999' answers 'Number overflow. Attempting to store a number with magnitude larger than supported range' where AWS answers 'The parameter cannot be converted to a numeric value: 1E99999999999999999999999999999999999999999'.
Why: a client that classifies rejections by message cannot tell a value it can rescale (true magnitude overflow) from a literal it must re-serialize, and error-text assertions in SDK/test suites fail.
Fix: the number parser shared by the AttributeValue N validator and the PartiQL parameter binder — classify an exponent that fails to parse as 'cannot be converted to a numeric value' (quoting the literal) before any range check.
Traces: dynamodb.zero-with-out-of-range-exponent-rejected, dynamodb.execute-statement-rejects-out-of-range-n-parameter.

Cause 5 of 6: BatchGetItem accepts expression and non-expression parameters mixed inside one table's KeysAndAttributes entry.
AWS: a KeysAndAttributes entry carrying both ProjectionExpression and AttributesToGet is refused with ValidationException, HTTP 400, message 'Can not use both expression and non-expression parameters in the same request: Non-expression parameters: {AttributesToGet} Expression parameters: {ProjectionExpression}' — fixed text whose variable parts are the colliding parameter names inside the braces. Smithy: KeysAndAttributes carries both AttributesToGet ('This is a legacy parameter. Use ProjectionExpression instead') and ProjectionExpression; the exclusivity rule and its text are documented/recorded behavior.
Emulator: the request SUCCEEDS with HTTP 200 and returns the whole item (Responses carried pk, a and b for an entry whose ProjectionExpression was 'a'), so neither the exclusivity rule nor the projection itself is applied.
Why: a client that accidentally sends both parameters gets unprojected items instead of a refusal, silently reading and paying for attributes it asked to exclude.
Fix: the BatchGetItem request validation — reject an entry mixing legacy and expression parameters before any read.
Traces: dynamodb.batch-get-item-expression-and-legacy-parameter-rules.

Cause 6 of 6: BatchGetItem does not refuse request-wide when the mix spans two different tables' entries.
AWS: the exclusivity is evaluated across the whole request, not per entry: RequestItems where one table's entry uses ProjectionExpression and ANOTHER table's entry uses AttributesToGet is refused with ValidationException and the same message 'Can not use both expression and non-expression parameters in the same request: Non-expression parameters: {AttributesToGet} Expression parameters: {ProjectionExpression}'. Expression parameters on every entry, or legacy parameters on every entry, are accepted. Recorded behavior for the request-wide scope; Smithy models only the members.
Emulator: the mixed two-table request SUCCEEDS with HTTP 200, answering both entries' projections ({t1: [{'a': …}], t2: [{'c': …}]}), so the request-wide rule is absent.
Why: a client migrating off the legacy parameters table-by-table is told its half-migrated request is fine on the emulator and is refused by AWS on the first real call.
Fix: the BatchGetItem request validation — scope the legacy/expression exclusivity check to the whole RequestItems map.
Traces: dynamodb.null-false-batchget-mixed-projection-and-query-keyconditions-with-expressions (its step 2; the NULL:false leg of that check passes).

## Evidence
Cause 1 — Expected message 'Transaction cancelled, please refer cancellation reasons for specific reasons [ConditionalCheckFailed]', got 'Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]'.
Cause 2 — case 1 ([failing ConditionCheck, Delete with wrong-typed key]): expected reasons[0] == {'Code': 'None'}, got {'Code': 'ConditionalCheckFailed', 'Message': 'The conditional request failed'}.
Cause 3 — Expected ('ValidationException', 'Item size to update has exceeded the maximum allowed size'); got 'TransactionCanceledException': 'Transaction cancelled, please refer cancellation reasons for specific reasons [ValidationError]' on all three checks (new item, existing item, both Regions, and the short-key ceiling case).
Cause 4 — PutItem v={'N':'1E9223372036854775808'}: expected '1 validation error detected: The parameter cannot be converted to a numeric value: 1E9223372036854775808', got '1 validation error detected: Number overflow. Attempting to store a number with magnitude larger than supported range'. ExecuteStatement N parameter '1E99999999999999999999999999999999999999999': expected 'The parameter cannot be converted to a numeric value: 1E99999999999999999999999999999999999999999', got 'Number overflow. Attempting to store a number with magnitude larger than supported range'. The in-range-exponent overflow/underflow legs of the same checks pass, which is what isolates the parse-vs-range classification.
Cause 5 — BatchGetItem RequestItems {t1: {'Keys': [{'pk':{'S':'k1'}}], 'ProjectionExpression': 'a', 'AttributesToGet': ['a']}} expected ValidationException; got HTTP 200 {'Responses': {t1: [{'b': {'S': 'B'}}, … full item 'b','a','pk']}, 'UnprocessedKeys': {}}.
Cause 6 — BatchGetItem RequestItems {t1: {'Keys': [...], 'ProjectionExpression': 'a'}, t2: {'Keys': [...], 'AttributesToGet': ['c']}} expected ValidationException; got HTTP 200 {'Responses': {t1: [{'a': {'S': 'A'}}], t2: [{'c': {'S': 'C'}}]}, 'UnprocessedKeys': {}}.

## Steps to Reproduce
Cause 1: create a table (pk S), PutItem {pk:'counter', n:{S:'not-a-number'}, status:{S:'ON'}}, then TransactWriteItems with one Update on that key, UpdateExpression 'SET n = n + :one', ConditionExpression '#s = :expected' with :expected 'OFF'; read Error.Message and CancellationReasons. Repeat with :expected 'ON' for the ValidationError leg.
Cause 2: same table, PutItem {pk:'typed', v:{S:'not-a-number'}}; TransactWriteItems [ConditionCheck on an absent key with attribute_exists(pk), Delete with Key {pk:{N:'1'}}]; read CancellationReasons (and the reverse order, and with an ADD-type-error Update in place of the ConditionCheck).
Cause 3: store an item near 409,600 bytes and issue TransactWriteItems with an Update that grows it past the limit (and one whose Update creates an oversized new item); read Error.Code/Message.
Cause 4: PutItem with v={'N':'1E9223372036854775807'} (overflow leg, passes) and v={'N':'1E9223372036854775808'}; ExecuteStatement 'SELECT … WHERE v = ?' with Parameters [{'N':'1E99999999999999999999999999999999999999999'}]; compare Error.Message.
Cause 5: BatchGetItem with one table entry carrying both ProjectionExpression and AttributesToGet.
Cause 6: BatchGetItem with table A's entry using ProjectionExpression and table B's entry using AttributesToGet.
Replay the eight check ids in trace_ids.
