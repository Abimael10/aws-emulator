Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sqs.deploy-cfn-queue-behaves sqs.deploy-cfn-stack-delete-removes-queue sqs.op-cancel-unknown-move-task-rejected
Issue-Shape-Id: com.amazonaws.sqs#AmazonSQS
Issue-Key: sqs.deploy-cfn-queue-behaves|run#7|AssertionError|assert resolved_url == queue_url, (
Issue-Key: sqs.deploy-cfn-stack-delete-removes-queue|run#7|AssertionError|assert pre_delete_url == queue_url, (
Issue-Key: sqs.op-cancel-unknown-move-task-rejected|run#18|AssertionError|assert error.get("Code") == "ResourceNotFoundException", (
Issue-Commit: a3e7427fc33f71baf84855dcb1036df3c2b77cc6

# sqs: 2 causes — a CloudFormation-deployed queue's stack Output URL is not the URL SQS serves, and CancelMessageMoveTask on an unknown task id answers InvalidParameterValue instead of ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `a3e7427fc33f71baf84855dcb1036df3c2b77cc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks / CancelMessageMoveTask

## Description
Cause 1 of 2: an AWS::SQS::Queue deployed by CloudFormation publishes its queue URL from a fixed real-AWS pattern (https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>) in the stack Output built from Ref, instead of the QueueUrl SQS returned, so the Output and SQS's GetQueueUrl answer two different strings for one queue.
AWS: Smithy (sqs): CreateQueueResult.QueueUrl / GetQueueUrlResult.QueueUrl is that queue's URL and the exact value every data-plane operation requires in QueueUrl; CloudFormation documents Ref on AWS::SQS::Queue as resolving to the queue's URL. The host is environment-specific and is not the requirement; the invariant is that the two values are the SAME string and name the served endpoint. Recorded behavior only for the concrete host.
Emulator: GetQueueUrl answers the emulator's served https://<host>:<port>/<account>/<name> while the stack Output reports https://sqs.<region>.amazonaws.com/<account>/<name>.
Why: the Output is how a template hands a queue to its consumer; a client that uses it for SendMessage/ReceiveMessage addresses a host nothing answers on, and a post-delete check of the Output's URL proves nothing about the queue.
Fix: the CloudFormation AWS::SQS::Queue resource provider's Ref/attribute resolution (report the QueueUrl CreateQueue returned). Same cause as the already-open 20261121T105340Z-cloudformation-lambda-sqs-an-aws-sqs-queue-stack.md, which is filed under a different service heading and does not carry these checks.
Traces: sqs.deploy-cfn-queue-behaves, sqs.deploy-cfn-stack-delete-removes-queue.

Cause 2 of 2: CancelMessageMoveTask given a well-formed TaskHandle whose taskId was never issued is refused as InvalidParameterValue instead of ResourceNotFoundException, so the refusal is classified as a malformed-input error rather than a missing-resource one.
AWS: Smithy (sqs) CancelMessageMoveTask models exactly these errors: InvalidAddress (404), InvalidSecurity (403), RequestThrottled (403), ResourceNotFoundException (404) and UnsupportedOperation (400, awsQueryError AWS.SimpleQueueService.UnsupportedOperation) — InvalidParameterValue is not among them. A handle that decodes correctly and names an existing source queue ARN but an unissued taskId is a resource that does not exist: HTTP 404 with error code ResourceNotFoundException and the message 'Task does not exist.' (fixed text, no variable part). Cancelling a real RUNNING task in the same run succeeded, so the operation is implemented.
Emulator: Error.Code 'InvalidParameterValue'.
Why: clients branch on the error code — ResourceNotFoundException is the retry-nothing/already-gone signal for a cancel, while InvalidParameterValue reads as a caller bug in the handle encoding; an SDK waiter or redrive tool therefore mis-handles a task that simply does not exist, and the HTTP status differs too (404 vs the 400 InvalidParameterValue carries).
Fix: the CancelMessageMoveTask handler's task lookup/validation (distinguish an undecodable or ineligible handle from a decodable handle naming no task, and raise ResourceNotFoundException with 'Task does not exist.' for the latter).
Traces: sqs.op-cancel-unknown-move-task-rejected.

The open issue 20261109T185037Z-sqs-7-net-new-causes-redrive-move-task-identity.md owns other move-task checks in this service but not this one.

## Evidence
Cause 1 — deploy-cfn-queue-behaves [mismatch]:

    AssertionError: GetQueueUrl 'https://127.0.0.1:37755/000000000000/<queue>' does not match the CFN stack's QueueUrl output 'https://sqs.us-west-2.amazonaws.com/000000000000/<queue>'

  deploy-cfn-stack-delete-removes-queue [mismatch]:

    AssertionError: expected GetQueueUrl to match the stack output, got 'https://127.0.0.1:35285/000000000000/<queue>' vs 'https://sqs.us-west-2.amazonaws.com/000000000000/<queue>'

  Both templates publish Outputs.QueueUrl = {"Ref": "Queue"}. (Hosts, ports, account id and queue names are per-run values; the invariant is the equality.)

Cause 2 — op-cancel-unknown-move-task-rejected [mismatch]:

    AssertionError: CancelMessageMoveTask on a handle naming an unknown task id must fail with ResourceNotFoundException, got 'InvalidParameterValue'

  The handle is base64(json {"taskId": "00000000-0000-0000-0000-000000000000", "sourceArn": <a real DLQ ARN>}); a CancelMessageMoveTask against a genuinely RUNNING task succeeded in the same check and that task's record stayed CANCELLED.
Expected (AWS): HTTP 404, Error.Code 'ResourceNotFoundException', Error.Message 'Task does not exist.'
Actual (emulator): Error.Code 'InvalidParameterValue'.

## Steps to Reproduce
Cause 1: create_stack with an AWS::SQS::Queue (QueueName from a parameter) and Outputs.QueueUrl = !Ref Queue; after CREATE_COMPLETE compare the Output with sqs GetQueueUrl(QueueName). Expected equal; observed the Output carries https://sqs.<region>.amazonaws.com/<account>/<name>. Then SendMessage/ReceiveMessage/DeleteMessage with the Output's URL.
Cause 2: create a DLQ and a destination queue, set the destination's RedrivePolicy to the DLQ, send messages, StartMessageMoveTask and cancel it successfully (positive control). Then call CancelMessageMoveTask with TaskHandle = base64(json {"taskId": "00000000-0000-0000-0000-000000000000", "sourceArn": <DLQ ARN>}). Expected: 404 ResourceNotFoundException 'Task does not exist.'; observed InvalidParameterValue.
