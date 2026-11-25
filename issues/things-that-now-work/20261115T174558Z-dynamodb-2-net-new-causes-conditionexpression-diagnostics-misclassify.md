Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.condition-expression-contains-same-path-and-between-reversed-rejected dynamodb.validation-messages-are-customer-intelligible
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.condition-expression-contains-same-path-and-between-reversed-rejected|run#14|AssertionError|assert message == (
Issue-Key: dynamodb.validation-messages-are-customer-intelligible|run#7|AssertionError|assert message == expected_message, (
Issue-Commit: 599e490a97a559fcc3995e99c1088cb4ddbb72a3

# dynamodb: 2 net-new causes — ConditionExpression diagnostics misclassify a same-path contains() as an operand-type error and an unterminated function call as an arity error

> _Note: this issue was identified against the code base at commit `599e490a97a559fcc3995e99c1088cb4ddbb72a3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateItem

## Description
Two net-new root causes in the DynamoDB condition-expression validator. Each is an emulator defect; the listed checks are examples. BOTH must be closed before submitting. (The open issue 20261103T065250Z-dynamodb-expression-engine-10-root-causes-in-expression.md lists the first check id, but its replay key for that check is the error-CODE assertion — the call is now correctly a ValidationException. The failures below are the message-level residue the suite still reports and are NOT covered by it; neither check is marked OWNED in the replay report.)

Cause 1 of 2: a condition function whose operands are not distinct is diagnosed as an operand-TYPE error instead of a non-distinctness error.
AWS: `contains(path, path)` — the two operands naming the same document path — is rejected while the ConditionExpression is parsed, before any item is examined, with HTTP 400, error code ValidationException and the message `1 validation error detected: Invalid ConditionExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: contains, first operand: [<path>]`. Smithy: UpdateItem.ConditionExpression targets com.amazonaws.dynamodb#ConditionExpression; UpdateItem's modelled errors are ConditionalCheckFailedException (client, 400) and the service-common ValidationException; the model does not carry the message text, so the sentence is recorded behavior only (AWS reference: Expressions.OperatorsAndFunctions). In that message the FIXED text is `1 validation error detected: Invalid ConditionExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: ` ... `, first operand: [` ... `]`; the VARIABLE parts are the member name (ConditionExpression / FilterExpression / KeyConditionExpression as bound by the request), the function name (`contains`) and the first operand's document path, both taken from the request's expression — the `tagset` in the recording only fills that slot.
Emulator: answers ValidationException 400 with `1 validation error detected: Invalid ConditionExpression: Incorrect operand type for operator or function; operator or function: contains, operand type: PATH` — i.e. it reports the operand's KIND as unacceptable rather than reporting that the operands must differ.
Why: a caller (and the SDK/ORM layers that surface the sentence) is told the wrong thing to fix — that a document path may not be passed to contains(), which is false; contains(path, :value) and contains(pathA, pathB) are both legal. Anything branching on the message class cannot distinguish a non-distinctness defect from a genuine type defect.
Fix: the operand-distinctness rule of the condition-expression validator shared by UpdateItem/PutItem/DeleteItem/TransactWriteItems, which must be checked and reported ahead of the operand-type rule.
Traces: dynamodb.condition-expression-contains-same-path-and-between-reversed-rejected.

Cause 2 of 2: an expression that ends in the middle of a function call is reported as a zero-operand call instead of a syntax error.
AWS: a truncated ConditionExpression — a function name and an opening parenthesis with no operand and no closing parenthesis, e.g. `attribute_exists(` — is a PARSE failure, refused with HTTP 400, ValidationException and the message `1 validation error detected: Invalid ConditionExpression: Syntax error; token: "<EOF>", near: "("`. Smithy: PutItem.ConditionExpression targets ConditionExpression; the service-common ValidationException (400) is the error, and the model does not carry the text — recorded behavior only. FIXED text: `1 validation error detected: Invalid ConditionExpression: Syntax error; token: "` ... `", near: "` ... `"`; VARIABLE parts: the member name, the offending token (the literal `<EOF>` marker when input ends early) and the surrounding source fragment, both taken from the request's expression string.
Emulator: answers ValidationException 400 with `1 validation error detected: Invalid ConditionExpression: Incorrect number of operands for operator or function; operator or function: attribute_exists, number of operands: 0` — the parser accepts the unterminated call as a well-formed invocation with zero arguments and then fails it on arity.
Why: unbalanced parentheses and truncated expressions (the commonest way a generated expression breaks) are reported as an arity defect, so a caller is pointed at the function's signature instead of at the position where the expression ran out. A client cannot locate the syntax defect from the message.
Fix: unexpected-end-of-input handling in the expression tokenizer/parser (emit the syntax-error class with the token and near-context), which must run before arity validation in the shared expression validator.
Traces: dynamodb.validation-messages-are-customer-intelligible.

## Evidence
Replayed behavioral suite, service heading '### dynamodb'. Both checks are [mismatch] and neither is marked ALREADY FILED or OWNED.

Cause 1 — check dynamodb.condition-expression-contains-same-path-and-between-reversed-rejected, verbatim from the replay output:
  AssertionError: unexpected message for same-path contains(): '1 validation error detected: Invalid ConditionExpression: Incorrect operand type for operator or function; operator or function: contains, operand type: PATH'
  Expected (the check's assertion, AWS's recorded text): '1 validation error detected: Invalid ConditionExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: contains, first operand: [tagset]'
  Status/code agree (HTTP 400 / ValidationException); only the message diverges. The check also asserts the code is NOT ConditionalCheckFailedException and that the item is unchanged — both hold.

Cause 2 — check dynamodb.validation-messages-are-customer-intelligible, verbatim from the replay output:
  AssertionError: unparseable condition expression: expected the message '1 validation error detected: Invalid ConditionExpression: Syntax error; token: "<EOF>", near: "("', got '1 validation error detected: Invalid ConditionExpression: Incorrect number of operands for operator or function; operator or function: attribute_exists, number of operands: 0'
  The call is PutItem(TableName=<t>, Item={'pk': {'S': 'c'}}, ConditionExpression='attribute_exists('). Status/code agree (HTTP 400 / ValidationException); only the message diverges. The other eight cases in that check (table-name length, 38-significant-digit Number, empty string set, duplicate set member, empty AttributeValue, invalid ReturnValues enum, invalid ReturnConsumedCapacity enum, empty nested attribute name) pass.

## Steps to Reproduce
1. Boot the emulator and point botocore at its dynamodb endpoint (any region/account).
2. CreateTable T with KeySchema [{pk, HASH}], AttributeDefinitions [{pk, S}], BillingMode PAY_PER_REQUEST; wait for ACTIVE.
3. Cause 1: PutItem(T, Item={'pk':{'S':'item'}, 'tagset':{'SS':['a','b']}, 'score':{'N':'5'}}), then UpdateItem(T, Key={'pk':{'S':'item'}}, UpdateExpression='SET score = :nv', ConditionExpression='contains(tagset, tagset)', ExpressionAttributeValues={':nv':{'N':'999'}}). Expect HTTP 400 / ValidationException with Message '1 validation error detected: Invalid ConditionExpression: The first operand must be distinct from the remaining operands for this operator or function; operator: contains, first operand: [tagset]' (the path in the brackets comes from the request). Observed: '... Incorrect operand type for operator or function; operator or function: contains, operand type: PATH'.
4. Cause 2: PutItem(T, Item={'pk':{'S':'c'}}, ConditionExpression='attribute_exists('). Expect HTTP 400 / ValidationException with Message '1 validation error detected: Invalid ConditionExpression: Syntax error; token: "<EOF>", near: "("'. Observed: '... Incorrect number of operands for operator or function; operator or function: attribute_exists, number of operands: 0'.
5. Or re-replay the behavioral suite filtered to the two check ids in trace_ids.
