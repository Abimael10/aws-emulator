Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol.create-resource-access-denied-blocked cloudcontrol.create-resource-missing-permission-denied-rejected
Issue-Shape-Id: com.amazonaws.cloudcontrol#CloudApiService
Issue-Key: cloudcontrol.create-resource-access-denied-blocked|run#15|AssertionError|assert final["ErrorCode"] == "AccessDenied", f"unexpected error code: {final}"
Issue-Key: cloudcontrol.create-resource-missing-permission-denied-rejected|run#19|AssertionError|assert final["ErrorCode"] == "AccessDenied", f"unexpected ErrorCode: {final}"
Issue-Commit: 9ecc7d7834593fb1b99b6e6e6113af3423af306a

# cloudcontrol: a CreateResource whose RoleArn lacks the downstream service permission still terminates FAILED with HandlerErrorCode GeneralServiceException instead of AccessDenied (REOPENS 20261123T182309Z)

> _Note: this issue was identified against the code base at commit `9ecc7d7834593fb1b99b6e6e6113af3423af306a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetResourceRequestStatus

## Description
REOPENS 20261123T182309Z-cloudcontrol-a-createresource-whose-rolearn-lacks-the-downstream.md: its fix left the call refused. That issue is marked fixed and carries the replay keys of exactly the two checks below (Issue-Trace-Ids: cloudcontrol.create-resource-access-denied-blocked cloudcontrol.create-resource-missing-permission-denied-rejected), yet both still fail on the same assertion with the same observed terminal ProgressEvent. This is not a new defect; it is the same root cause un-fixed.

ONE root cause in the cloudcontrol (Cloud Control API) service. Model citations are from services/service-com-amazonaws-cloudcontrol-cloudapiservice/smithy-model/com-amazonaws-cloudcontrol-cloudapiservice.json.

Cause 1 of 1: a CreateResource submitted with a RoleArn whose role grants none of the downstream action (here sqs:CreateQueue) does reach OperationStatus FAILED, but the terminal ProgressEvent classifies the failure as ErrorCode 'GeneralServiceException' instead of 'AccessDenied'. Failing checks: cloudcontrol.create-resource-access-denied-blocked, cloudcontrol.create-resource-missing-permission-denied-rejected.

AWS: Smithy models ProgressEvent.ErrorCode -> HandlerErrorCode, documented "For requests with a status of FAILED, the associated error code" and pointing at the CloudFormation CLI handler error codes. HandlerErrorCode is a closed enum whose members include AccessDenied, InvalidCredentials and GeneralServiceException as DISTINCT values, so which member a given failure carries is part of the resource-handler contract, not prose. ProgressEvent.StatusMessage is StatusMessage, @length(min=0,max=2048), pattern ^[\s\S]*$, "Any message explaining the current status". The model does not fix WHICH member a permission-denied downstream call gets, so for the concrete value the requirement is recorded behavior only: CreateResource(TypeName="AWS::SQS::Queue", DesiredState={"QueueName":<name>}, RoleArn=<a role trusted by cloudformation.amazonaws.com with no attached or inline policies at all>) is accepted synchronously (HTTP 200, OperationStatus PENDING/IN_PROGRESS) and the request terminates OperationStatus="FAILED" with ErrorCode="AccessDenied"; no queue exists afterwards (Cloud Control does NOT fall back to the caller's own credentials), and the identical create reissued after the role is granted sqs:* reaches SUCCESS. The StatusMessage is explicitly NOT a requirement for this class: both checks state it embeds the downstream SQS Request ID, a per-run value, and neither asserts on it. Only the status/ErrorCode pair and the absence of the resource are required.

Emulator: GetResourceRequestStatus on the request token reports the terminal event {'TypeName': 'AWS::SQS::Queue', 'Identifier': '<a served queue URL for the requested QueueName>', 'Operation': 'CREATE', 'OperationStatus': 'FAILED', 'StatusMessage': "Error occurred during operation 'CreateQueue'.", 'ErrorCode': 'GeneralServiceException'}. In that StatusMessage the fixed text is "Error occurred during operation '" + "'." and the only variable part is the downstream operation name the emulator interpolates ('CreateQueue'); the divergence filed here is solely the ErrorCode member (GeneralServiceException where AWS answers AccessDenied). Hypothesis (neither modelled nor asserted, because both checks stop at the ErrorCode assertion): the terminal FAILED event nonetheless carries a concrete queue Identifier of the form <queue-url>/<account-id>/<requested-queue-name>, which suggests the RoleArn is not used to authorize the downstream call at all and the handler then fails for an unrelated, unclassified reason — a fixer must also confirm the checks' next steps, that no queue with that QueueName is visible to sqs:ListQueues / sqs:GetQueueUrl after the denial (cloudcontrol.create-resource-missing-permission-denied-rejected requires GetQueueUrl to answer AWS.SimpleQueueService.NonExistentQueue with the message "The specified queue does not exist.").

Why: HandlerErrorCode is how a client classifies a failed Cloud Control request without parsing prose. AccessDenied is a permanent authorization error (grant the role the missing permission, then resubmit), while GeneralServiceException reads as an opaque downstream fault that retry/back-off layers and IaC error handling treat as possibly transient. A caller whose execution role is under-privileged is therefore told to retry a request that can never succeed until the policy changes, is given no signal that the failure is an IAM one, and any least-privilege test asserting that a missing permission surfaces as AccessDenied never matches. Because this is a permission-enforcement path, a client also cannot distinguish "the role was denied" from "the service glitched" and may conclude its restrictive role was honoured when the served Identifier suggests it was not consulted.

Fix: the error-classification step of the Cloud Control AWS::SQS::Queue CREATE handler in the cloudcontrol service — the code that assumes the request's RoleArn, performs the downstream call under it, and maps the downstream refusal onto the terminal ProgressEvent. It must propagate an authorization denial as HandlerErrorCode AccessDenied (reserving GeneralServiceException for downstream faults it cannot classify) and must not let the request proceed under the caller's own credentials. Note the inverse-direction issues on the same classifier so a fix is not made in one direction only: open 20261125T084851Z-cloudcontrol-a-deleteresource-aws-ssm-parameter-refused-downstream.md requires the AWS::SSM::Parameter DELETE path to answer GeneralServiceException for a downstream authorization refusal, and fixed 20261121T195413Z-cloudcontrol-3-net-new-causes-downstream-handler-input.md covers input refusals mapping to InvalidRequest. Neither covers these two checks' keys.

Traces: cloudcontrol.create-resource-access-denied-blocked (OperationStatus==FAILED passes, then the ErrorCode assertion fails; the no-queue-created check, the caller-credential positive control and the grant-then-reissue restore step are never reached), cloudcontrol.create-resource-missing-permission-denied-rejected (same assertion, same observed terminal event; its NonExistentQueue effect-absent step and positive control are never reached).

## Evidence
All quotes are from the replayed checks (suite simparity-behavioral-aws-v1). Generated role/queue names, hosts, ports, request tokens, account ids and EventTime values are per-run and are not part of the requirement.

cloudcontrol.create-resource-access-denied-blocked [mismatch]:
  Setup: iam.create_role(RoleName=<n>, AssumeRolePolicyDocument=trust for cloudformation.amazonaws.com) with NO attached or inline policies.
  Request: cc.create_resource(TypeName="AWS::SQS::Queue", DesiredState={"QueueName": <q>}, RoleArn=<role arn>) — accepted (HTTP 200); then cc.get_resource_request_status(RequestToken=...) polled to terminal.
  Agreeing field: ProgressEvent.OperationStatus == "FAILED".
  Diverging field: expected ProgressEvent.ErrorCode == "AccessDenied"; actual "GeneralServiceException".
  Observed terminal event: {'TypeName': 'AWS::SQS::Queue', 'Identifier': 'https://<host:port>/000000000000/sim2real-cc-denied-<suffix>', 'RequestToken': <per-run>, 'Operation': 'CREATE', 'OperationStatus': 'FAILED', 'EventTime': <per-run>, 'StatusMessage': "Error occurred during operation 'CreateQueue'.", 'ErrorCode': 'GeneralServiceException'}
  Runner assertion (check_cloudcontrol_create_resource_access_denied_blocked.py line 53):
    assert final["ErrorCode"] == "AccessDenied", f"unexpected error code: {final}"
    -> AssertionError: unexpected error code: {... 'ErrorCode': 'GeneralServiceException'}
  The check explicitly waives the message ("message varies: StatusMessage embeds the downstream SQS Request ID"), so only the ErrorCode member is at issue.

cloudcontrol.create-resource-missing-permission-denied-rejected [mismatch]:
  Same setup and request shape (role with no policies at all, so any downstream sqs:CreateQueue is denied).
  Runner assertion (check_cloudcontrol_create_resource_missing_permission_denied_rejected.py line 85):
    assert final["ErrorCode"] == "AccessDenied", f"unexpected ErrorCode: {final}"
    -> AssertionError: unexpected ErrorCode: {'TypeName': 'AWS::SQS::Queue', 'Identifier': 'https://<host:port>/000000000000/sim2real-behav-ccq-<suffix>', 'Operation': 'CREATE', 'OperationStatus': 'FAILED', 'StatusMessage': "Error occurred during operation 'CreateQueue'.", 'ErrorCode': 'GeneralServiceException'}

Fixed vs variable in the diverging values: the required ErrorCode is the fixed enum member "AccessDenied" — it has no variable part. The emulator's "GeneralServiceException" is a different fixed member of the same closed enum. In the emulator's StatusMessage, "Error occurred during operation '" ... "'." is fixed emulator-authored text and "CreateQueue" is the interpolated downstream operation name; no assertion depends on it.

Model: com.amazonaws.cloudcontrol#ProgressEvent.ErrorCode -> HandlerErrorCode, doc "For requests with a status of FAILED, the associated error code."; HandlerErrorCode values = [NotUpdatable, InvalidRequest, AccessDenied, UnauthorizedTaggingOperation, InvalidCredentials, AlreadyExists, NotFound, ResourceConflict, Throttling, ServiceLimitExceeded, NotStabilized, GeneralServiceException, ServiceInternalError, ServiceTimeout, NetworkFailure, InternalFailure]. ProgressEvent.StatusMessage -> StatusMessage, @length(0,2048), pattern ^[\s\S]*$.

Prior filing: 20261123T182309Z-cloudcontrol-a-createresource-whose-rolearn-lacks-the-downstream.md (status fixed) carries Issue-Trace-Ids "cloudcontrol.create-resource-access-denied-blocked cloudcontrol.create-resource-missing-permission-denied-rejected" and the identical Issue-Keys on the same two assertions. Both checks still mismatch identically, so that fix did not land for this path.

## Steps to Reproduce
Boot the emulator and point botocore at it: iam = client("iam"), cc = client("cloudcontrol"), sqs = client("sqs").

1. role_arn = iam.create_role(RoleName=r, AssumeRolePolicyDocument=json.dumps({"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"cloudformation.amazonaws.com"},"Action":"sts:AssumeRole"}]}))["Role"]["Arn"]. Attach NO inline or managed policies.
2. resp = cc.create_resource(TypeName="AWS::SQS::Queue", DesiredState=json.dumps({"QueueName": q}), RoleArn=role_arn). Retry only on a SYNCHRONOUS InvalidCredentialsException, or an AccessDeniedException whose message mentions being unable to assume the role — that is trust propagation, not the asynchronous failure under test.
3. Poll cc.get_resource_request_status(RequestToken=resp["ProgressEvent"]["RequestToken"]) until OperationStatus is terminal (SUCCESS / FAILED / CANCEL_COMPLETE).
   Expected: OperationStatus "FAILED" with ErrorCode "AccessDenied".
   Observed: OperationStatus "FAILED" with ErrorCode "GeneralServiceException" and StatusMessage "Error occurred during operation 'CreateQueue'.".
4. Confirm the steps the assertion in 3 blocks, which must also pass: sqs.list_queues(QueueNamePrefix=q) returns no QueueUrls and sqs.get_queue_url(QueueName=q) raises AWS.SimpleQueueService.NonExistentQueue with message "The specified queue does not exist." (no fallback to the caller's credentials); cc.create_resource of a different queue with the caller's own credentials (no RoleArn) reaches SUCCESS; then iam.put_role_policy(RoleName=r, PolicyName=p, PolicyDocument=json.dumps({"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"sqs:*","Resource":"*"}]})) and reissue the identical create with RoleArn=role_arn — it must reach SUCCESS and sqs.get_queue_url(QueueName=q) must then resolve.
5. Re-replay cloudcontrol.create-resource-access-denied-blocked and cloudcontrol.create-resource-missing-permission-denied-rejected to confirm the fix.
