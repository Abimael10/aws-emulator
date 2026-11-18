Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.tag-resource-aws-prefixed-key-rejected-and-its-companion-key-not-applied dynamodb.query-scan-exclusive-start-key-empty-value-rejected dynamodb.partiql-statement-index-key-type-mismatch-rejected dynamodb.query-base-table-exclusive-start-key-partition-key-type-mismatch-rejected dynamodb.scan-exclusive-start-key-from-another-segment-rejected dynamodb.query-filter-dangling-logical-operator-rejected dynamodb.execute-statement-in-operator-with-101-operands-rejected dynamodb.batch-execute-statement-update-stored-type-mismatch-is-a-slot-error-rejected dynamodb.partiql-delete-returning-modified-clauses-rejected dynamodb.partiql-write-through-index-qualifier-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.tag-resource-aws-prefixed-key-rejected-and-its-companion-key-not-applied|run#12|AssertionError|assert got == (_EXPECTED_CODE, _EXPECTED_MESSAGE), (
Issue-Key: dynamodb.query-scan-exclusive-start-key-empty-value-rejected|run#25|AssertionError|assert message == expected, f'Scan/{label}: expected {expected!r}, got {message!r}'
Issue-Key: dynamodb.partiql-statement-index-key-type-mismatch-rejected|run#9|AssertionError|assert err['Error'].get('Message') == message, (
Issue-Key: dynamodb.query-base-table-exclusive-start-key-partition-key-type-mismatch-rejected|expect_error#11|AssertionError|assert actual_message == message, f"expected error message {message!r}, got {actual_message!r}"
Issue-Key: dynamodb.scan-exclusive-start-key-from-another-segment-rejected|_error_code#17|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb.query-filter-dangling-logical-operator-rejected|run#15|AssertionError|assert message2 == expected_stray, f"expected message {expected_stray!r}, got {message2!r}"
Issue-Key: dynamodb.execute-statement-in-operator-with-101-operands-rejected|run#8|AssertionError|assert (code, message) == expected, (
Issue-Key: dynamodb.batch-execute-statement-update-stored-type-mismatch-is-a-slot-error-rejected|run#12|AssertionError|assert responses == [
Issue-Key: dynamodb.partiql-delete-returning-modified-clauses-rejected|_error#20|AssertionError|raise AssertionError(f"expected the statement to be refused, but it succeeded: {result!r}")
Issue-Key: dynamodb.partiql-write-through-index-qualifier-rejected|run#16|AssertionError|assert err.get('Message') == expected, f"expected message {expected!r}, got {err.get('Message')!r}"
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# DynamoDB data-plane refusal wording and PartiQL: 8 net-new causes — empty-key and starting-key messages use the wrong variant, parallel-scan resume is not checked, and PartiQL refusals are prefixed and mis-positioned

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStatement

## Description
Eight net-new root causes in DynamoDB's data-plane refusal messages and its PartiQL surface. All grounded in failing checks listed under '### dynamodb'. Related open issues that do NOT cover these checks: 20261111T170613Z-dynamodb-partiql-statement-parser-rejects-aws-s-grammar.md, 20261111T170613Z-dynamodb-partiql-semantic-validation-index-qualified-select-update.md, 20261111T170613Z-dynamodb-control-plane-refusals-use-emulator-authored-text.md.

Cause 1 of 8: the aws:-prefixed tag-key refusal is worded by the emulator.
AWS: TagResource with a reserved key is refused with ValidationException 'Tag Key cannot be prefixed with aws:, Key: <key>' — fixed text with the offending key taken from the request; no tag in the call is applied.
Emulator: answers ValidationException 'Invalid tag key <key>: the aws: prefix is reserved for Amazon Web Services-assigned tags and cannot be assigned by a caller'.
Why: a message AWS does not emit; clients matching the documented text mis-handle the refusal.
Fix: the TagResource tag-key validation message.
Traces: dynamodb.tag-resource-aws-prefixed-key-rejected-and-its-companion-key-not-applied

