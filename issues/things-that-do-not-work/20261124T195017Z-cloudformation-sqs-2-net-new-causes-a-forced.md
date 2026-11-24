Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails cloudformation__sqs.cfn-sqs-queuepolicy-invalid-action-rejected
Issue-Key: cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails|run#28|AssertionError|assert final["StackStatus"] == "UPDATE_ROLLBACK_COMPLETE", (
Issue-Key: cloudformation__sqs.cfn-sqs-queuepolicy-invalid-action-rejected|run#12|AssertionError|assert got_error == _REJECTED, f"CREATE_FAILED reason: expected {_REJECTED!r}, got {got_error!r}"
Issue-Commit: d50969a6e6efc58b2521aa7f9ba160b669c146a6

# cloudformation__sqs: 2 net-new causes — a forced continue-update-rollback ends UPDATE_ROLLBACK_FAILED instead of UPDATE_ROLLBACK_COMPLETE, and an SQS service error relayed by the AWS::SQS::QueuePolicy handler is classified InvalidRequest instead of GeneralServiceException

> _Note: this issue was identified against the code base at commit `d50969a6e6efc58b2521aa7f9ba160b669c146a6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ContinueUpdateRollback

## Description
Two net-new root causes in CloudFormation's rollback executor and in the error classification of its AWS::SQS::QueuePolicy resource handler. Both are grounded in failing checks the suite lists under `cloudformation__sqs` and neither is marked ALREADY FILED or OWNED in this run.

Cause 1 of 2: a forced continuation of a failed update rollback does not skip the resource whose rollback failed — the stack stays in the failed rollback state instead of completing past it.
AWS: ContinueUpdateRollback is modelled with ForceRollback (Smithy: cloudformation#ContinueUpdateRollbackInput$ForceRollback) whose documentation states "Specifies whether CloudFormation forces the rollback to continue by skipping resources currently in the UPDATE_FAILED state. Use this instead of listing each resource individually in ResourcesToSkip. Only resources that entered the UPDATE_FAILED state because a rollback failed are skipped" and "CloudFormation sets the status of the skipped resources to UPDATE_COMPLETE and continues to roll back the stack". So, for a stack sitting in UPDATE_FAILED whose updated queue was deleted out of band, one forced continuation (what `cdk rollback --force` issues) must: record exactly one UPDATE_FAILED event for the resource whose rollback cannot succeed, carrying the relayed handler reason, then SKIP that resource and drive the stack to the modelled terminal status UPDATE_ROLLBACK_COMPLETE (Smithy: cloudformation#StackStatus includes UPDATE_ROLLBACK_COMPLETE and UPDATE_ROLLBACK_FAILED as distinct members), deleting the resource the failed update had added (its final ResourceStatus is DELETE_COMPLETE). The per-resource skip semantics are documented behavior; the exact event ordering is recorded behavior only.
Emulator: the forced continuation leaves the stack in UPDATE_ROLLBACK_FAILED with StackStatusReason 'The following resource(s) failed to update: [Queue]. ' — fixed text whose only variable part is the bracketed list of logical ids taken from the stack's own state. The failing resource is never skipped, so the rollback never completes and the resource the failed update added is never deleted. Note this cause has ADVANCED past what open issue 20261102T094631Z-cloudformation-sqs-4-net-new-causes-an-import.md (Cause 2) recorded for the same check: the per-resource UPDATE_FAILED reason now matches AWS's relayed-handler form, so the check now gets as far as the terminal-status assertion and fails there. That open issue does not carry this new replay key, so this check will not be re-replayed by it; if it is fixed first, re-replay this check to close this cause.
Why: a stack wedged in UPDATE_ROLLBACK_FAILED is unrecoverable for a client. ForceRollback exists as the documented escape hatch out of exactly that state; with it inert, an operator (or `cdk rollback --force`, or any CI job that drives it) can never return a stack to a usable terminal status, and the resource the failed update added leaks forever.
Fix: the update-rollback executor's per-resource failure handling — the ForceRollback/ResourcesToSkip path of the ContinueUpdateRollback handler must mark a resource that entered UPDATE_FAILED because its rollback failed as UPDATE_COMPLETE, continue the rollback through the remaining resources (including deleting those the failed update added), and settle the stack at UPDATE_ROLLBACK_COMPLETE.
Traces: cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails.

Cause 2 of 2: an SQS validation error relayed through the AWS::SQS::QueuePolicy resource handler is classified with the wrong HandlerErrorCode.
AWS: when the queue-policy handler relays a 400 from SQS, the resource's CREATE_FAILED ResourceStatusReason has the form 'Resource handler returned message: "<service message> (Service: Sqs, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: <code>)'. For a policy naming an SQS action that does not exist, the service message is SQS's own validation text — 'Value <action> for parameter ActionName is invalid. Reason: Please refer to the appropriate WSDL for a list of valid actions.', whose fixed text is everything but <action>, which is the offending Action value copied from the template — and the handler classifies it as HandlerErrorCode 'GeneralServiceException' (a modelled member of Smithy cloudformation#HandlerErrorCode, alongside 'InvalidRequest'); which member AWS picks for a relayed downstream service refusal is recorded behavior. The stack then ends ROLLBACK_COMPLETE and the queue the attempt created is gone (GetQueueUrl answers AWS.SimpleQueueService.NonExistentQueue / 'The specified queue does not exist.').
Emulator: everything matches — the stack reaches ROLLBACK_COMPLETE, the relayed SDK envelope is present, and the service message is byte-for-byte AWS's — except the classification: HandlerErrorCode is 'InvalidRequest' where AWS reports 'GeneralServiceException'. Actual tuple (message, code) = ('Value sqs:NotARealAction for parameter ActionName is invalid. Reason: Please refer to the appropriate WSDL for a list of valid actions.', 'InvalidRequest'); expected (same message, 'GeneralServiceException').
Why: HandlerErrorCode is the machine-readable half of a resource failure. A client that branches on it cannot distinguish a template/property defect it should fix (InvalidRequest, raised by CloudFormation's own validation before any call) from a downstream service refusal (GeneralServiceException, raised when the owning service rejected the call). Retry and triage logic keyed on that distinction takes the wrong branch for every AWS::SQS::QueuePolicy that SQS refuses.
Fix: the error classification in the AWS::SQS::QueuePolicy resource handler (the mapping from a relayed SQS 400 to a HandlerErrorCode) — a refusal produced by the downstream service must be classified GeneralServiceException, not re-labelled as a request-validation error.
Traces: cloudformation__sqs.cfn-sqs-queuepolicy-invalid-action-rejected.

## Evidence
Cause 1 — cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails (check line 73):
  assert final["StackStatus"] == "UPDATE_ROLLBACK_COMPLETE"
  AssertionError: `cdk rollback --force` left the stack 'UPDATE_ROLLBACK_FAILED' ('The following resource(s) failed to update: [Queue]. ')
Expected: StackStatus 'UPDATE_ROLLBACK_COMPLETE' with no failure reason, after exactly one UPDATE_FAILED event for the queue carrying the relayed handler reason, and the added SSM parameter's last event ResourceStatus 'DELETE_COMPLETE'.
Actual: StackStatus 'UPDATE_ROLLBACK_FAILED', StackStatusReason 'The following resource(s) failed to update: [Queue]. ' (fixed text; the bracketed logical-id list is the only state-derived part). Everything before this matched: the first deploy succeeded, the --no-rollback update left the stack UPDATE_FAILED, the out-of-band DeleteQueue made GetQueueAttributes answer ('AWS.SimpleQueueService.NonExistentQueue', 'The specified queue does not exist.'), the forced rollback exited 0, and the queue's single UPDATE_FAILED event reason matched the full relayed form 'Resource handler returned message: "The specified queue does not exist. (Service: Sqs, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: NotFound)'.

Cause 2 — cloudformation__sqs.cfn-sqs-queuepolicy-invalid-action-rejected (check line 231):
  assert got_error == _REJECTED, f"CREATE_FAILED reason: expected {_REJECTED!r}, got {got_error!r}"
  AssertionError: CREATE_FAILED reason: expected ('Value sqs:NotARealAction for parameter ActionName is invalid. Reason: Please refer to the appropriate WSDL for a list of valid actions.', 'GeneralServiceException'), got ('Value sqs:NotARealAction for parameter ActionName is invalid. Reason: Please refer to the appropriate WSDL for a list of valid actions.', 'InvalidRequest')
Expected HandlerErrorCode: 'GeneralServiceException'. Actual: 'InvalidRequest'. The service message, the (Service: Sqs, Status Code: 400, Request ID: <id>) envelope, the ROLLBACK_COMPLETE stack status and the absence of the queue afterwards all matched; only the classification diverges.

## Steps to Reproduce
Cause 1:
1. CreateStack with one AWS::SQS::Queue (QueueName=<q>, VisibilityTimeout=30) and wait CREATE_COMPLETE; note the StackId and the queue URL.
2. UpdateStack with DisableRollback=true (i.e. `cdk deploy --no-rollback`) to a template that sets the queue's VisibilityTimeout=60 and adds two AWS::SSM::Parameter resources whose names collide (the second DependsOn the queue), so the queue update completes and the stack then fails. Wait for StackStatus UPDATE_FAILED.
3. DeleteQueue the stack's queue out of band; poll GetQueueAttributes until it answers AWS.SimpleQueueService.NonExistentQueue / 'The specified queue does not exist.'
4. ContinueUpdateRollback(StackName=<stackId>, ForceRollback=true) (equivalently `cdk rollback <stack> --force`).
   Expected: exactly one UPDATE_FAILED event for logical id Queue with reason 'Resource handler returned message: "The specified queue does not exist. (Service: Sqs, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: NotFound)', the resource then skipped, and DescribeStacks reporting StackStatus UPDATE_ROLLBACK_COMPLETE with the added SSM parameter's last event DELETE_COMPLETE.
   Emulator: StackStatus UPDATE_ROLLBACK_FAILED, StackStatusReason 'The following resource(s) failed to update: [Queue]. '

Cause 2:
1. CreateStack with a template holding an AWS::SQS::Queue (QueueName=<q>) and an AWS::SQS::QueuePolicy whose Queues is [{Ref Queue}] and whose PolicyDocument has one statement with Effect Allow, Principal {AWS: {Ref AWS::AccountId}}, Resource {Fn::GetAtt [Queue, Arn]} and Action 'sqs:NotARealAction'.
2. Poll DescribeStacks until terminal — expect ROLLBACK_COMPLETE (matches today).
3. DescribeStackEvents and read the single CREATE_FAILED row for logical id QueuePolicy; parse 'Resource handler returned message: "<message>" (RequestToken: <token>, HandlerErrorCode: <code>)'.
   Expected <code> = 'GeneralServiceException'. Emulator: 'InvalidRequest'.
4. GetQueueUrl(QueueName=<q>) must answer AWS.SimpleQueueService.NonExistentQueue / 'The specified queue does not exist.' (matches today).
5. Control: the same template with Action 'sqs:SendMessage' must reach CREATE_COMPLETE and the queue's Policy attribute must read back one statement with Principal {AWS: 'arn:<partition>:iam::<account>:root'} (matches today).
