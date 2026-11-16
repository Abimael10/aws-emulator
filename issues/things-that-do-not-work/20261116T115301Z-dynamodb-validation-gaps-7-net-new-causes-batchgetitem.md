Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.batch-get-item-expression-and-legacy-parameter-rules dynamodb.ddb-query-size-function-rejected-in-key-condition dynamodb.expression-attribute-map-key-syntax-and-length-rejected dynamodb.multiple-simultaneous-constraint-violations-rejected dynamodb.nesting-depth-error-precedence-across-malformed-requests dynamodb.transact-write-aggregate-size-over-4mb-rejected dynamodb.update-item-request-shape-byte-charges-by-clause-kind dynamodb.update-item-set-remove-fixed-action-cost-ceiling
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.batch-get-item-expression-and-legacy-parameter-rules|_error#8|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb|Query|ValidationException
Issue-Key: dynamodb.expression-attribute-map-key-syntax-and-length-rejected|run#14|AssertionError|assert (code, msg) == ("ValidationException", want), f"{label} gave {code!r}: {msg!r}"
Issue-Key: dynamodb.multiple-simultaneous-constraint-violations-rejected|run#12|AssertionError|assert message == (
Issue-Key: dynamodb.nesting-depth-error-precedence-across-malformed-requests|run#24|AssertionError|assert message == MSG_DEPTH, f'd-combo: expected the nesting-depth message, got {message!r}'
Issue-Key: dynamodb.transact-write-aggregate-size-over-4mb-rejected|run#11|AssertionError|assert (code, msg) == ("ValidationException", expected), (
Issue-Key: dynamodb.update-item-request-shape-byte-charges-by-clause-kind|run#7|AssertionError|assert delta == expected_delta, (
Issue-Key: dynamodb.update-item-set-remove-fixed-action-cost-ceiling|_measure#6|AssertionError|assert second_set_cost == 19, (
Issue-Commit: d68524ddfe39deacc36a1df634022e0b97d0c530

# DynamoDB validation gaps: 7 net-new causes — BatchGetItem legacy/expression parameter rules unenforced, wrong KeyConditionExpression nested-operation text, missing expression-attribute key length check, single-error instead of aggregated constraint reports, nesting-depth precedence, TransactWriteItems 4MB message, and UpdateItem per-clause byte charges

> _Note: this issue was identified against the code base at commit `d68524ddfe39deacc36a1df634022e0b97d0c530`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateItem

## Description
Seven net-new root causes in the DynamoDB request-validation and size-accounting layers. None is covered by an open issue; the open issues 20261111T170613Z-dynamodb-item-size-batch-size-and-request-size.md, 20261111T170613Z-dynamodb-non-constraint-validationexception-messages-are-wrapped-in.md and 20261111T170613Z-dynamodb-filter-projection-update-expression-validation-accepts-malformed.md are adjacent but carry none of the replay keys below.

Cause 1 of 7 — BatchGetItem does not enforce the per-table expression/legacy parameter rules.
AWS: Smithy: BatchGetItem input RequestItems -> BatchGetRequestMap value KeysAndAttributes, whose members are Keys (required), the legacy AttributesToGet, ConsistentRead, ProjectionExpression and ExpressionAttributeNames; AttributesToGet is documented as 'This is a legacy parameter. Use ProjectionExpression instead.' AWS refuses the mixtures with 400 ValidationException and these messages (fixed text, with the member names and the offending placeholder keys supplied from the request): 'Can not use both expression and non-expression parameters in the same request: Non-expression parameters: {AttributesToGet} Expression parameters: {ProjectionExpression}'; 'ExpressionAttributeNames can only be specified when using expressions'; 'Value provided in ExpressionAttributeNames unused in expressions: keys: {#n}'. Legacy-only requests and a projection that uses its only name succeed.
Emulator: the first case (one table carrying both ProjectionExpression and AttributesToGet) answers HTTP 200 — and worse, returns the WHOLE item ({'b','pk','a'}) rather than the projected attribute, so the projection is also ignored.
Why: a client whose request is malformed in a way AWS rejects gets a success plus unprojected attributes; code that relies on the refusal to catch a legacy/expression mix-up never sees it, and reads are billed/sized against more attributes than requested.
Fix: BatchGetItem's handler/validation for each KeysAndAttributes entry (expression-vs-legacy conflict, ExpressionAttributeNames without a projection — which must be refused even when the offending table is beside another table using AttributesToGet — and unused name keys), plus the projection application itself.
Traces: dynamodb.batch-get-item-expression-and-legacy-parameter-rules.

