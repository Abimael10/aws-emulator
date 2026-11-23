Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol.create-resource-access-denied-blocked cloudcontrol.create-resource-missing-permission-denied-rejected
Issue-Shape-Id: com.amazonaws.cloudcontrol#CloudApiService
Issue-Key: cloudcontrol.create-resource-access-denied-blocked|run#15|AssertionError|assert final["ErrorCode"] == "AccessDenied", f"unexpected error code: {final}"
Issue-Key: cloudcontrol.create-resource-missing-permission-denied-rejected|run#19|AssertionError|assert final["ErrorCode"] == "AccessDenied", f"unexpected ErrorCode: {final}"
Issue-Commit: 33ea92ed4461881d8f7ae2a85609594a4bd6a1fc

# cloudcontrol: a CreateResource whose RoleArn lacks the downstream service permission terminates FAILED with HandlerErrorCode GeneralServiceException instead of AccessDenied

> _Note: this issue was identified against the code base at commit `33ea92ed4461881d8f7ae2a85609594a4bd6a1fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourceRequestStatus

## Description
ONE net-new root cause in the cloudcontrol (Cloud Control API) service. Model citations are from services/service-com-amazonaws-cloudcontrol-cloudapiservice/smithy-model/com-amazonaws-cloudcontrol-cloudapiservice.json.

Cause 1 of 1: a CreateResource submitted with a RoleArn whose role carries no policy granting the downstream action (here sqs:CreateQueue) does reach OperationStatus FAILED, but the terminal ProgressEvent classifies the failure as ErrorCode 'GeneralServiceException' instead of 'AccessDenied'. Failing checks: cloudcontrol.create-resource-access-denied-blocked, cloudcontrol.create-resource-missing-permission-denied-rejected.

AWS: Smithy models ProgressEvent.ErrorCode -> HandlerErrorCode, documented "For requests with a status of FAILED, the associated error code" and pointing at the CloudFormation CLI handler error codes. HandlerErrorCode is a closed enum whose members include AccessDenied, InvalidCredentials and GeneralServiceException as DISTINCT values, so which one a failure carries is part of the contract, not prose. The model does not fix which member a permission-denied downstream call gets, so for the concrete value the requirement is recorded behavior only: CreateResource(TypeName="AWS::SQS::Queue", DesiredState={"QueueName":<name>}, RoleArn=<role trusted by cloudformation.amazonaws.com with no attached or inline policies>) is accepted synchronously (HTTP 200, OperationStatus PENDING/IN_PROGRESS) and the request terminates OperationStatus="FAILED" with ErrorCode="AccessDenied"; no queue exists afterwards (Cloud Control does NOT fall back to the caller's own credentials), and the identical create reissued after the role is granted sqs:* reaches SUCCESS. The StatusMessage of this class is NOT a requirement here: both checks state it embeds the downstream SQS Request ID, a per-run value, and neither asserts on it; only the status/ErrorCode pair and the absence of the resource are.

Emulator: GetResourceRequestStatus on the request token reports the terminal event {'TypeName': 'AWS::SQS::Queue', 'Identifier': '<a served queue URL for the requested QueueName>', 'Operation': 'CREATE', 'OperationStatus': 'FAILED', 'StatusMessage': "Error occurred during operation 'CreateQueue'.", 'ErrorCode': 'GeneralServiceException'}. In that StatusMessage the fixed text is "Error occurred during operation '" + "'." with the downstream operation name ('CreateQueue') interpolated; the divergence filed is the ErrorCode member. Hypothesis (neither modelled nor asserted, because both checks stop at the ErrorCode assertion): the terminal FAILED event nonetheless carries a concrete queue Identifier, which suggests the RoleArn is not used to authorize the downstream call at all and the handler fails for an unrelated, unclassified reason — a fixer should also confirm the check's next step, that no queue with that QueueName is visible to sqs:ListQueues/GetQueueUrl after the denial.

Why: HandlerErrorCode is how a client classifies a failed Cloud Control request without parsing prose. AccessDenied is a permanent authorization error (grant the role the missing permission, then resubmit), while GeneralServiceException reads as an opaque downstream fault that retry/back-off layers and IaC error handling treat as possibly transient. A caller whose execution role is under-privileged is therefore told to retry a request that can never succeed until the policy changes, is given no signal that the failure is an IAM one, and any least-privilege test that asserts a missing permission surfaces as AccessDenied never matches. Worse, because this is a permission-enforcement path, a client cannot distinguish "the role was denied" from "the service glitched" and may conclude its restrictive role was honoured when it was not.

Fix: the error-classification step of the Cloud Control AWS::SQS::Queue create handler in the cloudcontrol service — the code that assumes the request's RoleArn, performs the downstream call under it and maps the downstream refusal onto the terminal ProgressEvent — must propagate an authorization denial as HandlerErrorCode AccessDenied (reserving GeneralServiceException for downstream faults it cannot classify) and must not let the request proceed under the caller's own credentials.

Traces: cloudcontrol.create-resource-access-denied-blocked (asserts OperationStatus FAILED, which passes, then at ErrorCode; the no-queue-created check, the caller-credential positive control and the grant-then-reissue restore step were never reached), cloudcontrol.create-resource-missing-permission-denied-rejected (same assertion, same observed terminal event).

## Evidence
All quotes are from the replayed checks (suite simparity-behavioral-aws-v1). Generated role/queue names, hosts, ports, request tokens and EventTime values are per-run and are not part of the requirement.

cloudcontrol.create-resource-access-denied-blocked [mismatch]:
  Setup: iam.create_role(RoleName=<n>, AssumeRolePolicyDocument=trust for cloudformation.amazonaws.com) with NO attached or inline policies.
  Request: cc.create_resource(TypeName="AWS::SQS::Queue", DesiredState={"QueueName": <q>}, RoleArn=<role arn>) — accepted; then cc.get_resource_request_status(RequestToken=...) polled to terminal.
  Agreeing field: ProgressEvent.OperationStatus == "FAILED".
  Diverging field: expected ProgressEvent.ErrorCode == "AccessDenied"; actual "GeneralServiceException".
  Full terminal event observed: {'TypeName': 'AWS::SQS::Queue', 'Identifier': '<served queue url>/000000000000/<q>', 'RequestToken': <token>, 'Operation': 'CREATE', 'OperationStatus': 'FAILED', 'EventTime': <per-run>, 'StatusMessage': "Error occurred during operation 'CreateQueue'.", 'ErrorCode': 'GeneralServiceException'}
  Runner assertion: assert final["ErrorCode"] == "AccessDenied" -> AssertionError: unexpected error code.
  The check explicitly waives the message ("message varies: StatusMessage embeds the downstream SQS Request ID").

cloudcontrol.create-resource-missing-permission-denied-rejected [mismatch]:
  Same setup and request shape (role with no policies at all, so any downstream sqs:CreateQueue is denied).
  Runner assertion: assert final["ErrorCode"] == "AccessDenied" -> AssertionError: unexpected ErrorCode, with the terminal event {'TypeName': 'AWS::SQS::Queue', 'Identifier': '<served queue url>/000000000000/<q>', 'Operation': 'CREATE', 'OperationStatus': 'FAILED', 'StatusMessage': "Error occurred during operation 'CreateQueue'.", 'ErrorCode': 'GeneralServiceException'}.

Related but NOT covering issues: open 20261121T195413Z-cloudcontrol-3-net-new-causes-downstream-handler-input.md files the same GeneralServiceException-instead-of-InvalidRequest misclassification for schema/input refusals (DelaySeconds out of range, DynamoDB KeySchema), and the report does not mark either check above as owned by it. These two checks are an IAM/RoleArn authorization path, expect a different enum member (AccessDenied), and are not carried by that issue's replay keys — if its shared classifier is touched, re-replay these two as well.

## Steps to Reproduce
Boot the emulator and point botocore at it: iam = client("iam"), cc = client("cloudcontrol"), sqs = client("sqs").

1. role_arn = iam.create_role(RoleName=r, AssumeRolePolicyDocument=json.dumps({"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"cloudformation.amazonaws.com"},"Action":"sts:AssumeRole"}]}))["Role"]["Arn"]. Attach NO policies.
2. resp = cc.create_resource(TypeName="AWS::SQS::Queue", DesiredState=json.dumps({"QueueName": q}), RoleArn=role_arn). (Retry only on a synchronous InvalidCredentialsException / AccessDeniedException whose message mentions assuming the role, which is trust propagation, not the asynchronous failure under test.)
3. Poll cc.get_resource_request_status(RequestToken=resp["ProgressEvent"]["RequestToken"]) until OperationStatus is terminal.
   Expected: OperationStatus "FAILED" with ErrorCode "AccessDenied". Observed: OperationStatus "FAILED" with ErrorCode "GeneralServiceException" and StatusMessage "Error occurred during operation 'CreateQueue'.".
4. Confirm the rest of the check once the code is correct: sqs.list_queues(QueueNamePrefix=q) must be empty (no fallback to the caller's credentials); cc.create_resource of a different queue with the caller's own credentials (no RoleArn) must reach SUCCESS; then iam.put_role_policy(RoleName=r, PolicyName=p, PolicyDocument=json.dumps({"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"sqs:*","Resource":"*"}]})) and reissue the identical create with RoleArn=role_arn — it must reach SUCCESS and sqs.get_queue_url(QueueName=q) must then resolve.
