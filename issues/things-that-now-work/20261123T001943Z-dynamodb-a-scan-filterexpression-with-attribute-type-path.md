Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.scan-filter-expression-non-distinct-function-nested-path-operands-rejected
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.scan-filter-expression-non-distinct-function-nested-path-operands-rejected|expect_error#8|AssertionError|assert got_message == message, f"unexpected message {got_message!r}"
Issue-Commit: 5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0

# dynamodb: a Scan FilterExpression with attribute_type(path, same-path) is refused with the "Incorrect operand type" message instead of AWS's "first operand must be distinct" message

> _Note: this issue was identified against the code base at commit `5b88798ccff0ec7f765809a75ef2c7dfb1d7aad0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Scan

## Description
Cause 1 of 1: the parse-time distinct-operand rule is applied to begins_with but not to attribute_type, so attribute_type falls through to the operand-type check and the client is told the wrong thing about its expression. Failing check: dynamodb.scan-filter-expression-non-distinct-function-nested-path-operands-rejected.

AWS: Scan models FilterExpression -> ConditionExpression (Smithy: dynamodb Scan, input FilterExpression: ConditionExpression, ExpressionAttributeNames: ExpressionAttributeNameMap; Scan's modeled errors are InternalServerError/ProvisionedThroughputExceededException/RequestLimitExceeded/ResourceNotFoundException/ThrottlingException/InvalidEndpointException, so the parse-time refusal is the service-common ValidationException, HTTP 400). The message text is not modeled - recorded behavior only, and the rule is documented under Expressions.OperatorsAndFunctions: when a function's first operand is not distinct from its remaining operands the call is refused at parse time, before any item is read, with ValidationException and the message
  "Invalid FilterExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: <function>, first operand: [<path segments>]"
Fixed text: "Invalid <ExpressionName>: The first operand must be distinct from the remaining operands for this operator or function; operator: ", then ", first operand: ". Variable parts taken from the request: the expression member name (FilterExpression here), the function name as written (begins_with, attribute_type), and the resolved document path rendered as its bracketed segment list - resolved AFTER ExpressionAttributeNames substitution, so a bare path and an aliased path produce the same segment list. The distinctness check precedes operand-type checking, which is why a path-typed second operand never surfaces as an operand-type error for these functions.

Emulator: begins_with is refused with the correct code and message (that leg of the check passes, including the aliased #i.#l form), but attribute_type with the identical nested path as both operands is refused with ValidationException and the message 'Invalid FilterExpression: Incorrect operand type for operator or function; operator or function: attribute_type, operand type: PATH' instead of 'Invalid FilterExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: attribute_type, first operand: [info, label]'. So the emulator's attribute_type argument validation checks operand type first (and rejects a path where it wants a type-literal) rather than applying the distinct-operand rule the way begins_with does.

Why: a client parsing or surfacing the refusal is told its second operand has the wrong type - pointing at the type literal - when the actual defect AWS reports is that both operands name the same document path. SDK/user error handling and any test asserting AWS's wording diverges, and the two functions disagree with each other inside one emulator.

Fix: the DynamoDB expression parser's function-argument validation (the condition/filter expression module shared by Scan, Query, PutItem/UpdateItem/DeleteItem condition expressions): run the distinct-operand check for attribute_type on the same footing as begins_with, ahead of the operand-type check, and format the message with the post-substitution path segment list.

Traces: dynamodb.scan-filter-expression-non-distinct-function-nested-path-operands-rejected. Nearest open issue is 20261117T172826Z-dynamodb-expression-evaluation-10-net-new-causes-transaction.md (expression evaluation, incl. update-item-nested-function-operand-rejected and scan-filter-size-counts-utf16-code-units-not-bytes); it does not carry this replay key or the distinct-operand rule.

## Evidence
Check: dynamodb.scan-filter-expression-non-distinct-function-nested-path-operands-rejected
Setup: PAY_PER_REQUEST table, hash key pk (S); item {pk: "item1", info: {M: {label: {S: "hello"}}}}.

Leg 1 (passes): Scan FilterExpression="begins_with(info.label, info.label)" ->
  ValidationException "Invalid FilterExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: begins_with, first operand: [info, label]"

Leg 2 (FAILS): Scan FilterExpression="attribute_type(info.label, info.label)"
  expected (AWS): ValidationException, HTTP 400, message
    "Invalid FilterExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: attribute_type, first operand: [info, label]"
  actual (emulator): ValidationException with message
    "Invalid FilterExpression: Incorrect operand type for operator or function; operator or function: attribute_type, operand type: PATH"
  harness: AssertionError: unexpected message 'Invalid FilterExpression: Incorrect operand type for operator or function; operator or function: attribute_type, operand type: PATH' (check_dynamodb_scan_filter_expression_non_distinct_function_nested_path_operands_rejected.py line 37, called from line 71)

The error code (ValidationException) matches; only the message diverges. The segment list [info, label] is the request's resolved path, not a literal from one run.

## Steps to Reproduce
1. dynamodb.create_table(TableName=<t>, KeySchema=[{pk,HASH}], AttributeDefinitions=[{pk,S}], BillingMode="PAY_PER_REQUEST"); wait for ACTIVE.
2. dynamodb.put_item(TableName=<t>, Item={"pk":{"S":"item1"},"info":{"M":{"label":{"S":"hello"}}}}).
3. dynamodb.scan(TableName=<t>, ConsistentRead=True, FilterExpression="attribute_type(info.label, info.label)").
   Observed: ValidationException "Invalid FilterExpression: Incorrect operand type for operator or function; operator or function: attribute_type, operand type: PATH".
   Expected: ValidationException "Invalid FilterExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: attribute_type, first operand: [info, label]".
4. Repeat with FilterExpression="begins_with(info.label, info.label)" to see the correct message, and with ExpressionAttributeNames={"#i":"info","#l":"label"} / "attribute_type(#i.#l, #i.#l)" to confirm the aliased form must produce the same post-substitution segment list.
5. Controls that must still evaluate: scan with FilterExpression="attribute_type(info.label, :t)" / ExpressionAttributeValues={":t":{"S":"S"}} and "begins_with(info.label, :p)" / {":p":{"S":"hel"}} each return Count==1 with pk=item1.
