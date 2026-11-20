Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-attributes-to-get-duplicate-or-empty-rejected-on-reads
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-attributes-to-get-duplicate-or-empty-rejected-on-reads dynamodb-get-item-combined-table-name-key-and-attributes-to-get-validation dynamodb-get-item-return-consumed-capacity-enum-joins-name-and-key-validation-errors dynamodb-getitem-batchgetitem-duplicate-set-key-rendering dynamodb-getitem-projection-alias-overlap-and-syntax-error-token dynamodb-getitem-projection-expression-syntax-and-overlap-forms dynamodb-null-member-and-table-name-aggregation-on-item-ops dynamodb-projection-expression-placeholder-refusals-by-operation dynamodb-batch-caps-and-unprocessed dynamodb-batch-item-value-validation-matches-single-item-wording dynamodb-batch-request-structural-limits dynamodb-batch-write-delete-requests-over-limit dynamodb-batch-write-item-bs-ns-duplicate-members-in-item-and-key-rejected dynamodb-batch-write-item-delete-request-key-number-value-validation dynamodb-batch-write-item-empty-per-table-list-suppresses-other-errors dynamodb-batch-write-item-malformed-key-beside-bad-return-consumed-capacity dynamodb-batch-write-item-number-key-value-limits dynamodb-batch-write-item-number-set-and-negative-magnitude-rejected dynamodb-batch-write-item-number-value-validation-rejects-whole-call dynamodb-batch-write-item-rejects-nested-malformed-number-and-extreme-exponent dynamodb-batch-write-item-request-items-key-pattern-violation-rejected dynamodb-create-and-update-table-provisioned-throughput-bounds-rejected dynamodb-table-name-missing-or-pattern-invalid-across-admin-operations dynamodb-update-table-gsi-attribute-type-invalid-enum-rejected dynamodb-update-table-gsi-update-entry-validation dynamodb-empty-tablename-masks-invalid-returnvalues dynamodb-legacy-expected-comparison-operator-arity-write-ops dynamodb-put-item-condition-expression-operator-limit dynamodb-put-item-duplicate-set-member-rendering-rejected dynamodb-return-options-enum-errors-aggregate-in-fixed-order dynamodb-transact-get-items-invalid-table-name-member-path dynamodb-transact-items-index-arn-tablename-rejected tgi-projection-validation-messages
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|GetItem|4xx->4xx|$.message
Issue-Key: dynamodb|BatchWriteItem|4xx->4xx|$.message
Issue-Key: dynamodb|UpdateTable|4xx->4xx|$.message
Issue-Key: dynamodb|PutItem|4xx->4xx|$.message
Issue-Key: dynamodb|TransactGetItems|4xx->4xx|$.message
Issue-Commit: 4589a545c981676c72550ecd598553d4d546f08e

# DynamoDB ValidationException envelope diverges: member paths, value rendering and error aggregation