Cause 2 of 7 — a function call in a KeyConditionExpression is reported as a parser syntax error instead of the nested-operation refusal.
AWS: recorded behavior only for the text (the model types KeyConditionExpression as a string). AWS refuses Query with KeyConditionExpression 'size(pk) = :n' with 400 ValidationException and exactly 'Invalid KeyConditionExpression: KeyConditionExpressions cannot contain nested operations' — fixed text, no variable part. The same size() shape in a FilterExpression on the same Query still selects the item.
Emulator: 400 ValidationException with 'Invalid KeyConditionExpression: Syntax error; token: "(", near: "size ( pk"' — the generic parser message, whose token/near fragments are emulator-authored.
Why: clients and test suites that match on AWS's refusal text (and support tooling that classifies key-condition misuse) see an unrecognised message for a documented refusal class.
Fix: the Query KeyConditionExpression validator — recognise a function call/nested operation in a key condition before the generic syntax path and emit AWS's nested-operations message.
Traces: dynamodb.ddb-query-size-function-rejected-in-key-condition.

Cause 3 of 7 — the expression-attribute map KEY length limit is not enforced, so an over-long placeholder falls through to an 'undefined name' error.
AWS: Smithy: ExpressionAttributeNameMap keys are ExpressionAttributeNameVariable, ExpressionAttributeValueMap keys are ExpressionAttributeValueVariable; the documented placeholder limit is 255 bytes. A key longer than that is 400 ValidationException with 'ExpressionAttributeNames contains invalid key: The expression attribute map contains a key that is too long; size of key: <byte size of the offending key>' (and the ExpressionAttributeValues wording for value keys) — fixed text plus the measured key size from the request. This refusal takes precedence over any complaint about the expression not defining the name actually used.
Emulator: GetItem with ExpressionAttributeNames {'#' + 'a'*255: 'foo'} and ProjectionExpression '#a' answers 400 ValidationException 'Invalid ProjectionExpression: An expression attribute name used in the document path is not defined; attribute name: #a' — the length check never runs, so the wrong defect is reported.
Why: a client that over-ran the 255-byte placeholder limit is told its expression is wrong instead, and cannot distinguish a too-long key from a genuinely undefined placeholder.
Fix: the shared expression-attribute map validation (ExpressionAttributeNames/ExpressionAttributeValues key syntax and length), ordered ahead of expression name/value resolution, for GetItem, Scan and the other expression-bearing operations.
Traces: dynamodb.expression-attribute-map-key-syntax-and-length-rejected.

Cause 4 of 7 — simultaneous constraint violations are reported one at a time instead of aggregated into one message.
AWS: recorded behavior only for the aggregation text; the constraints themselves are modelled (TableName @length min 1, ReturnConsumedCapacity/ReturnValues enums, Key required). AWS reports every independent shape-constraint violation of one request in a single 400 ValidationException, counted and in a fixed order: '<N> validation errors detected: ' followed by '; '-joined 'Value '<value>' at '<memberPath>' failed to satisfy constraint: <rule>' clauses (e.g. four clauses for an UpdateItem with TableName '', ReturnConsumedCapacity 'BOGUS', ReturnValues 'BOGUS2' and no Key; two clauses naming 'globalSecondaryIndexes.1.member.indexName' and 'localSecondaryIndexes.1.member.indexName' for CreateTable with 2-character index names). The count, the member paths and the offending values come from the request; the sentence frames are fixed.
Emulator: answers '1 validation error detected: Value null at 'key' failed to satisfy constraint: Member must not be null' — only one of the four violations, so the count and the whole clause list diverge.
Why: a client fixing a malformed request is shown one defect per round trip instead of all of them, and any assertion on AWS's '<N> validation errors detected' envelope fails.
Fix: the shared request shape/constraint validator must collect all violations (scalar members and list element paths alike) and render them in AWS's counted, ordered, semicolon-joined envelope.
Traces: dynamodb.multiple-simultaneous-constraint-violations-rejected.

