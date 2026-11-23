Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol__sqs.cloudcontrol-lifecycle-visible-to-native-sqs-api
Issue-Key: cloudcontrol__sqs.cloudcontrol-lifecycle-visible-to-native-sqs-api|run#27|AssertionError|assert refused_code == "InvalidRequest", (
Issue-Commit: 7de16721fc47223af60b1bba7682783f47cdc260

# cloudcontrol__sqs: an UpdateResource patch setting AWS::SQS::Queue VisibilityTimeout above the documented maximum terminates FAILED with HandlerErrorCode GeneralServiceException instead of InvalidRequest

> _Note: this issue was identified against the code base at commit `7de16721fc47223af60b1bba7682783f47cdc260`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateResource

## Description
ONE net-new root cause in the cloudcontrol AWS::SQS::Queue update handler's error classification. A fixer must close this one cause before submitting.

Cause 1 of 1: a Cloud Control UpdateResource whose JSON patch sets VisibilityTimeout to a value above SQS's documented maximum is correctly refused, but the terminal ProgressEvent carries ErrorCode 'GeneralServiceException' where AWS reports the caller-input code 'InvalidRequest'. Failing check: cloudcontrol__sqs.cloudcontrol-lifecycle-visible-to-native-sqs-api.

AWS: Smithy (cloudcontrol, services/service-com-amazonaws-cloudcontrol-cloudapiservice/smithy-model/com-amazonaws-cloudcontrol-cloudapiservice.json) models ProgressEvent.ErrorCode -> HandlerErrorCode, documented "For requests with a status of FAILED, the associated error code" and pointing at the CloudFormation CLI handler error codes. HandlerErrorCode is a closed enum whose members include both values at issue — InvalidRequest (the handler rejected the caller's input) and GeneralServiceException (the downstream service raised something the handler could not classify) — so choosing between them is a contract, not cosmetics. Smithy (sqs) SetQueueAttributesRequest.Attributes documents "VisibilityTimeout – The visibility timeout for the queue, in seconds. Valid values: An integer from 0 to 43,200 (12 hours). Default: 30.", so a patch value above 43200 is a permanent, caller-fixable input error, not a service fault. The concrete code is recorded behavior only: UpdateResource(TypeName="AWS::SQS::Queue", Identifier=<queue url>, PatchDocument=[{"op":"replace","path":"/VisibilityTimeout","value":<above 43200>}]) is accepted synchronously, reaches OperationStatus FAILED with ErrorCode InvalidRequest, and leaves the live queue's VisibilityTimeout at its previous value. The StatusMessage of this class is explicitly NOT a requirement — the check itself notes it embeds the per-call SQS request id and SDK attempt count, which are per-run values.
Emulator: the request does reach FAILED (correct) and the live queue is left unchanged, but the terminal ProgressEvent's ErrorCode is 'GeneralServiceException', so the harness's refusal code is 'GeneralServiceException' where 'InvalidRequest' is expected.
Why: HandlerErrorCode is how a client classifies a failed Cloud Control request without parsing prose. InvalidRequest is permanent and caller-fixable (correct the patch and resubmit); GeneralServiceException reads as an opaque downstream fault and is exactly what retry/back-off layers and IaC error handling treat as possibly transient. A client that submits an out-of-range property is therefore told to retry a request that can never succeed, and CloudFormation-CLI-contract assertions on InvalidRequest never match.
Fix: the error-classification step of the Cloud Control AWS::SQS::Queue update handler — the code that maps a downstream SetQueueAttributes refusal onto the terminal ProgressEvent's ErrorCode — must emit HandlerErrorCode InvalidRequest for a caller-input/validation refusal, reserving GeneralServiceException for downstream faults it cannot classify. The same shared classifier very likely backs the create path.
Traces: cloudcontrol__sqs.cloudcontrol-lifecycle-visible-to-native-sqs-api (steps 1-4 now pass: the created queue's identifier equals sqs GetQueueUrl's QueueUrl, SQS reports VisibilityTimeout 42, a message round-trips, GetResource agrees with SQS on QueueName/VisibilityTimeout/Arn, and the valid patch to 77 is applied to the live queue; it asserts in step 5 on the refusal's code, and the step-5 "a rejected patch must leave the previous value in place" assertion and the step-6 delete/post-delete assertions follow and must also pass).

Related open issues, neither of which covers this cause: 20261121T195413Z-cloudcontrol-3-net-new-causes-downstream-handler-input.md files the SAME misclassification for the cloudcontrol CREATE path (AWS::SQS::Queue DelaySeconds=901 and AWS::DynamoDB::Table), but under the cloudcontrol service and carrying only those checks' replay keys — it says nothing about UpdateResource/VisibilityTimeout or this check; if its classifier fix is shared, re-replay this check to confirm. 20261121T121249Z-cloudcontrol-sqs-the-aws-sqs-queue-handler-keys.md carries this check's EARLIER replay key (the step-1 identifier assertion), which now passes; the check progresses to this later assertion, which that issue does not describe, and the report does not mark this failure owned.

## Evidence
All quotes are from the replayed check (suite simparity-behavioral-aws-v1). The generated queue name, host, port, request token and the SQS request id embedded in StatusMessage are per-run values and are not part of any requirement.

Check cloudcontrol__sqs.cloudcontrol-lifecycle-visible-to-native-sqs-api [mismatch]:
  Setup (passes): cc.create_resource(TypeName="AWS::SQS::Queue", DesiredState={"QueueName": <name>, "VisibilityTimeout": 42, "Tags": [...]}) -> SUCCESS; identifier == sqs.get_queue_url(QueueName=<name>)["QueueUrl"]; sqs.get_queue_attributes reports VisibilityTimeout "42"; send/receive round-trips; cc.get_resource agrees on QueueName, VisibilityTimeout and Arn; cc.update_resource with [{"op":"replace","path":"/VisibilityTimeout","value":77}] -> SUCCESS and SQS reports "77".
  Diverging step: cc.update_resource(TypeName="AWS::SQS::Queue", Identifier=<queue url>, PatchDocument=[{"op":"replace","path":"/VisibilityTimeout","value":999999}]) polled to terminal.
    Agreeing field: the request terminates FAILED and the live queue still reports VisibilityTimeout "77".
    Diverging field: expected ProgressEvent.ErrorCode == "InvalidRequest"; actual "GeneralServiceException".
    Runner assertion: AssertionError: cloudcontrol update <queue> with an out-of-range VisibilityTimeout ended 'GeneralServiceException', expected 'InvalidRequest'
  Model citations: cloudcontrol ProgressEvent.ErrorCode -> HandlerErrorCode, values [NotUpdatable, InvalidRequest, AccessDenied, UnauthorizedTaggingOperation, InvalidCredentials, AlreadyExists, NotFound, ResourceConflict, Throttling, ServiceLimitExceeded, NotStabilized, GeneralServiceException, ServiceInternalError, ServiceTimeout, NetworkFailure, InternalFailure]; sqs SetQueueAttributesRequest.Attributes — VisibilityTimeout "Valid values: An integer from 0 to 43,200 (12 hours). Default: 30." 999999 in the trace is one request value filling that out-of-range template; any value above 43200 must be classified the same way.
  The StatusMessage is not filed: the check records that it embeds the per-call SQS Request ID and SDK attempt count.

## Steps to Reproduce
Boot the emulator and point botocore at it: cc = client("cloudcontrol"), sqs = client("sqs").

1. cc.create_resource(TypeName="AWS::SQS::Queue", DesiredState=json.dumps({"QueueName": q, "VisibilityTimeout": 42})); poll cc.get_resource_request_status(RequestToken=...) to SUCCESS and take ProgressEvent.Identifier (the queue URL).
2. cc.update_resource(TypeName="AWS::SQS::Queue", Identifier=<that URL>, PatchDocument=json.dumps([{"op": "replace", "path": "/VisibilityTimeout", "value": 77}])); poll to SUCCESS and confirm sqs.get_queue_attributes(QueueUrl=<URL>, AttributeNames=["VisibilityTimeout"]) == "77".
3. cc.update_resource(TypeName="AWS::SQS::Queue", Identifier=<that URL>, PatchDocument=json.dumps([{"op": "replace", "path": "/VisibilityTimeout", "value": 999999}])) — 999999 is above the documented maximum of 43200; poll the RequestToken to a terminal OperationStatus.
   Expected: OperationStatus FAILED with ProgressEvent.ErrorCode "InvalidRequest". Observed: FAILED with ErrorCode "GeneralServiceException". Any value above 43200 (and the boundary 43201) must classify as InvalidRequest, while 43200 itself must succeed.
4. Confirm the rest of the check still holds: sqs.get_queue_attributes(...)["Attributes"]["VisibilityTimeout"] is still "77" after the refusal, then cc.delete_resource(TypeName="AWS::SQS::Queue", Identifier=<URL>) reaches SUCCESS and sqs.get_queue_url(QueueName=q) is refused AWS.SimpleQueueService.NonExistentQueue "The specified queue does not exist."