> _Note: this issue was identified against the code base at commit `4589a545c981676c72550ecd598553d4d546f08e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetItem

## Description
The emulator answers request-shape violations with the right error class and status (HTTP 400, com.amazon.coral.validate#ValidationException) but assembles the `message` itself instead of reproducing AWS's Coral validation envelope. Five independent causes, all net-new (none is claimed by the replay report's owned_by), all on the `$.message` member. The open issue 20261117T221515Z-dynamodb-refusal-wording-and-validation-precedence-diverge-across.md records the same family of fault on a DIFFERENT set of operations and does not carry these replay keys; all five below must be closed here.

THIS BUNDLE CONTAINS 5 CAUSES.

Cause 1 of 5 — GetItem/BatchGetItem: an empty AttributesToGet is refused with the wrong member path and an extra rendered value.
AWS: Smithy: GetItem.AttributesToGet targets AttributeNameList with @length min 1, so a zero-length list is refused 400 com.amazon.coral.validate#ValidationException. Recorded message: "1 validation error detected: Value at 'AttributesToGet' failed to satisfy constraint: Member must have length greater than or equal to 1". Fixed text of the class: "<n> validation error(s) detected: Value at '<member path>' failed to satisfy constraint: Member must have length greater than or equal to <min>". Variable parts taken from the request/model: the violation count, the member path, and the minimum from the @length trait. For this member AWS names the API-cased path `AttributesToGet` and renders NO value token at all.
Emulator: 400 with "1 validation error detected: Value '[]' at 'attributesToGet' failed to satisfy constraint: Member must have length greater than or equal to 1" — it lower-camels the member path and interpolates a rendered `'[]'` value that AWS omits.
Why: a client that parses the refusal to tell a user which field was wrong is handed a member name (`attributesToGet`) that does not exist in the API, and any assertion on AWS's exact text fails.
Fix: the shared input-validation message renderer for list @length violations on the read path (GetItem/BatchGetItem validation), not the operation handler.
Traces: dynamodb-attributes-to-get-duplicate-or-empty-rejected-on-reads#task#6, plus 12 further steps in the same cluster (dynamodb-getitem-batchgetitem-duplicate-set-key-rendering#task#5, dynamodb-null-member-and-table-name-aggregation-on-item-ops#task#6, ...).

Cause 2 of 5 — BatchWriteItem: the RequestItems constraint message does not reproduce AWS's rendering of the offending value.
AWS: recorded behavior only (the model states the shape, not the message). BatchWriteItem.RequestItems is refused 400 com.amazon.coral.validate#ValidationException and the recorded message is "1 validation error detected: Value '<rendered RequestItems>' at '<member path>' failed to satisfy constraint: ...", where the rendered value is the service model's own structural rendering of the submitted map: `{<table name>=[WriteRequest(putRequest=PutRequest(item={<attr>=AttributeValue(s=<v>, n=null, b=null, iNT=null, dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null, dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}, workloadProfileName=null), deleteRequest=null), ...]}`. The REQUIREMENT is that rendering shape — every structure member spelled out and every unset AttributeValue arm rendered `null`; the table name, the attribute values and the list length are taken from the request, never from one recording.
Emulator: 400 whose `message` is emulator-authored and does not reproduce that rendering — the replay reports `$.message: value_mismatch` for this cluster (39 affected steps). The replay detail is truncated past the expected value, so the exact emulator text must be re-read by replaying the step.
Why: callers that surface the refusal verbatim, and suites that pin it, see text unrelated to AWS's; the rendered value is how AWS shows WHICH request item was rejected.
Fix: the BatchWriteItem input-validation message renderer (value rendering for map/list members).
Traces: dynamodb-batch-caps-and-unprocessed#task#9, plus 38 further steps (dynamodb-batch-write-item-number-key-value-limits#task#5, dynamodb-batch-request-structural-limits#task#7, ...).

Cause 3 of 5 — UpdateTable: only one of two null required ProvisionedThroughput members is reported.
AWS: Smithy: ProvisionedThroughput.ReadCapacityUnits and ProvisionedThroughput.WriteCapacityUnits are BOTH @required (target PositiveLongObject, @range min 1). When both are absent AWS reports both violations in ONE envelope: "2 validation errors detected: Value null at 'provisionedThroughput.writeCapacityUnits' failed to satisfy constraint: Member must not be null; Value null at 'provisionedThroughput.readCapacityUnits' failed to satisfy constraint: Member must not be null". Fixed text per clause: "Value null at '<member path>' failed to satisfy constraint: Member must not be null"; clauses are joined with "; ", the leading count equals the number of clauses, and writeCapacityUnits is reported before readCapacityUnits.
Emulator: "1 validation error detected: Value null at 'provisionedThroughput.readCapacityUnits' failed to satisfy constraint: Member must not be null" — it short-circuits on the first violation it finds, so the count is wrong and one required member is never named.
Why: a caller that fixes exactly what the refusal named gets refused a second time; error counts reported to users are wrong.
Fix: aggregate all required-member violations before raising in the CreateTable/UpdateTable input validator.
Traces: dynamodb-create-and-update-table-provisioned-throughput-bounds-rejected#task#8, dynamodb-table-name-missing-or-pattern-invalid-across-admin-operations#task#1, dynamodb-update-table-gsi-attribute-type-invalid-enum-rejected#task#3, dynamodb-update-table-gsi-update-entry-validation#task#5.

Cause 4 of 5 — PutItem: an empty TableName must mask the other field errors, and is reported with the wrong path.
AWS: Smithy: PutItem.TableName targets TableArn, @required, @length 1..1024. For an empty-string TableName submitted together with an out-of-set ReturnValues, AWS emits exactly ONE clause: "1 validation error detected: Value at 'TableName' failed to satisfy constraint: Member must have length greater than or equal to 1" — API-cased path, no rendered value, and the invalid ReturnValues enum is NOT reported in the same response. The precedence (an unusable TableName suppresses the remaining field checks) is the requirement.
Emulator: "2 validation errors detected: Value '' at 'tableName' failed to satisfy constraint: Member must have length greater than or equal to 1; Value 'BOGUS_RV' at 'returnValues' failed to satisfy constraint: Member must satisfy enum value set: [ALL_NEW, UPDATED_OLD, ALL_OLD, NONE, UPDATED_NEW]" — it reports both, lower-camels the path and interpolates the empty value.
Why: the count, the set of reported fields and the member path all differ, so a client cannot match AWS's refusal or learn the single blocking field.
Fix: validation precedence (short-circuit on an empty/absent TableName) plus the message renderer in the item-write validator.
Traces: dynamodb-empty-tablename-masks-invalid-returnvalues#task#4, plus 10 further steps (dynamodb-put-item-duplicate-set-member-rendering-rejected#task#3, dynamodb-return-options-enum-errors-aggregate-in-fixed-order#task#4, ...).

Cause 5 of 5 — TransactGetItems: the nested member path is flattened to the leaf name.
AWS: Smithy: TransactGetItems.TransactItems is a list (@length 1..100) of TransactGetItem{Get: Get{TableName: TableArn, @pattern [a-zA-Z0-9_.-]+}}. AWS names the FULL path of the offending member, including the 1-based list index and the `member` hop: "1 validation error detected: Value 'bad!name' at 'transactItems.1.member.get.tableName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9_.-]+". Fixed text is the constraint clause; the variable parts are the submitted value, the index of the offending action, and the pattern from the model.
Emulator: "1 validation error detected: Value 'bad!name' at 'tableName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9_.-]+" — it reports the leaf member only.
Why: a transaction may carry up to 100 actions; without the index the caller cannot tell which action is malformed.
Fix: thread the member path (list index + nested member names) through the nested validators used by TransactGetItems/TransactWriteItems.
Traces: dynamodb-transact-get-items-invalid-table-name-member-path#task#4, #task#5, dynamodb-transact-items-index-arn-tablename-rejected#task#4, tgi-projection-validation-messages#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout; all five are `verdict: mismatch` clusters with empty stub_path and empty owned_by.

Cause 1 (GetItem, dynamodb-attributes-to-get-duplicate-or-empty-rejected-on-reads#task#6, 13 affected, status 400 -> 400):
  expected $.message = "1 validation error detected: Value at 'AttributesToGet' failed to satisfy constraint: Member must have length greater than or equal to 1"
  actual   $.message = "1 validation error detected: Value '[]' at 'attributesToGet' failed to satisfy constraint: Member must have length greater than or equal to 1"

Cause 2 (BatchWriteItem, dynamodb-batch-caps-and-unprocessed#task#9, 39 affected, status 400 -> 400):
  expected $.message = "1 validation error detected: Value '{<table>=[WriteRequest(putRequest=PutRequest(item={pk=AttributeValue(s=k0, n=null, b=null, iNT=null, dECIMAL=null, dOUBLE=null, sS=null, nS=null, bS=null, iNTSET=null, dECIMALSET=null, dOUBLESET=null, m=null, l=null, nULL=null, bOOL=null)}, workloadProfileName=null), deleteRequest=null), ...]}' at ... failed to satisfy constraint: ..."
  actual: $.message value_mismatch (emulator-authored text; replay detail truncated past the expected value)

Cause 3 (UpdateTable, dynamodb-create-and-update-table-provisioned-throughput-bounds-rejected#task#8, 4 affected, status 400 -> 400):
  expected $.message = "2 validation errors detected: Value null at 'provisionedThroughput.writeCapacityUnits' failed to satisfy constraint: Member must not be null; Value null at 'provisionedThroughput.readCapacityUnits' failed to satisfy constraint: Member must not be null"
  actual   $.message = "1 validation error detected: Value null at 'provisionedThroughput.readCapacityUnits' failed to satisfy constraint: Member must not be null"

Cause 4 (PutItem, dynamodb-empty-tablename-masks-invalid-returnvalues#task#4, 11 affected, status 400 -> 400):
  expected $.message = "1 validation error detected: Value at 'TableName' failed to satisfy constraint: Member must have length greater than or equal to 1"
  actual   $.message = "2 validation errors detected: Value '' at 'tableName' failed to satisfy constraint: Member must have length greater than or equal to 1; Value 'BOGUS_RV' at 'returnValues' failed to satisfy constraint: Member must satisfy enum value set: [ALL_NEW, UPDATED_OLD, ALL_OLD, NONE, UPDATED_NEW]"

Cause 5 (TransactGetItems, dynamodb-transact-get-items-invalid-table-name-member-path#task#4, 4 affected, status 400 -> 400):
  expected $.message = "1 validation error detected: Value 'bad!name' at 'transactItems.1.member.get.tableName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9_.-]+"
  actual   $.message = "1 validation error detected: Value 'bad!name' at 'tableName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9_.-]+"

Model citations: Smithy GetItem.AttributesToGet -> AttributeNameList @length min 1; ProvisionedThroughput.ReadCapacityUnits/.WriteCapacityUnits both @required @range min 1; PutItem.TableName -> TableArn @required @length 1..1024; TransactGetItems.TransactItems -> list @length 1..100 of TransactGetItem{Get @required}.

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: replay fixture dynamodb-attributes-to-get-duplicate-or-empty-rejected-on-reads step task#6 — GetItem with AttributesToGet: [] — and compare $.message.
3. Cause 2: replay fixture dynamodb-batch-caps-and-unprocessed step task#9 — BatchWriteItem whose RequestItems violates the per-request cap — and compare $.message.
4. Cause 3: replay fixture dynamodb-create-and-update-table-provisioned-throughput-bounds-rejected step task#8 — UpdateTable with ProvisionedThroughput present but both ReadCapacityUnits and WriteCapacityUnits null — and compare the violation count and clauses in $.message.
5. Cause 4: replay fixture dynamodb-empty-tablename-masks-invalid-returnvalues step task#4 — PutItem with TableName "" and ReturnValues out of the enum set — and check that exactly one clause naming 'TableName' is returned.
6. Cause 5: replay fixture dynamodb-transact-get-items-invalid-table-name-member-path step task#4 — TransactGetItems whose first action's Get.TableName is "bad!name" — and compare the member path in $.message.
7. All five must pass before closing.
