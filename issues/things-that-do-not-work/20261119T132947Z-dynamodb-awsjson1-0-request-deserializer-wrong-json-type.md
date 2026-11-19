Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-batch-get-item-key-and-table-option-serialization-type-mismatches-rejected
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-batch-get-item-key-and-table-option-serialization-type-mismatches-rejected dynamodb-batch-write-item-serialization-type-errors dynamodb-put-item-serialization-type-matrix-rejected dynamodb-putitem-condition-expression-and-placeholder-maps-wrong-type-rejected dynamodb-putitem-item-top-level-wrong-type-variants-rejected dynamodb-delete-item-expected-value-json-type-matrix-rejected dynamodb-delete-item-expression-attribute-wrong-json-type-rejected dynamodb-delete-item-request-member-type-mismatches-rejected dynamodb-deleteitem-wrong-json-type-for-return-options-and-expressions-rejected dynamodb-query-expression-and-projection-member-wrong-json-type-rejected dynamodb-query-filter-and-key-conditions-attribute-value-list-json-type-variants-rejected dynamodb-query-return-consumed-capacity-and-limit-json-type-variants-rejected dynamodb-update-item-expected-and-attributeupdates-type-mismatches-rejected dynamodb-update-item-string-and-map-member-type-mismatch-rejected dynamodb-updateitem-key-expected-attributeupdates-json-type-mismatches-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|BatchGetItem|4xx->4xx|$.Message
Issue-Key: dynamodb|BatchWriteItem|4xx->4xx|$.Message
Issue-Key: dynamodb|PutItem|4xx->4xx|$.Message
Issue-Key: dynamodb|DeleteItem|4xx->4xx|$.Message
Issue-Key: dynamodb|Query|4xx->4xx|$.Message
Issue-Key: dynamodb|UpdateItem|4xx->4xx|$.Message
Issue-Commit: 1d39de9b4cbdf24f97bce0e468a0bf0f3f7e1cf5

# DynamoDB awsJson1.0 request deserializer: wrong-JSON-type refusal bodies carry AWS-foreign type tokens and an extra Message