Cause 5 of 7 — the nesting-depth refusal does not take precedence over an unused ExpressionAttributeValues key.
AWS: recorded behavior only (the limit is documented as 32 nesting levels). When an UpdateItem carries BOTH a value nested past the limit and an unused expression-attribute value, AWS reports the depth error: '1 validation error detected: Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit' (fixed text). Only when the document is shallow does the unused-value error surface: '1 validation error detected: Value provided in ExpressionAttributeValues unused in expressions: keys: {<the unused keys>}'. Either way nothing is written.
Emulator: for the deep + unused-value combination it answers the unused-value message, i.e. it checks unused expression attributes before item nesting depth. (The sibling precedence cases — bad ReturnValues, empty attribute name, and a ConditionExpression syntax error beside the deep document — do match.)
Why: a client with an over-nested document is pointed at an unrelated placeholder, and the error a retry loop keys on changes depending on which check ran first.
Fix: the UpdateItem/PutItem validation ordering — run attribute nesting-depth validation ahead of the unused-ExpressionAttributeValues check.
Traces: dynamodb.nesting-depth-error-precedence-across-malformed-requests.

Cause 6 of 7 — the TransactWriteItems over-4MB refusal uses emulator-authored text and omits the payload size.
AWS: recorded behavior only for the text; the 4 MB aggregate transaction limit is documented. A TransactWriteItems whose items are each under 400 KB but exceed 4 MB in aggregate is refused with 400 ValidationException and 'Transaction payload size cannot exceed 4MB. Payload Size: <aggregate payload size in bytes>' — fixed text plus the size computed from the request — with no CancellationReasons, and none of the items written; a two-item subset of the same items then commits.
Emulator: 400 ValidationException 'Transaction size has exceeded the maximum allowed size' — different wording and no payload size, so the client cannot see by how much it over-ran.
Why: callers that size or split transactions from the reported payload size get no number, and message-matching clients misclassify the refusal.
Fix: the TransactWriteItems handler's aggregate-size validation and its error message construction.
Traces: dynamodb.transact-write-aggregate-size-over-4mb-rejected.

Cause 7 of 7 — UpdateItem's item-size ceiling charges only the resulting item's stored bytes, not each clause's own request shape.
AWS: recorded behavior only (documented 400 KB item limit). AWS's UpdateItem size accounting adds a fixed per-clause/per-action charge on top of the stored bytes, so the largest accepted operand shrinks relative to a plain 'SET v = :v' baseline by a fixed, region-independent amount per clause kind: document path +26, arithmetic +71, function (if_not_exists) +76, ADD +21, DELETE +2 bytes; a second SET clause costs exactly 19 fixed bytes; a REMOVE beside a SET costs exactly 1 byte plus the removed path's length, whether or not the path holds a value. Crossing the ceiling is 400 ValidationException 'Item size to update has exceeded the maximum allowed size' (fixed text) and leaves the attribute at its last accepted length.
Emulator: the boundary does not move with the clause shape — a document-path clause shows 7 bytes of charge where AWS shows 26, and a second SET clause shows 0 bytes of fixed overhead where AWS shows 19 (measured at n_set1=409590 vs n_set2=409586). So writes AWS refuses are accepted near the limit.
Why: an item that AWS refuses as too large is stored by the emulator, so a client validated against the emulator hits ValidationException only in production; capacity/size-sensitive code also computes different headroom.
Fix: the UpdateItem size-limit accounting in the update-expression evaluation/validation path — charge each SET/REMOVE/ADD/DELETE clause's own request shape (fixed per-action overhead, document paths, arithmetic, functions, removed path length) in addition to the finished item's bytes.
Traces: dynamodb.update-item-request-shape-byte-charges-by-clause-kind, dynamodb.update-item-set-remove-fixed-action-cost-ceiling.

