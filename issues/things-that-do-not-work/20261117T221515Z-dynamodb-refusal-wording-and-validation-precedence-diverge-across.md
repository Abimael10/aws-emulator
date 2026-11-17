Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-create-table-billingmode-and-readcapacityunits-type-tokens-rejected
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-create-table-billingmode-and-readcapacityunits-type-tokens-rejected dynamodb-create-table-gsi-nested-member-type-mismatches-rejected dynamodb-create-table-index-and-projection-type-mismatches-rejected dynamodb-create-table-key-schema-and-attribute-definitions-token-type-mismatches-rejected dynamodb-create-table-lsi-member-type-mismatch-rejected dynamodb-create-table-member-type-mismatches-rejected dynamodb-create-table-nested-member-scalar-type-mismatch-rejected dynamodb-describe-time-to-live-tablename-type-mismatch-rejected dynamodb-list-tables-exclusivestarttablename-and-limit-type-matrix-rejected dynamodb-tag-resource-serialization-type-matrix-rejected dynamodb-update-table-gsi-create-member-type-mismatch-rejected dynamodb-update-table-gsi-update-and-provisioned-throughput-wrong-json-type-rejected dynamodb-update-table-gsi-update-member-type-mismatch-rejected dynamodb-update-table-provisioned-throughput-and-billing-mode-type-mismatch-rejected dynamodb-tag-resource-arn-tags-precedence dynamodb-untag-resource-arn-tagkeys-precedence-rejected dynamodb-search-vectors-request-validation-order-and-type-rejected dynamodb-vector-index-and-search-lower-bound-validation-rejected dynamodb-update-table-gsi-update-nested-member-type-mismatch-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|CreateTable|4xx->4xx|$.Message
Issue-Key: dynamodb|DescribeTimeToLive|4xx->4xx|$.Message
Issue-Key: dynamodb|ListTables|4xx->4xx|$.Message
Issue-Key: dynamodb|TagResource|4xx->4xx|$.Message
Issue-Key: dynamodb|UpdateTable|4xx->4xx|$.Message
Issue-Key: dynamodb|TagResource|4xx->4xx|$.message
Issue-Key: dynamodb|UntagResource|4xx->4xx|$.message
Issue-Key: dynamodb|SearchVectors|4xx->4xx|$.message
Issue-Key: dynamodb|UpdateTable|4xx->4xx|$.__type
Issue-Key: dynamodb|ListTables|2xx->4xx|$.TableNames
Issue-Key: dynamodb|ListTables|2xx->4xx|status
Issue-Related: 20261110T232401Z-dynamodb-validation-and-not-found-refusal-messages-are.md
Issue-Commit: 656cbf9b11e99225d4fd85cb8b7394b019211c7b

# DynamoDB refusal wording and validation precedence diverge across ten operations, and one valid ListTables is refused

