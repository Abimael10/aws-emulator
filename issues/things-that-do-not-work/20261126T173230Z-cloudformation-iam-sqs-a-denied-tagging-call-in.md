Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__sqs.sqs-queue-handler-permission-update-denied-then-delete-denied-recovers
Issue-Key: cloudformation__iam__sqs.sqs-queue-handler-permission-update-denied-then-delete-denied-recovers|run#43|AssertionError|assert _mask(reason, ctx.account) == expected_reason, f"the queue update failed with {reason!r}"
Issue-Commit: 909b19ea020eb5a0e53f1bf3e57b93e5193187d7

# cloudformation__iam__sqs: a denied tagging call in the AWS::SQS::Queue handler is reported as bare AccessDenied, without CloudFormation's tagging-permission wrapper or HandlerErrorCode UnauthorizedTaggingOperation

> _Note: this issue was identified against the code base at commit `909b19ea020eb5a0e53f1bf3e57b93e5193187d7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
1 net-new cause behind the cloudformation__iam__sqs failure.

Cause 1 of 1: when a stack operation run under a RoleARN is denied the tagging call a resource handler needs (here sqs:TagQueue on an in-place AWS::SQS::Queue property update), CloudFormation reports the failure with its tagging-specific wrapper and the UnauthorizedTaggingOperation handler error code; the emulator reports the raw denial as an ordinary AccessDenied.

AWS: recorded behavior only — the ResourceStatusReason text and HandlerErrorCode of a stack event are not in the Smithy model (cloudformation DescribeStackEvents returns StackEvent.ResourceStatusReason as a free string). AWS distinguishes a permissions failure of a tagging call from any other AccessDenied: the UPDATE_FAILED event's ResourceStatusReason is the handler message NESTED inside a tagging-guidance wrapper and the trailing parenthetical names HandlerErrorCode: UnauthorizedTaggingOperation. Its shape, with the fixed text verbatim and the variable parts taken from the request/state, is:
Resource handler returned message: "Encountered a permissions error performing a tagging operation, please add required tag permissions. See https://repost.aws/knowledge-center/cloudformation-tagging-permission-error for how to resolve. Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<role-name>/<session-name> is not authorized to perform: sqs:tagqueue on resource: arn:aws:sqs:<region>:<account>:<queue-name> because no identity-based policy allows the sqs:tagqueue action (Service: Sqs, Status Code: 403, Request ID: <request-id>) (SDK Attempt Count: <n>)"" (RequestToken: <token>, HandlerErrorCode: UnauthorizedTaggingOperation)
Fixed text: "Resource handler returned message:", the tagging-guidance sentence and its repost.aws URL, "is not authorized to perform:", "on resource:", "because no identity-based policy allows the ... action", "(Service: Sqs, Status Code: 403, Request ID: ...)", "(SDK Attempt Count: ...)" and "(RequestToken: ..., HandlerErrorCode: UnauthorizedTaggingOperation)". Variable parts: the account, the assumed-role name and session name of the stack role, the lower-cased action (sqs:tagqueue), the queue ARN from the template/state, the per-call request id, attempt count and request token. Note the DOUBLED closing quote: the inner handler message is quoted inside the outer wrapper.
Emulator: the same event carries only the inner denial, unwrapped, and the wrong handler error code —
Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<role-name>/AWSCloudFormation is not authorized to perform: sqs:tagqueue on resource: arn:aws:sqs:<region>:<account>:<queue-name> because no identity-based policy allows the sqs:tagqueue action (Service: Sqs, Status Code: 403, Request ID: <id>) (SDK Attempt Count: 1)" (RequestToken: <id>, HandlerErrorCode: AccessDenied)
The stack status (UPDATE_ROLLBACK_COMPLETE) and the decisive event subsequence (Queue UPDATE_FAILED, Queue UPDATE_COMPLETE, stack UPDATE_ROLLBACK_COMPLETE) do match; only the reason text and the handler error code diverge, so the check fails on the reason assertion before it reaches the DELETE-denial leg (sqs:DeleteQueue, HandlerErrorCode: AccessDenied) — that leg is unverified, not known-good.
Why: tooling and operators branch on the handler error code and on this wrapper. A deploy pipeline that retries or reports "add the missing tag permissions" when it sees UnauthorizedTaggingOperation (and the repost.aws remediation link) takes the generic AccessDenied path instead, so the one class of denial AWS deliberately singles out as fixable-by-adding-tag-permissions becomes indistinguishable from any other authorization failure.
Fix: the stack-operation failure path of CloudFormation's AWS::SQS::Queue resource handler — where a denied downstream SQS call is turned into a resource-level failure reason and HandlerErrorCode. The denial raised by a tagging call (TagQueue/UntagQueue) must be wrapped in the tagging-guidance message and classified UnauthorizedTaggingOperation, while denials of non-tagging calls keep plain AccessDenied.
Traces: cloudformation__iam__sqs.sqs-queue-handler-permission-update-denied-then-delete-denied-recovers.

## Evidence
Check cloudformation__iam__sqs.sqs-queue-handler-permission-update-denied-then-delete-denied-recovers, assertion on the Queue UPDATE_FAILED event's ResourceStatusReason (account, request ids, attempt count and session name masked by the check).

Expected (AWS):
Resource handler returned message: "Encountered a permissions error performing a tagging operation, please add required tag permissions. See https://repost.aws/knowledge-center/cloudformation-tagging-permission-error for how to resolve. Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<update-role>/<session> is not authorized to perform: sqs:tagqueue on resource: arn:aws:sqs:us-west-2:<account>:<queue> because no identity-based policy allows the sqs:tagqueue action (Service: Sqs, Status Code: 403, Request ID: <id>) (SDK Attempt Count: <n>)"" (RequestToken: <id>, HandlerErrorCode: UnauthorizedTaggingOperation)

Actual (emulator):
Resource handler returned message: "User: arn:aws:sts::<account>:assumed-role/<update-role>/AWSCloudFormation is not authorized to perform: sqs:tagqueue on resource: arn:aws:sqs:us-west-2:<account>:<queue> because no identity-based policy allows the sqs:tagqueue action (Service: Sqs, Status Code: 403, Request ID: <id>) (SDK Attempt Count: 1)" (RequestToken: <id>, HandlerErrorCode: AccessDenied)

AssertionError: the queue update failed with '<actual above>' (the tagging wrapper is absent and HandlerErrorCode is AccessDenied instead of UnauthorizedTaggingOperation).

## Steps to Reproduce
1. Boot the emulator and point botocore at it.
2. Create an IAM role CloudFormation can assume holding the AWS::SQS::Queue handler permissions EXCEPT sqs:TagQueue (sqs:CreateQueue, GetQueueUrl, GetQueueAttributes, ListQueueTags, SetQueueAttributes, UntagQueue, DeleteQueue).
3. cfn.create_stack with a single AWS::SQS::Queue (QueueName set, no Tags) and RoleARN = that role; wait for CREATE_COMPLETE.
4. cfn.update_stack with the same template plus Tags [{Key: stage, Value: after}] and the same RoleARN; wait for a terminal status (UPDATE_ROLLBACK_COMPLETE).
5. cfn.describe_stack_events and read the ResourceStatusReason of the Queue UPDATE_FAILED event -> expect the tagging-permission wrapper and HandlerErrorCode: UnauthorizedTaggingOperation; observe the bare denial with HandlerErrorCode: AccessDenied.
6. Re-replay the check id in trace_ids.