## Evidence
Cause 1: expected 400 ValidationException 'Can not use both expression and non-expression parameters in the same request: Non-expression parameters: {AttributesToGet} Expression parameters: {ProjectionExpression}'; actual HTTP 200 with {'Responses': {<table>: [{'b': {'S': 'B'}, 'pk': {'S': 'k1'}, 'a': {'S': 'A'}}]}, 'UnprocessedKeys': {}}.
Cause 2: expected ('ValidationException', 'Invalid KeyConditionExpression: KeyConditionExpressions cannot contain nested operations'); actual ('ValidationException', 'Invalid KeyConditionExpression: Syntax error; token: "(", near: "size ( pk"').
Cause 3: expected ('ValidationException', 'ExpressionAttributeNames contains invalid key: The expression attribute map contains a key that is too long; size of key: 256'); actual ('ValidationException', 'Invalid ProjectionExpression: An expression attribute name used in the document path is not defined; attribute name: #a').
Cause 4: expected "4 validation errors detected: Value 'BOGUS' at 'returnConsumedCapacity' failed to satisfy constraint: Member must satisfy enum value set: [INDEXES, TOTAL, NONE]; Value '' at 'tableName' failed to satisfy constraint: Member must have length greater than or equal to 1; Value 'BOGUS2' at 'returnValues' failed to satisfy constraint: Member must satisfy enum value set: [ALL_NEW, UPDATED_OLD, ALL_OLD, NONE, UPDATED_NEW]; Value null at 'key' failed to satisfy constraint: Member must not be null"; actual "1 validation error detected: Value null at 'key' failed to satisfy constraint: Member must not be null".
Cause 5: expected '1 validation error detected: Nesting Levels have exceeded supported limits: Attributes in the item have nested levels beyond supported limit'; actual '1 validation error detected: Value provided in ExpressionAttributeValues unused in expressions: keys: {:unused}'.
Cause 6: expected ('ValidationException', 'Transaction payload size cannot exceed 4MB. Payload Size: <aggregate bytes>'); actual ('ValidationException', 'Transaction size has exceeded the maximum allowed size') for a 4,393,092-byte transaction.
Cause 7: expected a document-path clause to cost 26 bytes of request-shape charge relative to a plain SET and a second SET clause to cost 19 fixed bytes; measured 7 and 0 respectively (n_set1=409590, n_set2=409586), identically in us-west-2 and us-east-1.

## Steps to Reproduce
Cause 1: create two on-demand tables keyed pk (S), put an item in each, then BatchGetItem with RequestItems {t1: {Keys:[{pk:{S:'k1'}}], ProjectionExpression:'a', AttributesToGet:['a']}} — observe 200 with all attributes instead of the ValidationException; then the ExpressionAttributeNames-without-projection and unused-name variants.
Cause 2: Query a pk/sk table with KeyConditionExpression 'size(pk) = :n', ExpressionAttributeValues {':n': {N:'5'}} — compare the refusal message.
Cause 3: GetItem with ProjectionExpression '#a' and ExpressionAttributeNames {('#' + 'a'*255): 'foo'} — compare the refusal message; likewise a 256-byte ':'-prefixed key in a Scan's ExpressionAttributeValues.
Cause 4: with client-side parameter validation disabled, call UpdateItem(TableName='', ReturnConsumedCapacity='BOGUS', ReturnValues='BOGUS2') with no Key — compare the aggregated message; likewise CreateTable with 2-character GSI and LSI names.
Cause 5: UpdateItem 'SET doc = :v' with :v nested 40 levels deep plus an extra unused ':unused' value — compare which error is reported.
Cause 6: TransactWriteItems with 11 Puts of ~390 KB each (>4 MB aggregate) — compare the message and assert no CancellationReasons and no items written.
Cause 7: bisect the largest accepted operand length for 'SET v = :v' and for 'SET v1 = :v1, v2 = :v2' / 'SET v = :v, m.x = :mval' / 'SET v = :v REMOVE w' on a 400 KB-limited table and compare the per-clause deltas.
Replay: dynamodb.batch-get-item-expression-and-legacy-parameter-rules, dynamodb.ddb-query-size-function-rejected-in-key-condition, dynamodb.expression-attribute-map-key-syntax-and-length-rejected, dynamodb.multiple-simultaneous-constraint-violations-rejected, dynamodb.nesting-depth-error-precedence-across-malformed-requests, dynamodb.transact-write-aggregate-size-over-4mb-rejected, dynamodb.update-item-request-shape-byte-charges-by-clause-kind, dynamodb.update-item-set-remove-fixed-action-cost-ceiling