> _Note: this issue was identified against the code base at commit `656cbf9b11e99225d4fd85cb8b7394b019211c7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTable

## Description
Ten net-new causes on dynamodb in the request-deserializer and the per-operation validation ordering: the coercion-failure message names a generic token class instead of the literal that was found, two structural serialization messages use emulator-authored text, two tag operations check the ARN before the empty-collection rule AWS checks first, the required-member message is re-worded, UpdateTable checks table existence before index validation, and one well-formed ListTables is refused outright. Related but NOT covering: open issue 20261111T221143Z-dynamodb-item-and-batch-operations-answer-validationexception-where.md covers item/batch classes only and carries none of these replay keys.

Cause 1 of 10: CreateTable coercion failure names the JSON token class, not the literal token.
AWS: Smithy: CreateTable.ProvisionedThroughput.ReadCapacityUnits targets a Long; BillingMode targets the BillingMode enum. When a boolean literal appears where a Long is expected the awsJson1_0 deserializer answers HTTP 400 with Message 'TRUE_VALUE cannot be converted to Long'. The fixed text is '<TOKEN> cannot be converted to <TargetType>'; the variable parts are the literal token actually found (TRUE_VALUE / FALSE_VALUE / STRING_VALUE ...) taken from the request body and the modeled target type.
Emulator: Message 'BOOLEAN_VALUE cannot be converted to Long' — it substitutes the token's generic class for the literal.
Why: clients and test suites that match the deserializer's message see different text for the same malformed payload, and the message no longer identifies which literal was found.
Fix: the token-name table in the awsJson1_0 request deserializer's coercion-error formatter.
Traces: dynamodb-create-table-billingmode-and-readcapacityunits-type-tokens-rejected#task#1 (cluster of 15 steps across 6 CreateTable fixtures).

Cause 2 of 10: the same substitution on a String target, via DescribeTimeToLive.
AWS: Smithy: DescribeTimeToLive.TableName targets TableArn (string, @length 1-1024). A boolean literal there yields Message 'TRUE_VALUE cannot be converted to String'.
Emulator: 'BOOLEAN_VALUE cannot be converted to String'.
Why: same as cause 1, on the string-coercion path.
Fix: same formatter.
Traces: dynamodb-describe-time-to-live-tablename-type-mismatch-rejected#task#5 (cluster of 3 steps).

Cause 3 of 10: the same substitution on ListTables.
AWS: Smithy: ListTables.ExclusiveStartTableName is a string; a boolean literal yields Message 'TRUE_VALUE cannot be converted to String'.
Emulator: 'BOOLEAN_VALUE cannot be converted to String'.
Why: same as cause 1.
Fix: same formatter.
Traces: dynamodb-list-tables-exclusivestarttablename-and-limit-type-matrix-rejected#task#1 (cluster of 7 steps).

Cause 4 of 10: TagResource structural serialization error is emulator-authored.
AWS: Smithy: TagResource.ResourceArn is a string and Tags is a TagList; when an object/map appears where a scalar or list member is modeled, the deserializer answers HTTP 400 with Message 'Start of structure or map found where not expected' (fixed text, no variable parts).
Emulator: Message 'Unexpected field type'.
Why: the structural-shape error class is indistinguishable from any other type error, and the AWS wording clients key on is absent.
Fix: the structural branch of the awsJson1_0 deserializer's error formatter.
Traces: dynamodb-tag-resource-serialization-type-matrix-rejected#task#2 (cluster of 3 steps).

Cause 5 of 10: UpdateTable value-type serialization error is emulator-authored.
AWS: Smithy: UpdateTable.ProvisionedThroughput / GlobalSecondaryIndexUpdates members are typed structures; a wrong JSON value type inside them yields HTTP 400 with Message 'Unexpected value type in payload' (fixed text).
Emulator: Message 'Unexpected field type'.
Why: same wording divergence, on the nested-value path; the two distinct AWS messages are collapsed into one emulator-authored string.
Fix: same error formatter, nested-value branch.
Traces: dynamodb-update-table-gsi-create-member-type-mismatch-rejected#task#3 (cluster of 13 steps across 4 UpdateTable fixtures).

Cause 6 of 10: TagResource checks the ARN before the empty-Tags rule.
AWS: Smithy: TagResource.Tags is @required (TagList). With an empty Tags list and a malformed/unresolvable ResourceArn, DynamoDB refuses on the collection first: HTTP 400 ValidationException with message 'Atleast one Tag needs to be provided as Input.' (fixed text, exactly as AWS spells it, including the missing space in 'Atleast' and the terminating period).
Emulator: refuses on the ARN instead — message 'Invalid TableArn: Invalid ResourceArn provided as input <arn>' (its own template, filled from the request's ResourceArn).
Why: validation precedence is observable: a client fixing the error AWS reports first (the empty tag list) still gets a refusal here, and vice versa, so error-driven retry loops diverge.
Fix: ordering in the TagResource handler's validation — the required/non-empty Tags check must precede ARN resolution.
Traces: dynamodb-tag-resource-arn-tags-precedence#task#2.

Cause 7 of 10: UntagResource has the same inverted precedence.
AWS: Smithy: UntagResource.TagKeys is @required (TagKeyList). With an empty TagKeys list, DynamoDB answers HTTP 400 ValidationException 'Atleast one Tag Key needs to be provided as Input.' (fixed text) before examining the ARN.
Emulator: answers the ARN refusal 'Invalid TableArn: Invalid ResourceArn provided as input <arn>'.
Why: same as cause 6.
Fix: ordering in the UntagResource handler's validation.
Traces: dynamodb-untag-resource-arn-tagkeys-precedence-rejected#task#2.

Cause 8 of 10: SearchVectors re-words the missing-required-member refusal.
AWS: Smithy: SearchVectors.TableName is @required. With TableName absent the recorded refusal is HTTP 400 with message 'missing field `TableName` at line 1 column 62' — fixed text 'missing field `<member>` at line <line> column <column>', whose variable parts are the member name and the parse position within the request body.
Emulator: message "The parameter 'TableName' is required but was not present in the request".
Why: the message is produced by a different layer than AWS's (a body-parse position vs a parameter check), so clients see neither the wording nor the location AWS reports.
Fix: the required-member branch of the SearchVectors request deserializer.
Traces: dynamodb-search-vectors-request-validation-order-and-type-rejected#task#7 (cluster of 2 steps).

Cause 9 of 10: UpdateTable checks table existence before secondary-index validation.
AWS: Smithy: UpdateTable declares both ResourceNotFoundException and (service-common) ValidationException. For a GlobalSecondaryIndexUpdate that creates an index whose key attributes are not declared in AttributeDefinitions, DynamoDB refuses with HTTP 400, __type 'com.amazon.coral.validate#ValidationException', message 'One or more parameter values were invalid: AttributeDefinitions is not specified for index: <indexName>' — fixed text with the index name from the request — EVEN when the named table does not exist.
Emulator: answers HTTP 400 __type 'com.amazonaws.dynamodb.v20120810#ResourceNotFoundException', message 'Requested resource not found: Table: <tableName> not found'.
Why: the request-shape validation that AWS performs before any state lookup is skipped, so a client gets a retryable not-found class for a permanently invalid request.
Fix: ordering in the UpdateTable handler — index/AttributeDefinitions validation precedes table resolution.
Traces: dynamodb-update-table-gsi-update-nested-member-type-mismatch-rejected#task#7.

Cause 10 of 10: a well-formed ListTables is refused 400 instead of answering an empty listing.
AWS: Smithy: ListTables has no required members; ExclusiveStartTableName (string) and Limit (@range min 1) are optional. The recorded well-formed call answers HTTP 200 with TableNames: [] (an empty list, not an omitted member).
Emulator: HTTP 400 and no TableNames member.
Why: a legal listing call fails, so clients cannot enumerate tables at all after a malformed one; and the empty-list-vs-absent-member contract is broken.
Fix: the ListTables handler's input validation (it must not reject the well-formed parameter combination) and its serializer (TableNames must be emitted as []).
Traces: dynamodb-list-tables-exclusivestarttablename-and-limit-type-matrix-rejected#task#9.

## Evidence
Cause 1 — CreateTable: expected Message 'TRUE_VALUE cannot be converted to Long'; actual 'BOOLEAN_VALUE cannot be converted to Long' (15 steps).
Cause 2 — DescribeTimeToLive: expected 'TRUE_VALUE cannot be converted to String'; actual 'BOOLEAN_VALUE cannot be converted to String' (3 steps).
Cause 3 — ListTables: same pair (7 steps).
Cause 4 — TagResource: expected Message 'Start of structure or map found where not expected'; actual 'Unexpected field type' (3 steps).
Cause 5 — UpdateTable: expected Message 'Unexpected value type in payload'; actual 'Unexpected field type' (13 steps).
Cause 6 — TagResource: expected message 'Atleast one Tag needs to be provided as Input.'; actual 'Invalid TableArn: Invalid ResourceArn provided as input arn:aws:dynamodb:<region>:<account>:table/<name>'.
Cause 7 — UntagResource: expected 'Atleast one Tag Key needs to be provided as Input.'; actual the same Invalid-TableArn message.
Cause 8 — SearchVectors: expected message 'missing field `TableName` at line 1 column 62'; actual "The parameter 'TableName' is required but was not present in the request" (2 steps).
Cause 9 — UpdateTable: expected 400 __type 'com.amazon.coral.validate#ValidationException' + 'One or more parameter values were invalid: AttributeDefinitions is not specified for index: <indexName>'; actual 400 __type 'com.amazonaws.dynamodb.v20120810#ResourceNotFoundException' + 'Requested resource not found: Table: <tableName> not found'.
Cause 10 — ListTables: expected 200 with TableNames=[]; actual 400 with TableNames missing.

## Steps to Reproduce
1. Replay fixtures dynamodb-create-table-billingmode-and-readcapacityunits-type-tokens-rejected (task#1), dynamodb-describe-time-to-live-tablename-type-mismatch-rejected (task#5), dynamodb-list-tables-exclusivestarttablename-and-limit-type-matrix-rejected (task#1, task#9), dynamodb-tag-resource-serialization-type-matrix-rejected (task#2), dynamodb-update-table-gsi-create-member-type-mismatch-rejected (task#3), dynamodb-tag-resource-arn-tags-precedence (task#2), dynamodb-untag-resource-arn-tagkeys-precedence-rejected (task#2), dynamodb-search-vectors-request-validation-order-and-type-rejected (task#7), dynamodb-update-table-gsi-update-nested-member-type-mismatch-rejected (task#7).
2. Causes 1-3: send CreateTable/DescribeTimeToLive/ListTables with a boolean literal (true) in a Long- or String-typed member and compare the deserializer Message.
3. Causes 4-5: send TagResource with an object where Tags/ResourceArn is modeled, and UpdateTable with a wrong JSON value type inside ProvisionedThroughput/GlobalSecondaryIndexUpdates.
4. Causes 6-7: TagResource with {"ResourceArn":"arn:aws:dynamodb:us-west-2:000000000000:table/ab","Tags":[]} and UntagResource with the same ARN and "TagKeys":[].
5. Cause 8: SearchVectors omitting TableName.
6. Cause 9: UpdateTable on an absent table with a GlobalSecondaryIndexUpdates Create whose key attribute is not in AttributeDefinitions.
7. Cause 10: the well-formed ListTables at task#9 of the type-matrix fixture.
