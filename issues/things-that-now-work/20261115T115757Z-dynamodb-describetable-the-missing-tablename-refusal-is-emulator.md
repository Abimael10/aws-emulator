Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-ltr-describe-table-name-validation-rejected
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-ltr-describe-table-name-validation-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|DescribeTable|4xx->4xx|$.message
Issue-Commit: 015bd153266295538afd2721c5b6fd3d37297a70

# dynamodb DescribeTable: the missing-TableName refusal is emulator-authored instead of AWS's validation-constraint message

> _Note: this issue was identified against the code base at commit `015bd153266295538afd2721c5b6fd3d37297a70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTable

## Description
1 cause.

Cause 1 of 1: DescribeTable called with no TableName is refused with the emulator's own sentence rather than AWS's constraint-violation message.
AWS: Smithy: DescribeTable.input DescribeTableInput.TableName (target TableArn) is @required with @length min 1, max 1024; the operation is awsJson1_0 and a request that omits a required member is refused with ValidationException at HTTP 400. Recorded behavior: the body's message is the standard constraint envelope, "1 validation error detected: Value null at 'tableName' failed to satisfy constraint: Member must not be null". Its FIXED text is "<n> validation error detected: Value null at '<member>' failed to satisfy constraint: Member must not be null"; the variable parts are the number of violations and the member's lowerCamel input path taken from the request shape — here 'tableName' for the omitted TableName member. No value from the request or state appears in it, because the member has no value.
Emulator: answered 400 with the message "The parameter 'TableName' is required but was not present in the request" — AWS's wording is replaced by an emulator-authored sentence that uses the shape's PascalCase member name, states no constraint, and carries no violation count or envelope. The HTTP status and the error code matched; only the message diverges.
Why: the message is the only machine-readable detail AWS gives for a constraint violation. Clients, SDK test suites and CLI users parse it to learn which member failed and why: a harness asserting on "failed to satisfy constraint: Member must not be null", counting "N validation error(s) detected" to find multi-member failures, or extracting the lowerCamel member path to point a user at the right field, all fail against the emulator even though the request was refused for the right reason, so a client's null-parameter handling cannot be exercised here.
Fix: DescribeTable's input validation (the shared required-member check that renders ValidationException messages) — emit AWS's constraint envelope with the violation count and the member's lowerCamel input path instead of a per-operation hand-written sentence.
Traces: dynamodb-ltr-describe-table-name-validation-rejected#task#1.

Related but not covering: open issue 20261111T170613Z-dynamodb-non-constraint-validationexception-messages-are-wrapped-in.md reports the opposite defect (the envelope applied where AWS does not use it), and 20261110T232401Z-dynamodb-validation-and-not-found-refusal-messages-are.md / 20261111T170613Z-dynamodb-control-plane-refusals-use-emulator-authored-text.md cover emulator-authored refusals on other operations; none carries this cluster's replay key.

## Evidence
DescribeTable with TableName omitted, dynamodb-ltr-describe-table-name-validation-rejected#task#1: expected HTTP 400, got HTTP 400 (status and error code agree). $.message expected "1 validation error detected: Value null at 'tableName' failed to satisfy constraint: Member must not be null", got "The parameter 'TableName' is required but was not present in the request".

## Steps to Reproduce
1. Replay dynamodb-ltr-describe-table-name-validation-rejected#task#1: send DescribeTable (awsJson1_0, X-Amz-Target DynamoDB_20120810.DescribeTable) with a body that omits TableName.
2. The reply must be HTTP 400 ValidationException whose message is the constraint envelope "1 validation error detected: Value null at 'tableName' failed to satisfy constraint: Member must not be null" — fixed text with the violation count and the lowerCamel member path filled in from the request shape.
