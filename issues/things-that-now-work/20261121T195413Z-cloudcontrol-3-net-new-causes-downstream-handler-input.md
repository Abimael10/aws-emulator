Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol.create-resource-out-of-range-property-rejected cloudcontrol.create-resource-out-of-range-value-rejected cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table cloudcontrol.server-side-progress-events-tracked-per-request
Issue-Shape-Id: com.amazonaws.cloudcontrol#CloudApiService
Issue-Key: cloudcontrol.create-resource-out-of-range-property-rejected|run#10|AssertionError|assert final["ErrorCode"] == "InvalidRequest", f"unexpected error code: {final}"
Issue-Key: cloudcontrol.create-resource-out-of-range-value-rejected|run#8|AssertionError|assert bad_final.get("ErrorCode") == "InvalidRequest", f"unexpected ErrorCode: {bad_final}"
Issue-Key: cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table|run#8|AssertionError|assert terminal["ErrorCode"] == "InvalidRequest", (
Issue-Key: cloudcontrol.server-side-progress-events-tracked-per-request|run#22|AssertionError|assert identifier == real_url, f"tracked Identifier {identifier!r} != actual queue URL {real_url!r}"
Issue-Commit: 7feef7c3a108bb63e3851ac69c48ec064beaa56b

# cloudcontrol: 3 net-new causes — downstream-handler input refusals are classified GeneralServiceException instead of HandlerErrorCode InvalidRequest, and a SUCCESS ProgressEvent.Identifier for AWS::SQS::Queue is a synthesized real-AWS-host QueueUrl