Cause 2 of 8: the two empty-string key-attribute refusal variants are applied to the wrong operations.
AWS: there are two distinct messages. Query with an ExclusiveStartKey holding an empty key value uses the plain form 'One or more parameter values are not valid. The AttributeValue for a key attribute cannot contain an empty <kind> value. Key: <attr>'. Scan — which always validates the key against a secondary index — uses the index form 'One or more parameter values are not valid. A value specified for a secondary index key is not supported. The AttributeValue for a key attribute cannot contain an empty <kind> value. IndexName: <index>, IndexKey: <attr>', even for a base-table key attribute that index does not contain. A PartiQL UPDATE setting a secondary-index key to an empty string uses the plain update-expression form 'One or more parameter values are not valid. The update expression attempted to update a secondary index key to a value that is not supported. The AttributeValue for a key attribute cannot contain an empty string value.' The fixed text is the template; attr/index/kind come from the request and the table's schema.
Emulator: swaps them — Scan answers the plain 'Key: pk' form, and the PartiQL UPDATE answers the index form with 'IndexName: by-g, IndexKey: gpk' appended.
Why: both refusals are byte-compared by clients and fixtures; each operation now reports the other's message.
Fix: the key-validation error builder must pick the variant from the operation and the key's role.
Traces: dynamodb.query-scan-exclusive-start-key-empty-value-rejected, dynamodb.partiql-statement-index-key-type-mismatch-rejected

Cause 3 of 8: the ExclusiveStartKey type-mismatch message carries an extra clause.
AWS: a Query whose ExclusiveStartKey partition key has the wrong type is refused with ValidationException 'The provided starting key is invalid' — that text alone.
Emulator: appends a schema clause: 'The provided starting key is invalid: The provided key element does not match the schema'.
Why: the message differs from AWS's for this error class.
Fix: the ExclusiveStartKey validation message.
Traces: dynamodb.query-base-table-exclusive-start-key-partition-key-type-mismatch-rejected

Cause 4 of 8: a parallel Scan accepts an ExclusiveStartKey returned under a different Segment.
AWS: a LastEvaluatedKey is bound to the Segment it came from; resuming it under another Segment of the same TotalSegments is refused with ValidationException 'The provided starting key is invalid: Invalid ExclusiveStartKey. Please use ExclusiveStartKey with correct Segment. TotalSegments: <n> Segment: <s>' — fixed text with the segment numbers from the request.
Emulator: answers HTTP 200 and returns a page of items.
Why: a parallel-scan worker that mixes up segments silently reads the wrong slice instead of being told, so items are double-read or skipped with no error.
Fix: the Scan handler must validate ExclusiveStartKey against the requested Segment/TotalSegments.
Traces: dynamodb.scan-exclusive-start-key-from-another-segment-rejected

Cause 5 of 8: a FilterExpression syntax error names the wrong token and context.
AWS: for a dangling logical operator the refusal is ValidationException 'Invalid FilterExpression: Syntax error; token: "AND", near: "AND AND #s"' — the reported token is the one that cannot start an operand and 'near' is the surrounding slice of the submitted expression.
Emulator: reports 'Invalid FilterExpression: Syntax error; token: "#s", near: "AND #s ="' — it blames a later token.
Why: the diagnostic points the caller at the wrong part of its expression.
Fix: the expression parser's error-position reporting for FilterExpression.
Traces: dynamodb.query-filter-dangling-logical-operator-rejected

Cause 6 of 8: PartiQL runtime refusals are prefixed with 'Invalid Statement: ' and use the expression-engine wording.
AWS: a PartiQL semantic/runtime refusal carries the bare message — 'The IN operator is provided with too many operands; number of operands: <n>' for an over-long IN list, and for a BatchExecuteStatement slot whose UPDATE meets a stored value of the wrong type the slot's Error is {'Code': 'ValidationError', 'Message': 'An operand in the update expression has an incorrect data type'} (a path through a stored scalar gives 'The document path provided in the update expression is invalid for update'), while sibling slots still apply.
Emulator: prefixes 'Invalid Statement: ' and substitutes the operator-detail wording, e.g. 'Invalid Statement: The IN operator is provided with too many operands; number of operands: 101' and slot Error.Message 'Invalid Statement: Incorrect operand type for operator or function; operator or function: +, operand type: S'.
Why: the per-slot error a client inspects to decide which statements to retry is unrecognisable.
Fix: the PartiQL error mapper must emit the runtime message unprefixed, both top-level and per batch slot.
Traces: dynamodb.execute-statement-in-operator-with-101-operands-rejected, dynamodb.batch-execute-statement-update-stored-type-mismatch-is-a-slot-error-rejected

