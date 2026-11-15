Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodbstreams-shard-iterator-request-validation
Issue-Step: task#5
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodbstreams-shard-iterator-request-validation
Issue-Shape-Id: com.amazonaws.dynamodbstreams#DynamoDBStreams_20120810
Issue-Key: dynamodbstreams|GetShardIterator|4xx->4xx|$.message
Issue-Commit: 5d7ae394f187f060d35765f1ed4ed8891185905a

# GetShardIterator ValidationException lists shardIteratorType enum set in the wrong order

> _Note: this issue was identified against the code base at commit `5d7ae394f187f060d35765f1ed4ed8891185905a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetShardIterator

## Description
Cause 1 of 1: DynamoDB Streams GetShardIterator rejects an unmodeled ShardIteratorType value with HTTP 400 and a ValidationException whose message enumerates the permitted enum values, but the emulator enumerates them in a different order than AWS does, so the message text diverges.

AWS: Smithy: GetShardIterator.ShardIteratorType is @required and targets the enum com.amazonaws.dynamodbstreams#ShardIteratorType, whose declared values are [TRIM_HORIZON, LATEST, AT_SEQUENCE_NUMBER, AFTER_SEQUENCE_NUMBER, AT_TIMESTAMP]. The model does not state the wording of the constraint-violation message, so for the message itself this is recorded behavior only: the recording shows HTTP 400 with message "1 validation error detected: Value at 'shardIteratorType' failed to satisfy constraint: Member must satisfy enum value set: [AFTER_SEQUENCE_NUMBER, LATEST, AT_TIMESTAMP, AT_SEQUENCE_NUMBER, TRIM_HORIZON]". The fixed text of that message is "1 validation error detected: Value at '<memberName>' failed to satisfy constraint: Member must satisfy enum value set: [<values>]"; the variable parts are the member name taken from the request-bound member ('shardIteratorType') and, when the offending value is echoed for other member classes, the value from the request. The bracketed value list is NOT per-run data: AWS serializes it in a fixed order for this API that is not the model's declaration order, and that order is part of the message AWS returns for every such request. Hypothesis: AWS's request-validation framework renders the set in its own internal set iteration order rather than model order.

Emulator: returns the correct HTTP status 400 and the correct fixed text, but renders the value list in the Smithy model's declaration order: "1 validation error detected: Value at 'shardIteratorType' failed to satisfy constraint: Member must satisfy enum value set: [TRIM_HORIZON, LATEST, AT_SEQUENCE_NUMBER, AFTER_SEQUENCE_NUMBER, AT_TIMESTAMP]".

Why: a client that asserts on, logs, or snapshot-compares the validation message for an invalid shardIteratorType — including SDK integration tests and contract tests recorded against AWS — sees a message that never occurs on AWS. The enum set order is deterministic on AWS, so a consumer treating the message as stable text (rather than a per-run value) diverges when run against the emulator.

Fix: the GetShardIterator input validation layer for dynamodbstreams — specifically wherever the enum-constraint ValidationException message is rendered from the enum's value list. The rendered order must be the order AWS emits for this member rather than the model's declaration order.

Traces: dynamodbstreams-shard-iterator-request-validation#task#5 (expected vs actual message quoted above). No other cluster in the corpus carries this cause. Note: the other open dynamodbstreams issues (20261029T172902Z-... DescribeStream projection, 20261104T063639Z-... shard iterator token envelope, 20261111T180301Z-... ListStreams Limit maximum) are about different operations/fields and do not cover this message-ordering defect.

## Evidence
Fixture dynamodbstreams-shard-iterator-request-validation, step task#5, operation GetShardIterator, service dynamodbstreams.
Status: expected 400, actual 400 (matches).
Body key $.message: value_mismatch.
Expected (recorded AWS): "1 validation error detected: Value at 'shardIteratorType' failed to satisfy constraint: Member must satisfy enum value set: [AFTER_SEQUENCE_NUMBER, LATEST, AT_TIMESTAMP, AT_SEQUENCE_NUMBER, TRIM_HORIZON]"
Actual (emulator): "1 validation error detected: Value at 'shardIteratorType' failed to satisfy constraint: Member must satisfy enum value set: [TRIM_HORIZON, LATEST, AT_SEQUENCE_NUMBER, AFTER_SEQUENCE_NUMBER, AT_TIMESTAMP]"
Fixed text (identical in both): "1 validation error detected: Value at '<member>' failed to satisfy constraint: Member must satisfy enum value set: [...]". Variable parts: the member name 'shardIteratorType', taken from the request's member binding. The bracketed list is a constant for this member, not per-run data.
Model corroboration (smithy_query, services/service-com-amazonaws-dynamodbstreams-dynamodbstreams-20120810/smithy-model/com-amazonaws-dynamodbstreams-dynamodbstreams-20120810.json): ShardIteratorType enum values are declared [TRIM_HORIZON, LATEST, AT_SEQUENCE_NUMBER, AFTER_SEQUENCE_NUMBER, AT_TIMESTAMP] — exactly the emulator's output order, confirming the emulator emits model order while AWS emits its own fixed order.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture dynamodbstreams-shard-iterator-request-validation.
2. At step task#5, call DynamoDBStreams GetShardIterator with a valid StreamArn and ShardId and a ShardIteratorType that is not a member of the enum (e.g. an unrecognized string).
3. Observe HTTP 400 with a ValidationException whose $.message ends "Member must satisfy enum value set: [TRIM_HORIZON, LATEST, AT_SEQUENCE_NUMBER, AFTER_SEQUENCE_NUMBER, AT_TIMESTAMP]".
4. AWS returns the same status and the same fixed text but with the list ordered [AFTER_SEQUENCE_NUMBER, LATEST, AT_TIMESTAMP, AT_SEQUENCE_NUMBER, TRIM_HORIZON]; compare against the recorded response for that step.