> _Note: this issue was identified against the code base at commit `1d39de9b4cbdf24f97bce0e468a0bf0f3f7e1cf5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutItem

## Description
Two root causes in the shared awsJson1.0 request deserializer's error path, reproduced by 6 divergence clusters (33 replayed steps) spread over BatchGetItem, BatchWriteItem, PutItem, DeleteItem, Query and UpdateItem. Both are about the body of the refusal the deserializer emits when a request member's JSON type is wrong; the HTTP status (400) and the __type (com.amazon.coral.service#SerializationException) are already right, only the body members/text diverge.

Related open issue that does NOT cover these fixture steps (it is grounded in the behavioral corpus, on DeleteTable/DescribeTable/ListTagsOfResource/BatchWriteItem checks): 20261117T172826Z-dynamodb-control-plane-and-request-deserialization-10-net.md. The clusters below are unowned by any open issue, per the replay report.

Cause 1 of 2: the coral source-type token for a JSON boolean is wrong in the coercion message.
AWS: when a member modeled as a string receives a JSON `true`, the deserializer refuses with SerializationException whose message is the fixed text '<TOKEN> cannot be converted to String', where <TOKEN> names the KIND of the offending JSON literal as coral names it: a `true` literal is TRUE_VALUE (a `false` literal FALSE_VALUE, an integral number NUMBER_VALUE, a fractional number DECIMAL_VALUE). Nothing in the Smithy model states the token table — recorded behavior only for the token itself; the member types it applies to are modeled (e.g. Smithy: BatchGetItemInput.RequestItems is a map keyed by TableArn (string); PutItemInput.TableName -> TableArn string; AttributeValue.S -> string).
Emulator: emits one generic token for every boolean, 'BOOLEAN_VALUE cannot be converted to String', so TRUE_VALUE and FALSE_VALUE are indistinguishable and neither matches AWS.
Why: the message is the only diagnostic a client gets for a serialization refusal (there is no member path in it), so SDK tests and client log/message matching that key on AWS's token mis-report or fail to classify which literal was at fault.
Fix: the awsJson1.0 request deserializer's type-coercion message builder — the JSON-value-kind -> coral token mapping (true/false must map to TRUE_VALUE/FALSE_VALUE, not to a single BOOLEAN_VALUE).
Traces: dynamodb-batch-get-item-key-and-table-option-serialization-type-mismatches-rejected#task#4 (BatchGetItem, +2 steps in cluster), dynamodb-batch-write-item-serialization-type-errors#task#5 (BatchWriteItem, +1), dynamodb-put-item-serialization-type-matrix-rejected#task#3 (PutItem, +8).

Cause 2 of 2: the SerializationException carrier carries a Message member AWS does not send.
AWS: for a wrong JSON field type the deserializer cannot coerce at all (e.g. a map/list/number where a structure or string is modeled), the refusal body is the bare awsJson carrier — only {'__type': 'com.amazon.coral.service#SerializationException'} — with NO 'Message' or 'message' member. Recorded behavior only (the model types the members but says nothing about the error body): every one of these recorded responses has no message member.
Emulator: adds 'Message': 'Unexpected field type' to the body.
Why: a client that branches on the presence of a message to tell a bare serialization refusal from a modeled, message-bearing error (ValidationException, ResourceNotFoundException) takes the wrong branch, and exact-body assertions see a body AWS never produces.
Fix: the awsJson1.0 error serializer — omit the message member for SerializationException (same component as cause 1, different field).
Traces: dynamodb-delete-item-expected-value-json-type-matrix-rejected#task#7 (DeleteItem, +6 steps in cluster), dynamodb-query-expression-and-projection-member-wrong-json-type-rejected#task#4 (Query, +11), dynamodb-update-item-expected-and-attributeupdates-type-mismatches-rejected#task#4 (UpdateItem, +3).

## Evidence
Wire replay of simparity-wire-aws-v1 against the emulator, expected = recorded AWS response, actual = emulator response.

Cause 1 (status 400 -> 400, key $.Message):
- BatchGetItem, fixture dynamodb-batch-get-item-key-and-table-option-serialization-type-mismatches-rejected step task#4: $.Message value_mismatch — expected 'TRUE_VALUE cannot be converted to String', got 'BOOLEAN_VALUE cannot be converted to String' (cluster also covers task#7, task#8).
- BatchWriteItem, fixture dynamodb-batch-write-item-serialization-type-errors step task#5: same expected/actual pair (cluster also task#6).
- PutItem, fixture dynamodb-put-item-serialization-type-matrix-rejected step task#3: same expected/actual pair (cluster also covers task#4, task#5, task#7, task#8, task#9 of that fixture plus dynamodb-putitem-condition-expression-and-placeholder-maps-wrong-type-rejected#task#4, #task#5 and dynamodb-putitem-item-top-level-wrong-type-variants-rejected#task#5).

Cause 2 (status 400 -> 400, key $.Message):
- DeleteItem, fixture dynamodb-delete-item-expected-value-json-type-matrix-rejected step task#7: $.Message unexpected_present — expected None, got 'Unexpected field type' (cluster also covers dynamodb-delete-item-expression-attribute-wrong-json-type-rejected#task#7, dynamodb-delete-item-request-member-type-mismatches-rejected#task#6, dynamodb-deleteitem-wrong-json-type-for-return-options-and-expressions-rejected#task#11..#task#14).
- Query, fixture dynamodb-query-expression-and-projection-member-wrong-json-type-rejected step task#4: $.Message unexpected_present — expected None, got 'Unexpected field type' (12-step cluster, incl. dynamodb-query-filter-and-key-conditions-attribute-value-list-json-type-variants-rejected#task#4/#task#8 and dynamodb-query-return-consumed-capacity-and-limit-json-type-variants-rejected#task#1..#task#8).
- UpdateItem, fixture dynamodb-update-item-expected-and-attributeupdates-type-mismatches-rejected step task#4: $.Message unexpected_present — expected None, got 'Unexpected field type' (cluster also dynamodb-update-item-string-and-map-member-type-mismatch-rejected#task#2, dynamodb-updateitem-key-expected-attributeupdates-json-type-mismatches-rejected#task#8/#task#11).

In every case the status and __type matched; only the message member diverged.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it, then replay the wire corpus (replay_wire_corpus).
2. Cause 1: POST an awsJson1.0 PutItem (X-Amz-Target DynamoDB_20120810.PutItem) whose TableName is the JSON literal true. AWS answers 400 {'__type':'com.amazon.coral.service#SerializationException','...'} with message 'TRUE_VALUE cannot be converted to String'; the emulator answers 'BOOLEAN_VALUE cannot be converted to String'. Repeat for BatchGetItem (boolean in RequestItems' table-option slot) and BatchWriteItem.
3. Cause 2: POST an awsJson1.0 Query/DeleteItem/UpdateItem body in which a structure- or string-typed member is sent as the wrong JSON type (e.g. ExpressionAttributeValues as a list, Expected as a string). AWS returns the bare carrier body {'__type':'com.amazon.coral.service#SerializationException'}; the emulator adds 'Message':'Unexpected field type'.
4. Re-replay the fixture steps named in trace_ids / Traces to confirm each cause closes.