Cause 7 of 8: a PartiQL DELETE with RETURNING MODIFIED clauses is executed instead of refused.
AWS: refuses the statement with HTTP 400 ValidationException; nothing is deleted and no items are returned.
Emulator: answers HTTP 200 with Items and performs the delete.
Why: an unsupported clause silently changes data and returns a payload AWS never returns.
Fix: the PartiQL DELETE handler's RETURNING clause validation.
Traces: dynamodb.partiql-delete-returning-modified-clauses-rejected

Cause 8 of 8: a PartiQL parse-error coordinate reports the wrong token length.
AWS: the refusal is 'FROM clause may only contain a single table name in data manipulation statements at 1:13:29' — the coordinate is line:column:length of the offending FROM clause as AWS measures it.
Emulator: reports '... at 1:13:36'; the fixed text matches but the third coordinate differs.
Why: byte-exact message comparison fails and the caller is pointed at a span of the wrong size.
Fix: the PartiQL parser's source-span computation for the FROM clause.
Traces: dynamodb.partiql-write-through-index-qualifier-rejected

## Evidence
From the behavioral replay, per cause:
- Tag key: "an aws:-prefixed key beside stage gave ('ValidationException', 'Invalid tag key aws:owner: the aws: prefix is reserved for Amazon Web Services-assigned tags and cannot be assigned by a caller'), expected ('ValidationException', 'Tag Key cannot be prefixed with aws:, Key: aws:owner')".
- Empty-key variants: "Scan/table hash (pk, S): expected 'One or more parameter values are not valid. A value specified for a secondary index key is not supported. The AttributeValue for a key attribute cannot contain an empty string value. IndexName: gsi1, IndexKey: pk', got 'One or more parameter values are not valid. The AttributeValue for a key attribute cannot contain an empty string value. Key: pk'"; and the inverse on PartiQL: "expected 'One or more parameter values are not valid. The update expression attempted to update a secondary index key to a value that is not supported. ...', got '... A value specified for a secondary index key is not supported. ... IndexName: by-g, IndexKey: gpk'".
- Starting key: "expected error message 'The provided starting key is invalid', got 'The provided starting key is invalid: The provided key element does not match the schema'".
- Parallel scan resume: "expected the call to be refused, but it succeeded: {'Items': [...], 'Count': 32, 'ScannedCount': 32 ...}" (expected ValidationException 'The provided starting key is invalid: Invalid ExclusiveStartKey. Please use ExclusiveStartKey with correct Segment. TotalSegments: <n> Segment: <s>').
- Filter syntax error: "expected message 'Invalid FilterExpression: Syntax error; token: \"AND\", near: \"AND AND #s\"', got 'Invalid FilterExpression: Syntax error; token: \"#s\", near: \"AND #s =\"'".
- PartiQL prefix: "the 101-operand IN raised ('ValidationException', 'Invalid Statement: The IN operator is provided with too many operands; number of operands: 101'), expected ('ValidationException', 'The IN operator is provided with too many operands; number of operands: 101')"; "slot responses for the stored-type mismatch were [{'Error': {'Code': 'ValidationError', 'Message': 'Invalid Statement: Incorrect operand type for operator or function; operator or function: +, operand type: S'}, 'TableName': ...}, {'TableName': ...}]".
- RETURNING MODIFIED: "expected the statement to be refused, but it succeeded: {'Items': [{'sk': ..., 'pk': ..., 'name': {'S': 'Bea'}, 'qty': {'N': '5'}}] ...}".
- Parse coordinate: "expected message 'FROM clause may only contain a single table name in data manipulation statements at 1:13:29', got '... at 1:13:36'".

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the check ids in trace_ids; each reproduces one cause.
3. Representative manual repro: Scan a table that has one GSI, passing an ExclusiveStartKey whose base-table partition key is an empty string — AWS answers the 'IndexName: gsi1, IndexKey: pk' variant, the emulator answers the plain 'Key: pk' variant.
4. Scan with Segment=0/TotalSegments=4 and Limit=1, then resume the returned LastEvaluatedKey with Segment=1 — AWS refuses with ValidationException, the emulator returns a page.
5. ExecuteStatement with an IN list of 101 operands — compare the message prefix.
6. BatchExecuteStatement with one UPDATE 'SET s = s + 1' against a stored String and one valid sibling — compare Responses[0].Error.Message.