> _Note: this issue was identified against the code base at commit `7feef7c3a108bb63e3851ac69c48ec064beaa56b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourceRequestStatus

## Description
THREE net-new root causes, all in the cloudcontrol (Cloud Control API) service. Each is an independent defect; a fixer must close all three before submitting. Model citations are from services/service-com-amazonaws-cloudcontrol-cloudapiservice/smithy-model/com-amazonaws-cloudcontrol-cloudapiservice.json.

Cause 1 of 3: a CreateResource that the downstream SQS handler refuses because a property value is outside its schema-declared range terminates FAILED with ProgressEvent.ErrorCode 'GeneralServiceException' instead of 'InvalidRequest'. Failing checks: cloudcontrol.create-resource-out-of-range-property-rejected, cloudcontrol.create-resource-out-of-range-value-rejected.
AWS: Smithy models ProgressEvent.ErrorCode -> HandlerErrorCode, documented "For requests with a status of FAILED, the associated error code" and pointing at the CloudFormation CLI handler error codes. That enum carries both values at issue: InvalidRequest (the handler rejected the caller's input) and GeneralServiceException (the downstream service returned an exception the handler could not classify) — they are distinct members of one closed vocabulary, so picking between them is a contract, not a cosmetic choice. The model does not fix which value an out-of-range property gets, so the requirement is recorded behavior only: CreateResource(TypeName="AWS::SQS::Queue", DesiredState={"QueueName":<name>,"DelaySeconds":901}) — 901 is above the AWS::SQS::Queue schema's DelaySeconds maximum of 900 — is accepted synchronously (200) and reaches OperationStatus FAILED with ErrorCode InvalidRequest, no queue created. The StatusMessage for this class is not a requirement (both checks note it embeds the downstream SQS request id, a per-run value).
Emulator: the request does reach FAILED (correct) but GetResourceRequestStatus reports ErrorCode 'GeneralServiceException' with StatusMessage 'Invalid value for the parameter DelaySeconds.' — in that message the sentence "Invalid value for the parameter " + <member name> is fixed text with the offending property name ('DelaySeconds') taken from the request; the divergence being filed is the ErrorCode, not that text.
Why: HandlerErrorCode is how a client classifies a failed Cloud Control request without parsing prose. InvalidRequest is a permanent caller error (fix the DesiredState and resubmit); GeneralServiceException reads as an opaque downstream fault and is exactly the code retry/back-off and IaC error-handling treat as possibly transient. A caller that submits a bad property value is therefore told to retry a request that can never succeed, and CloudFormation-CLI-style contract assertions on InvalidRequest never match.
Fix: the error-classification step of the AWS::SQS::Queue create handler in the cloudcontrol service — the code that maps a downstream refusal onto the terminal ProgressEvent's ErrorCode — must emit HandlerErrorCode InvalidRequest when the downstream rejection is a caller-input/validation refusal, reserving GeneralServiceException for downstream faults it cannot classify.
Traces: cloudcontrol.create-resource-out-of-range-property-rejected (asserts at ErrorCode; the no-queue-created and in-range positive-control steps were never reached), cloudcontrol.create-resource-out-of-range-value-rejected (same assertion).

Cause 2 of 3: the same misclassification on the AWS::DynamoDB::Table handler — a schema-valid create that the DynamoDB handler refuses terminates FAILED with ErrorCode 'GeneralServiceException' instead of 'InvalidRequest'. Failing check: cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table.
AWS: same model citation (ProgressEvent.ErrorCode -> HandlerErrorCode, members InvalidRequest and GeneralServiceException); recorded behavior only for the concrete value: CreateResource(TypeName="AWS::DynamoDB::Table") whose KeySchema HASH AttributeName is not declared in AttributeDefinitions passes the front-door registry schema, is refused by the resource handler, and ends FAILED with ErrorCode InvalidRequest and StatusMessage "An attribute referenced in a KeySchema element is not defined in AttributeDefinitions" (entirely fixed text, no interpolation), leaving no table behind.
Emulator: ErrorCode is 'GeneralServiceException'. Note this is a REGRESSION against the behavior recorded in open issue 20261116T000321Z-cloudcontrol-a-downstream-handler-rejected-createresource-reports-the.md, which filed only the StatusMessage of this very check and records that OperationStatus FAILED and ErrorCode InvalidRequest both passed at that time; the check now fails one assertion EARLIER, on ErrorCode, so that issue's StatusMessage cause is no longer even reached and is not re-filed here. Hypothesis: the same shared downstream-refusal-to-ErrorCode mapping backs Cause 1 and Cause 2, in which case one fix closes both — but they are two handlers and two independent observations, so both must be verified.
Why: as in Cause 1 — a permanent, caller-fixable model error is reported in the vocabulary of an unclassified service fault, so clients retry instead of failing fast, and the handler-contract distinction between "your model is wrong" and "the service misbehaved" is lost.
Fix: the AWS::DynamoDB::Table create handler's downstream-refusal path in the cloudcontrol service (the step that records the terminal ProgressEvent), or the shared classifier it calls into.
Traces: cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table (asserts at ErrorCode; the StatusMessage, no-leftover-table and positive-control steps follow and must also pass).

Cause 3 of 3: the SUCCESS ProgressEvent of a CreateResource(AWS::SQS::Queue) carries a synthesized https://sqs.<region>.amazonaws.com/<account-id>/<queue-name> as Identifier instead of the QueueUrl the served SQS endpoint hands out for that queue. Failing check: cloudcontrol.server-side-progress-events-tracked-per-request.
AWS: Smithy models ProgressEvent.Identifier as "The primary identifier for the resource" (@length 1-1024, pattern ^.+$); Cloud Control's primary identifier for AWS::SQS::Queue is the queue's QueueUrl, and sqs CreateQueueResult.QueueUrl / GetQueueUrlResult.QueueUrl is the exact string every SQS operation on that queue requires. The host, port and account are environment-specific and are NOT the requirement; the invariant is that the Identifier Cloud Control reports for a queue it created and the QueueUrl SQS reports for the same QueueName are the SAME string, addressable at the endpoint being talked to. Recorded behavior only for any concrete URL.
Emulator: the terminal ProgressEvent reports Identifier 'https://sqs.us-west-2.amazonaws.com/<account>/<queue-name>' (a real-AWS host that this deployment does not serve) while sqs GetQueueUrl(QueueName=<same name>) answers the emulator's own served listener URL for the same queue — the two services do not name the same resource.
Why: the Identifier is the only handle CreateResource returns. Used against SQS it addresses an unserved host, so every data-plane call built from it fails, and every later GetResource/UpdateResource/DeleteResource keyed on the URL SQS actually hands a client misses.
Fix: the identifier resolution of the Cloud Control AWS::SQS::Queue create handler — take the QueueUrl the downstream CreateQueue call returned and record it on the ProgressEvent, instead of formatting a real-AWS QueueUrl from region/account/name.
Traces: cloudcontrol.server-side-progress-events-tracked-per-request (all earlier assertions — asynchronous non-terminal initial status, constant RequestToken/TypeName/Operation across polls, terminal SUCCESS with a non-empty Identifier — pass; it asserts on identifier == sqs.get_queue_url(...)['QueueUrl']).
Open issue 20261121T121249Z-cloudcontrol-sqs-the-aws-sqs-queue-handler-keys.md describes the same synthesized-identifier behavior for the cloudcontrol__sqs service, but it is filed under that service and carries only the three cloudcontrol__sqs checks' replay keys; this cloudcontrol check is not covered by it and the report does not mark it owned. If that issue's Cause 1 is fixed at the handler, this check should be re-replayed to confirm it too.

## Evidence
All quotes are from the replayed checks (suite simparity-behavioral-aws-v1). Generated queue/table names, hosts, ports, request tokens and EventTime values are per-run and are not part of any requirement.

Cause 1 — cloudcontrol.create-resource-out-of-range-property-rejected [mismatch]:
  Request: CreateResource(TypeName="AWS::SQS::Queue", DesiredState={"QueueName":<name>,"DelaySeconds":901}); then GetResourceRequestStatus(RequestToken=...) polled to terminal.
  Agreeing field: ProgressEvent.OperationStatus == "FAILED".
  Diverging field: expected ProgressEvent.ErrorCode == "InvalidRequest"; actual "GeneralServiceException".
  Full terminal event observed: {'TypeName': 'AWS::SQS::Queue', 'Identifier': '<a queue url>', 'RequestToken': <token>, 'Operation': 'CREATE', 'OperationStatus': 'FAILED', 'StatusMessage': 'Invalid value for the parameter DelaySeconds.', 'ErrorCode': 'GeneralServiceException'}
  Runner assertion: assert final["ErrorCode"] == "InvalidRequest" -> AssertionError: unexpected error code.
  cloudcontrol.create-resource-out-of-range-value-rejected [mismatch] fails identically: assert bad_final.get("ErrorCode") == "InvalidRequest" -> 'GeneralServiceException', same StatusMessage.
  In 'Invalid value for the parameter DelaySeconds.' the fixed part is "Invalid value for the parameter " + "." and 'DelaySeconds' is the property name from the request; the filed divergence is the ErrorCode member, not this text.

Cause 2 — cloudcontrol.cloudcontrol-downstream-handler-rejected-create-leaves-no-native-table [mismatch]:
  Request: CreateResource(TypeName="AWS::DynamoDB::Table", DesiredState={"TableName":<name>,"KeySchema":[{"AttributeName":"nosuch","KeyType":"HASH"}],"AttributeDefinitions":[{"AttributeName":"pk","AttributeType":"S"}],"BillingMode":"PAY_PER_REQUEST","Tags":[...]}); then GetResourceRequestStatus polled to terminal.
  Agreeing field: OperationStatus == "FAILED".
  Diverging field: expected ErrorCode "InvalidRequest"; actual "GeneralServiceException".
  Runner assertion: AssertionError: the refusal carried ErrorCode 'GeneralServiceException', expected 'InvalidRequest'.
  ('nosuch' and 'pk' are request values. The expected StatusMessage for this class, "An attribute referenced in a KeySchema element is not defined in AttributeDefinitions", is already filed as 20261116T000321Z-...; the check no longer reaches that assertion.)

Cause 3 — cloudcontrol.server-side-progress-events-tracked-per-request [mismatch]:
  Request: CreateResource(TypeName="AWS::SQS::Queue", DesiredState={"QueueName":<name>,"Tags":[...]}); polled to SUCCESS; then sqs GetQueueUrl(QueueName=<same name>).
  Runner assertion: AssertionError: tracked Identifier 'https://sqs.us-west-2.amazonaws.com/000000000000/<name>' != actual queue URL '<the emulator's served listener>/000000000000/<name>'.
  Expected (AWS): the two strings are equal — ProgressEvent.Identifier is the QueueUrl the owning service serves for that queue. Actual: Cloud Control reports a real-AWS-host URL the deployment does not serve.

## Steps to Reproduce
Boot the emulator and point botocore at it: cc = client("cloudcontrol"), sqs = client("sqs"), ddb = client("dynamodb").

Cause 1: cc.create_resource(TypeName="AWS::SQS::Queue", DesiredState=json.dumps({"QueueName": n, "DelaySeconds": 901})); take ProgressEvent.RequestToken and poll cc.get_resource_request_status(RequestToken=...) until OperationStatus is terminal. Expected FAILED with ErrorCode "InvalidRequest"; observed FAILED with ErrorCode "GeneralServiceException". Then confirm the rest of the check: sqs.list_queues(QueueNamePrefix=n) must be empty, and the same create with DelaySeconds=5 (or the boundary 900) must reach SUCCESS and round-trip that value through cc.get_resource / sqs.get_queue_attributes.

Cause 2: cc.create_resource(TypeName="AWS::DynamoDB::Table", DesiredState=json.dumps({"TableName": t, "KeySchema": [{"AttributeName": "nosuch", "KeyType": "HASH"}], "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}], "BillingMode": "PAY_PER_REQUEST"})) — valid against the registry schema, refusable only by the handler. Poll the request to terminal: expected FAILED with ErrorCode "InvalidRequest"; observed "GeneralServiceException". (The StatusMessage assertion that follows is covered by open issue 20261116T000321Z-....)

Cause 3: cc.create_resource(TypeName="AWS::SQS::Queue", DesiredState=json.dumps({"QueueName": n, "Tags": [{"Key": "k", "Value": "v"}]})); poll to SUCCESS; compare the terminal ProgressEvent.Identifier with sqs.get_queue_url(QueueName=n)["QueueUrl"]. Expected: identical strings, addressable at the served endpoint. Observed: Cloud Control reports https://sqs.<region>.amazonaws.com/<account>/<n> while SQS serves a different URL for the same queue.
