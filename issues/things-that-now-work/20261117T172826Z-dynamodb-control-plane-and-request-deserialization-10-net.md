Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.batchwriteitem-requestitems-wrong-json-type-rejected dynamodb.deletetable-tablename-wrong-json-type-serialization-exception dynamodb.list-tags-of-resource-arn-wrong-json-type-rejected dynamodb.describetable-tablename-wrong-json-type-rejected dynamodb.update-table-gsi-update-shape-validation-precedes-table-lookup-rejected dynamodb.update-table-delete-of-an-absent-gsi-rejected dynamodb.update-table-gsi-update-absent-index-or-table-rejected dynamodb.update-table-six-gsi-deletes-of-absent-indexes-limit-exceeded-rejected dynamodb.update-table-gsi-delete-targeting-lsi-name-rejected dynamodb.deep-dynamodb-1-ddbvec-delete-unknown-vector-index-rejected dynamodb.update-table-gsi-add-duplicate-attribute-name-in-attribute-definitions-rejected dynamodb.create-table-rejects-provisioned-throughput-above-the-per-table-quota dynamodb.create-table-stream-enabled-without-a-view-type-rejected dynamodb.create-table-throughput-validation-precedes-keyschema-validation dynamodb.create-table-attribute-name-over-255-chars-rejected dynamodb.export-table-to-point-in-time-invalid-export-format-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.batchwriteitem-requestitems-wrong-json-type-rejected|run#24|AssertionError|assert set(body_array) == {"__type"}, (
Issue-Key: dynamodb.deletetable-tablename-wrong-json-type-serialization-exception|run#27|AssertionError|assert "Message" not in body_list, (
Issue-Key: dynamodb.list-tags-of-resource-arn-wrong-json-type-rejected|run#31|AssertionError|assert body == {"__type": _SER}, (
Issue-Key: dynamodb.describetable-tablename-wrong-json-type-rejected|run#22|AssertionError|assert body.get("Message") == expected_message, (
Issue-Key: dynamodb.update-table-gsi-update-shape-validation-precedes-table-lookup-rejected|run#15|AssertionError|assert (code, msg) == expected_fold, (
Issue-Key: dynamodb.update-table-delete-of-an-absent-gsi-rejected|run#9|AssertionError|assert (code, msg) == want, f"deleting the absent GSI {_ABSENT!r} gave {code!r}: {msg!r}"
Issue-Key: dynamodb.update-table-gsi-update-absent-index-or-table-rejected|run#15|AssertionError|assert (code, msg) == expected_rcu, (
Issue-Key: dynamodb.update-table-six-gsi-deletes-of-absent-indexes-limit-exceeded-rejected|run#14|AssertionError|assert (code, msg) == expected_not_found, (
Issue-Key: dynamodb.update-table-gsi-delete-targeting-lsi-name-rejected|run#9|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: dynamodb.deep-dynamodb-1-ddbvec-delete-unknown-vector-index-rejected|run#11|AssertionError|assert err.get('Code') == 'ResourceNotFoundException', (
Issue-Key: dynamodb.update-table-gsi-add-duplicate-attribute-name-in-attribute-definitions-rejected|_error#6|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: dynamodb.create-table-rejects-provisioned-throughput-above-the-per-table-quota|expect_error#10|AssertionError|raise AssertionError(f"expected a ClientError, but the call succeeded: {result!r}")
Issue-Key: dynamodb.create-table-stream-enabled-without-a-view-type-rejected|run#8|AssertionError|assert message == no_view, f"the refusal carried the message {message!r}"
Issue-Key: dynamodb.create-table-throughput-validation-precedes-keyschema-validation|run#6|AssertionError|assert msg1 == expected1, f"expected message {expected1!r}, got {msg1!r}"
Issue-Key: dynamodb.create-table-attribute-name-over-255-chars-rejected|run#14|AssertionError|assert message == expected, f"unexpected message {message!r}"
Issue-Key: dynamodb.export-table-to-point-in-time-invalid-export-format-rejected|run#18|AssertionError|assert message == expected, f"ExportFormat='CSV' message was {message!r}, expected {expected!r}"
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# DynamoDB control plane and request deserialization: 10 net-new causes — absent-index updates answer the wrong error class, quotas and duplicate definitions are not enforced, and malformed JSON refusals carry the wrong body

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateTable

## Description
Ten net-new root causes in the DynamoDB control plane and in the awsJson1.0 request deserializer. All grounded in failing checks listed under '### dynamodb'. Related open issues that do NOT cover these checks: 20261111T170613Z-dynamodb-control-plane-refusals-use-emulator-authored-text.md, 20261029T180026Z-dynamodb-createtable-updatetable-putitem-validation-10-missing-or.md, 20261111T170613Z-dynamodb-non-constraint-validationexception-messages-are-wrapped-in.md.

Cause 1 of 10: a SerializationException for a wrong JSON field type carries a message AWS does not send.
AWS: when a member's JSON type is wrong in a way the deserializer cannot coerce (e.g. RequestItems or ResourceArn or TableName sent as a JSON list), the refusal body is awsJson's bare carrier: {'__type': 'com.amazon.coral.service#SerializationException'} with NO 'Message'/'message' member.
Emulator: sends {'__type': 'com.amazon.coral.service#SerializationException', 'Message': 'Unexpected field type'}.
Why: a client or test that asserts the exact error body — or that branches on the presence of a message to distinguish a serialization refusal from a modelled error — sees a body AWS never produces.
Fix: the awsJson error serializer must omit the message member for SerializationException.
Traces: dynamodb.batchwriteitem-requestitems-wrong-json-type-rejected, dynamodb.deletetable-tablename-wrong-json-type-serialization-exception, dynamodb.list-tags-of-resource-arn-wrong-json-type-rejected

Cause 2 of 10: the coral type name for a fractional number in a coercion message is wrong.
AWS: TableName sent as 3.14 is refused with the message 'DECIMAL_VALUE cannot be converted to String' — the fixed text, with the source type token taken from the JSON value's kind (a fractional literal is DECIMAL_VALUE).
Emulator: sends 'NUMBER_VALUE cannot be converted to String'.
Why: the diagnostic names a different source type than AWS, so message-matching clients mis-report.
Fix: the deserializer's type-coercion message must distinguish integral from fractional JSON numbers.
Traces: dynamodb.describetable-tablename-wrong-json-type-rejected

Cause 3 of 10: a null required member inside GlobalSecondaryIndexUpdates produces a serde SerializationException instead of a ValidationException constraint report.
AWS: shape validation runs before the table lookup and reports every violated constraint in one ValidationException, e.g. "5 validation errors detected: Value 'bad name!' at 'globalSecondaryIndexUpdates.1.member.update.indexName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9_.-]+; Value null at 'globalSecondaryIndexUpdates.1.member.update.provisionedThroughput.writeCapacityUnits' failed to satisfy constraint: Member must not be null; ..." — the count, the member paths and the constraint clauses are the fixed template; the values come from the request.
Emulator: answers ('SerializationException', 'invalid type: null, expected i64 at line 1 column 179') — an internal Rust/serde diagnostic leaking a byte offset.
Why: the caller gets an unmodelled error class with no member path, and cannot see that several members are invalid.
Fix: the UpdateTable input deserializer must accept nulls into optional slots and let the constraint validator report them, rather than failing the parse.
Traces: dynamodb.update-table-gsi-update-shape-validation-precedes-table-lookup-rejected

Cause 4 of 10: naming an index that does not exist answers ValidationException instead of ResourceNotFoundException.
AWS: Smithy — UpdateTable models ResourceNotFoundException (client, HTTP 400). An UpdateTable Update or Delete (including a VectorIndexUpdates Delete) that names an absent index is refused with ResourceNotFoundException 'Requested resource not found: Index <indexName> for table <tableName>' — fixed text, with the index and table names taken from the request. A Delete that names an existing LOCAL secondary index is instead refused with ValidationException 'Cannot delete an index that is not a GlobalSecondaryIndex: <indexName>'. Six Deletes of absent indexes in one call additionally hit the per-call limit.
Emulator: answers ValidationException with emulator-authored text — 'Attempting to delete an index which does not exist: <name>', 'Attempting to update an index which does not exist: <name>' — and uses the same 'does not exist' text when the named index is an LSI.
Why: a client's not-found branch (ResourceNotFoundException) never fires, so a typo'd or already-deleted index looks like a request-shape error, and an LSI mis-targeted delete is indistinguishable from an absent index.
Fix: the UpdateTable GSI/vector-index update handler's index lookup and its error mapping.
Traces: dynamodb.update-table-delete-of-an-absent-gsi-rejected, dynamodb.update-table-gsi-update-absent-index-or-table-rejected, dynamodb.update-table-six-gsi-deletes-of-absent-indexes-limit-exceeded-rejected, dynamodb.update-table-gsi-delete-targeting-lsi-name-rejected, dynamodb.deep-dynamodb-1-ddbvec-delete-unknown-vector-index-rejected

