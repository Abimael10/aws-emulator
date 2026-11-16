Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table
Issue-Shape-Id: com.amazonaws.cloudcontrol#CloudApiService
Issue-Key: cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table|run#11|AssertionError|assert message == expected, (
Issue-Commit: b8a6145c82ff7150299d1bdeea75ea37edcc92b2

# cloudcontrol: a downstream-handler-rejected CreateResource reports the raw DynamoDB ValidationException text as ProgressEvent.StatusMessage instead of the resource handler's own KeySchema diagnosis

> _Note: this issue was identified against the code base at commit `b8a6145c82ff7150299d1bdeea75ea37edcc92b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourceRequestStatus

## Description
Cause 1 of 1: the StatusMessage a FAILED ProgressEvent carries for a create that the downstream resource handler (not the front-door registry schema) rejects is the owning service's raw ValidationException wording, not the handler's own diagnosis that AWS surfaces through Cloud Control. Failing check: cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table.

AWS: Smithy (services/service-com-amazonaws-cloudcontrol-cloudapiservice) models ProgressEvent.StatusMessage as a StatusMessage string, @length min 0 max 2048, pattern ^[\s\S]*$, documented "Any message explaining the current status", and ProgressEvent.ErrorCode as HandlerErrorCode, documented "For requests with a status of FAILED, the associated error code." The model does not fix the message text, so the requirement is recorded behavior only: for a CreateResource of AWS::DynamoDB::Table whose KeySchema names an attribute that AttributeDefinitions does not declare, the request reaches OperationStatus FAILED with ErrorCode InvalidRequest and StatusMessage "An attribute referenced in a KeySchema element is not defined in AttributeDefinitions". That text is entirely fixed — the AWS::DynamoDB::Table resource handler's own schema diagnosis — and interpolates no attribute name, table name, request token or id from the request or from state. AWS does NOT relay DynamoDB's own CreateTable ValidationException string here; the handler translates the downstream refusal into this sentence before it is recorded on the progress event.

Emulator: the same request does reach FAILED and does carry ErrorCode InvalidRequest (both of those assertions pass), but GetResourceRequestStatus returns StatusMessage "One or more parameter values were invalid: Some index key attributes are not defined in AttributeDefinitions. Keys: [nosuch], AttributeDefinitions: [pk]" — the verbatim DynamoDB CreateTable ValidationException message, with the request's own key-attribute name and declared attribute names interpolated into "Keys: [...]"/"AttributeDefinitions: [...]". The emulator is passing the downstream service error straight through instead of mapping it to the resource handler's diagnosis. ("nosuch" and "pk" in that string are values from the request, not part of any required template; the divergence is the whole sentence, which is the wrong fixed text.)

Why: a client that reads ProgressEvent.StatusMessage to classify or display why a Cloud Control request failed gets a message from a different error vocabulary. Callers that match the handler's documented schema-diagnosis wording — test suites, CloudFormation-CLI contract assertions, and operator tooling that distinguishes "the model references an undeclared attribute" from generic parameter-validation noise — see no match and fall through to an unclassified failure, even though status and error code are correct. It also leaks the owning service's internal message shape through the Cloud Control surface, so a message that AWS keeps stable across DynamoDB wording changes becomes coupled to it here.

Fix: the AWS::DynamoDB::Table resource handler's create path in the cloudcontrol service — specifically the step that converts a downstream service refusal into the terminal ProgressEvent. It must map the downstream rejection to the handler's own diagnosis string when recording StatusMessage, rather than copying the downstream exception message onto the progress event. The FAILED/InvalidRequest mapping already in place is correct and should not change.

Traces: cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table (asserts at the StatusMessage comparison; the preceding OperationStatus and ErrorCode assertions pass, and the later no-table-left-behind and positive-control steps were never reached).

Not covered by any open issue: 20261115T091557Z-cloudcontrol-5-net-new-causes-registry-schema-refusals.md concerns front-door registry-schema refusals that AWS answers synchronously with ValidationException + "Model validation failed", whereas this create is schema-valid and is refused asynchronously by the downstream handler with InvalidRequest, which the emulator already classifies correctly.

## Evidence
Check: cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table
Verdict: mismatch

Request: CreateResource(TypeName="AWS::DynamoDB::Table", DesiredState={"TableName":<name>, "KeySchema":[{"AttributeName":"nosuch","KeyType":"HASH"}], "AttributeDefinitions":[{"AttributeName":"pk","AttributeType":"S"}], "BillingMode":"PAY_PER_REQUEST", "Tags":[...]})
Then: GetResourceRequestStatus(RequestToken=<token from CreateResource>) polled to a terminal OperationStatus.

Agreeing fields (emulator correct):
  ProgressEvent.OperationStatus == "FAILED"
  ProgressEvent.ErrorCode == "InvalidRequest"

Diverging field:
  Expected ProgressEvent.StatusMessage (fixed text, no interpolation):
    "An attribute referenced in a KeySchema element is not defined in AttributeDefinitions"
  Actual ProgressEvent.StatusMessage:
    "One or more parameter values were invalid: Some index key attributes are not defined in AttributeDefinitions. Keys: [nosuch], AttributeDefinitions: [pk]"

Fixed vs variable in the actual message: "One or more parameter values were invalid: Some index key attributes are not defined in AttributeDefinitions. Keys: [...], AttributeDefinitions: [...]" is DynamoDB's fixed CreateTable ValidationException text; "nosuch" is the KeySchema AttributeName from the request and "pk" is the AttributeDefinitions AttributeName from the request. Neither variable part belongs in the expected message, which has no variable parts at all.

Assertion raised by the runner:
  File "check_cloudcontrol_cloudcontrol_downstream_handler_rejected_create_leaves_no_native_table.py", line 101, in run
    assert message == expected
  AssertionError: the StatusMessage does not carry the downstream handler's own diagnosis: 'One or more parameter values were invalid: Some index key attributes are not defined in AttributeDefinitions. Keys: [nosuch], AttributeDefinitions: [pk]'

Model citation: Smithy ProgressEvent.StatusMessage -> StatusMessage, @length(min=0,max=2048), pattern ^[\s\S]*$; ProgressEvent.ErrorCode -> HandlerErrorCode.

## Steps to Reproduce
1. Boot the emulator and create a cloudcontrol client plus a dynamodb client.
2. Call CreateResource with TypeName "AWS::DynamoDB::Table" and a DesiredState whose KeySchema HASH AttributeName is NOT present in AttributeDefinitions, e.g. KeySchema=[{"AttributeName":"nosuch","KeyType":"HASH"}] with AttributeDefinitions=[{"AttributeName":"pk","AttributeType":"S"}], BillingMode "PAY_PER_REQUEST". This DesiredState is valid against the registry schema; only the resource handler can reject it.
3. Take ProgressEvent.RequestToken from the response and poll GetResourceRequestStatus(RequestToken=...) until OperationStatus is terminal (SUCCESS, FAILED or CANCEL_COMPLETE).
4. Observe OperationStatus == "FAILED" and ErrorCode == "InvalidRequest" (both correct).
5. Read ProgressEvent.StatusMessage. Expected exactly "An attribute referenced in a KeySchema element is not defined in AttributeDefinitions"; observed the DynamoDB ValidationException text "One or more parameter values were invalid: Some index key attributes are not defined in AttributeDefinitions. Keys: [nosuch], AttributeDefinitions: [pk]".
6. Re-replay cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table to confirm the fix; the check continues past the StatusMessage assertion into a ListTables/DescribeTable no-leftover-table check and a positive-control create that round-trips an item, so those later steps must also pass.
