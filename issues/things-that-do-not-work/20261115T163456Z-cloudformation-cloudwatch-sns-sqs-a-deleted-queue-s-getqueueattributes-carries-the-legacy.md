Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch__sns__sqs.cdk-deploy-alarm-sns-sqs-ok-transition-without-ok-actions-delivers-nothing
Issue-Key: cloudformation__cloudwatch__sns__sqs.cdk-deploy-alarm-sns-sqs-ok-transition-without-ok-actions-delivers-nothing|run#44|AssertionError|assert got == ("AWS.SimpleQueueService.NonExistentQueue", "The specified queue does not exist."), (
Issue-Commit: 4bf99a6da981b1c03b4c2a4da03064374e7befb5

# A deleted queue's GetQueueAttributes carries the legacy query-protocol "for this wsdl version" clause instead of AWS's "The specified queue does not exist."

> _Note: this issue was identified against the code base at commit `4bf99a6da981b1c03b4c2a4da03064374e7befb5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueueAttributes

## Description
Cause 1 of 1: every URL-addressed SQS operation renders the NonExistentQueue refusal with a trailing "for this wsdl version" clause that AWS does not send on the JSON protocol.
AWS: Smithy: QueueDoesNotExist is a client error with httpError 400 and awsQueryError code AWS.SimpleQueueService.NonExistentQueue; the model documents only "Ensure that the QueueUrl is correct and that the queue has not been deleted" and states no message text. Recorded behavior: a GetQueueAttributes against a queue URL that has been deleted is refused with HTTP 400, Error.Code "AWS.SimpleQueueService.NonExistentQueue" and Error.Message "The specified queue does not exist." — that sentence is the COMPLETE fixed text of the refusal; it deliberately names no queue, so there is no variable part. The "… for this wsdl version." wording belongs to the legacy AWS Query/XML SQS endpoint, not to the JSON protocol these SDK clients speak.
Emulator: answers the right code and status but the message "The specified queue does not exist for this wsdl version." The text is a single hard-coded constant used by every URL-addressed lookup, so the divergence is service-wide rather than specific to one operation.
Why: a client that matches the SQS absence refusal by message (the usual way to tell a deleted queue from a transient failure, since the code is shared with other absence conditions) never matches, and any assertion or log scrape built on AWS's sentence diverges. Here it aborts a CloudFormation/CDK alarm→SNS→SQS teardown check before its delivery assertions are reached.
Fix: the SQS queue-resolution refusal message constant shared by every URL-addressed operation (SendMessage, GetQueueAttributes, ReceiveMessage, DeleteQueue, …) — it must be "The specified queue does not exist."; the separate NAME-addressed message used by GetQueueUrl is a different constant and is not in scope here.
Traces: cloudformation__cloudwatch__sns__sqs.cdk-deploy-alarm-sns-sqs-ok-transition-without-ok-actions-delivers-nothing.

## Evidence
cloudformation__cloudwatch__sns__sqs.cdk-deploy-alarm-sns-sqs-ok-transition-without-ok-actions-delivers-nothing:

AssertionError: the deleted queue's GetQueueAttributes gave ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist for this wsdl version.')

Expected (asserted as an exact tuple by the check): ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist.')

The status (400) and the error code are correct; only the message diverges. Source of the constant: services/service-com-amazonaws-sqs-amazonsqs/src/state.rs:3597-3598 — `pub const QUEUE_DOES_NOT_EXIST_MESSAGE: &str = "The specified queue does not exist for this wsdl version.";` with the doc comment at lines 3583-3596 explaining that the closing clause is intended to name the API version. Smithy: com.amazonaws.sqs#QueueDoesNotExist, errorKind client, httpError 400, awsQueryError {code: AWS.SimpleQueueService.NonExistentQueue, httpResponseCode: 400}, single `message` member — the model fixes no text, so the recorded sentence is the requirement.

## Steps to Reproduce
1. sqs:CreateQueue(QueueName=<name>) and keep the returned QueueUrl.
2. sqs:DeleteQueue(QueueUrl=<url>).
3. sqs:GetQueueAttributes(QueueUrl=<url>, AttributeNames=['All']) with a JSON-protocol SDK client.
4. Read Error.Code and Error.Message from the 400 response: the code is AWS.SimpleQueueService.NonExistentQueue and the message must be exactly 'The specified queue does not exist.'; the emulator appends ' for this wsdl version'.