Cause 5 of 10: UpdateTable adding a GSI with a duplicated AttributeDefinitions entry is accepted.
AWS: refuses the call with HTTP 400 ValidationException; the table is not put into UPDATING and no index is created.
Emulator: answers HTTP 200 with TableStatus 'UPDATING' and the new index in CREATING.
Why: a malformed template/SDK call lands a table in a state AWS would never produce, and the caller never sees the defect.
Fix: the UpdateTable attribute-definition validation (duplicate attribute names).
Traces: dynamodb.update-table-gsi-add-duplicate-attribute-name-in-attribute-definitions-rejected

Cause 6 of 10: the per-table provisioned-throughput quota is not enforced at CreateTable.
AWS: a CreateTable asking for read capacity above the per-table quota is refused (HTTP 400) and no table is created.
Emulator: answers HTTP 200 with TableDescription ProvisionedThroughput {'ReadCapacityUnits': 40001, 'WriteCapacityUnits': 1} and creates the table.
Why: capacity-quota handling in client code is never exercised, and the emulator admits a table shape AWS refuses.
Fix: the CreateTable throughput validation (per-table quota ceiling).
Traces: dynamodb.create-table-rejects-provisioned-throughput-above-the-per-table-quota

Cause 7 of 10: the CreateTable 'stream without a view type' refusal is worded by the emulator.
AWS: StreamSpecification {'StreamEnabled': true} with no StreamViewType is refused with ValidationException 'One or more parameter values were invalid: Table is being created with a stream enabled, UpdateViewType is required' — that exact fixed text.
Emulator: answers 'One or more parameter values were invalid: StreamViewType is required when StreamEnabled is true'.
Why: the message differs from AWS's wording for this error class, so message-matching clients and fixtures diverge.
Fix: the CreateTable StreamSpecification validation message.
Traces: dynamodb.create-table-stream-enabled-without-a-view-type-rejected

Cause 8 of 10: CreateTable validates key schema before throughput, inverting AWS's order.
AWS: when a request is both missing PROVISIONED throughput and has a malformed KeySchema, the throughput defect is reported first: ValidationException 'One or more parameter values were invalid: ReadCapacityUnits and WriteCapacityUnits must both be specified when BillingMode is PROVISIONED'.
Emulator: answers 'Invalid KeySchema: The second KeySchemaElement is not a RANGE key type' — the key-schema check runs first.
Why: a caller fixing the first reported defect follows a different repair sequence than against AWS, and the single-error assertion fails.
Fix: the CreateTable validation ordering.
Traces: dynamodb.create-table-throughput-validation-precedes-keyschema-validation

Cause 9 of 10: a multi-violation ValidationException lists its members in the wrong order.
AWS: when one over-long attribute name violates constraints on both keySchema and attributeDefinitions, the message is '2 validation errors detected: Value '<name>' at 'keySchema.1.member.attributeName' failed to satisfy constraint: Member must have length less than or equal to 255; Value '<name>' at 'attributeDefinitions.1.member.attributeName' failed to satisfy constraint: ...' — keySchema first. The count and clauses are fixed text; the value and member paths come from the request.
Emulator: emits the same two violations with attributeDefinitions first.
Why: the message is a documented, byte-stable contract many clients assert on; the order change breaks it.
Fix: the constraint-validation reporter's member ordering.
Traces: dynamodb.create-table-attribute-name-over-255-chars-rejected

Cause 10 of 10: the enum value set printed in a constraint message uses the model's declaration order instead of AWS's.
AWS: ExportTableToPointInTime with ExportFormat='CSV' is refused with "1 validation error detected: Value 'CSV' at 'exportFormat' failed to satisfy constraint: Member must satisfy enum value set: [ION, DYNAMODB_JSON]" — the listed set is AWS's own order, which is NOT the Smithy declaration order (model ExportFormat values: [DYNAMODB_JSON, ION]); the same inversion holds for ScalarAttributeType ([B, N, S] in messages vs [S, N, B] in the model).
Emulator: prints '[DYNAMODB_JSON, ION]' — the model order.
Why: byte-exact message comparison fails, and the emulator's order cannot be derived from the model, so it must be pinned per enum.
Fix: the constraint-validation reporter's enum-value-set rendering (per-enum message order table).
Traces: dynamodb.export-table-to-point-in-time-invalid-export-format-rejected

## Evidence
From the behavioral replay, per cause:
- SerializationException body: "RequestItems=['a', 'b'] refusal body carried a Message: {'__type': 'com.amazon.coral.service#SerializationException', 'Message': 'Unexpected field type'}, expected only __type"; "a list-typed ResourceArn gave body {'__type': 'com.amazon.coral.service#SerializationException', 'Message': 'Unexpected field type'}, expected only __type with no message"; "TableName as a JSON list carried a message 'Unexpected field type' -- measured, DynamoDB sends none for this refusal".
- Coercion type name: "TableName=3.14 gave message 'NUMBER_VALUE cannot be converted to String', expected 'DECIMAL_VALUE cannot be converted to String'".
- Null required member: "the two malformed Update entries gave ('SerializationException', 'invalid type: null, expected i64 at line 1 column 179'), expected ('ValidationException', \"5 validation errors detected: Value 'bad name!' at 'globalSecondaryIndexUpdates.1.member.update.indexName' ...\")".
- Absent index: "deleting the absent GSI 'by-nothing' gave 'ValidationException': 'Attempting to delete an index which does not exist: by-nothing'"; "Update of absent index 'absent-index-name' with a huge RCU gave ('ValidationException', 'Attempting to update an index which does not exist: absent-index-name'), expected ('ResourceNotFoundException', 'Requested resource not found: Index absent-index-name for table <table>')"; "expected message 'Cannot delete an index that is not a GlobalSecondaryIndex: lsi-main', got 'Attempting to delete an index which does not exist: lsi-main'"; "expected ResourceNotFoundException, got {'Message': 'Attempting to delete an index which does not exist: idx-dropped', 'Code': 'ValidationException'}".
- Duplicate attribute definition: "expected the call to be rejected, but it succeeded: {'TableDescription': {... 'TableStatus': 'UPDATING' ...}}".
- Throughput quota: "expected a ClientError, but the call succeeded: {'TableDescription': {... 'ProvisionedThroughput': {'ReadCapacityUnits': 40001, 'WriteCapacityUnits': 1} ...}}".
- Stream view type: "the refusal carried the message 'One or more parameter values were invalid: StreamViewType is required when StreamEnabled is true'" (expected '... Table is being created with a stream enabled, UpdateViewType is required').
- Validation order: "expected message 'One or more parameter values were invalid: ReadCapacityUnits and WriteCapacityUnits must both be specified when BillingMode is PROVISIONED', got 'Invalid KeySchema: The second KeySchemaElement is not a RANGE key type'".
- Violation order: emulator emitted "2 validation errors detected: Value '<k...>' at 'attributeDefinitions.1.member.attributeName' ...; Value '<k...>' at 'keySchema.1.member.attributeName' ..." where AWS lists keySchema first.
- Enum order: "ExportFormat='CSV' message was \"...enum value set: [DYNAMODB_JSON, ION]\", expected \"...enum value set: [ION, DYNAMODB_JSON]\"".

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the check ids in trace_ids; each reproduces one cause.
3. Representative manual repro: POST an awsJson1.0 DeleteTable body with TableName as a JSON list — compare the error body members with AWS's bare __type carrier.
4. UpdateTable with GlobalSecondaryIndexUpdates [{'Delete': {'IndexName': 'absent'}}] on an existing table — AWS answers ResourceNotFoundException 'Requested resource not found: Index absent for table <t>', the emulator answers ValidationException.
5. CreateTable with BillingMode PROVISIONED, ProvisionedThroughput ReadCapacityUnits 40001 — AWS refuses, the emulator creates the table.
6. ExportTableToPointInTime with ExportFormat='CSV' and compare the enum value set order in the message.
