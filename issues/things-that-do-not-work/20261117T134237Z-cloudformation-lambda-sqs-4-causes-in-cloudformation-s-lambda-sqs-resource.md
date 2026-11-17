Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lambda__sqs.cfn-esm-malformed-filter-pattern-rolls-back-queue-and-function cloudformation__lambda__sqs.cfn-lambda-deadletterconfig-nonexistent-queue-rejected cloudformation__lambda__sqs.cfn-sqs-esm-scaling-config-below-minimum-rejected cloudformation__lambda__sqs.deploy-eventinvokeconfig-retry-attempts-above-the-schema-max-rejected-and-rolls-back
Issue-Key: cloudformation__lambda__sqs.cfn-esm-malformed-filter-pattern-rolls-back-queue-and-function|_assert_malformed_filter_reason#4|AssertionError|assert reason.startswith(prefix), f"unexpected CREATE_FAILED reason prefix: {reason!r}"
Issue-Key: cloudformation__lambda__sqs.cfn-lambda-deadletterconfig-nonexistent-queue-rejected|run#25|AssertionError|assert got == want, f"Fn's failure reason {got!r}, expected {want!r}"
Issue-Key: cloudformation__lambda__sqs.cfn-sqs-esm-scaling-config-below-minimum-rejected|run#12|AssertionError|assert prefix == (
Issue-Key: cloudformation__lambda__sqs.deploy-eventinvokeconfig-retry-attempts-above-the-schema-max-rejected-and-rolls-back|run#19|AssertionError|assert failed, f"expected a stack-level CREATE_FAILED event; events were {events!r}"
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# 4 causes in CloudFormation's Lambda/SQS resource handlers: two refusal messages are not the ones AWS words, a nonexistent DLQ is misattributed to IAM, and an EventInvokeConfig schema violation emits no stack-level CREATE_FAILED

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Four distinct root causes, all in CloudFormation resource handlers for Lambda resources that reference SQS.

Cause 1 of 4: the CREATE_FAILED reason for an AWS::Lambda::EventSourceMapping with a malformed `FilterCriteria` drops CloudFormation's `Invalid request provided:` prefix.
AWS: the resource handler wraps the service refusal in a fixed template whose only variable parts are the two per-call ids. The fixed text is `Resource handler returned message: "Invalid request provided: Invalid filter pattern definition. (Service: Lambda, Status Code: 400, Request ID: ` + <request id> + `) (SDK Attempt Count: 1)" (RequestToken: ` + <request token> + `, HandlerErrorCode: InvalidRequest)`. (Smithy: lambda CreateEventSourceMapping input member `FilterCriteria`, error InvalidParameterValueException httpError 400 — recorded behavior only for the handler wrapper text.)
Emulator: the `Invalid request provided: ` segment is missing from an otherwise correctly shaped wrapper.
Why: a client matching the documented handler wrapper to classify a template error does not match, and the failure reason it surfaces to users differs from AWS's.
Fix: the AWS::Lambda::EventSourceMapping resource handler's failure-reason wrapper in the cloudformation service.
Traces: cloudformation__lambda__sqs.cfn-esm-malformed-filter-pattern-rolls-back-queue-and-function

Cause 2 of 4: an AWS::Lambda::Function whose `DeadLetterConfig.TargetArn` names a queue that does not exist is refused as an IAM permission problem instead of a nonexistent-queue problem.
AWS: Lambda probes the DLQ at create time and reports the SQS error it got. The fixed text is `Error occurred while SendMessage. SQS Error Code: AWS.SimpleQueueService.NonExistentQueue. SQS Error Message: The specified queue does not exist. (Service: Lambda, Status Code: 400, Request ID: ` + <request id> + `) (SDK Attempt Count: ` + <n> + `)` with HandlerErrorCode `InvalidRequest`. (Smithy: lambda CreateFunction input member `DeadLetterConfig` -> `DeadLetterConfig.TargetArn`, error InvalidParameterValueException httpError 400 — recorded behavior only for the message text.)
Emulator: the reason blames the execution role's SendMessage permission instead of reporting that the queue is absent. The error class/HandlerErrorCode are right; the diagnosis is wrong.
Why: the message is the only thing telling an operator what to fix. Sending them to the role's policy when the queue simply does not exist makes the failure undiagnosable, and automation that branches on `AWS.SimpleQueueService.NonExistentQueue` never fires.
Fix: the AWS::Lambda::Function handler's DeadLetterConfig pre-flight (and lambda CreateFunction's DLQ probe) — it must distinguish an absent queue from a denied SendMessage and report the SQS error code it actually received.
Traces: cloudformation__lambda__sqs.cfn-lambda-deadletterconfig-nonexistent-queue-rejected

Cause 3 of 4: an AWS::Lambda::EventSourceMapping with `ScalingConfig.MaximumConcurrency` below the minimum is refused with the Lambda API's parameter-validation wording instead of CloudFormation's resource-schema wording.
AWS: the value is out of the resource schema's declared range, so the handler refuses it before calling Lambda, with fixed text `Resource handler returned message: "Model validation failed (#/ScalingConfig/MaximumConcurrency: failed validation constraint for keyword [minimum])" (RequestToken: ` + <token> + `, HandlerErrorCode: InvalidRequest)`; the only variable part is the request token. (Smithy: lambda CreateEventSourceMapping input member `ScalingConfig` -> `ScalingConfig.MaximumConcurrency` with range min 2, max 1000.)
Emulator: the reason carries the service's own validation message, so the value reached Lambda instead of being caught by the schema.
Why: same consequence as Cause 1 — the failure is attributed to the service rather than the template, and the documented `Model validation failed` classification is unavailable.
Fix: the AWS::Lambda::EventSourceMapping handler's property-validation stage — numeric `range` constraints from the resource schema must be enforced ahead of the service call.
Traces: cloudformation__lambda__sqs.cfn-sqs-esm-scaling-config-below-minimum-rejected

Cause 4 of 4: an AWS::Lambda::EventInvokeConfig whose `MaximumRetryAttempts` is above the schema maximum rolls the stack back without ever emitting a stack-level CREATE_FAILED event.
AWS: when a create fails, DescribeStackEvents carries a CREATE_FAILED event for the stack's own logical id (ResourceType AWS::CloudFormation::Stack) carrying the reason, before ROLLBACK_IN_PROGRESS/ROLLBACK_COMPLETE. (Smithy: cloudformation DescribeStackEvents output member `StackEvents[].ResourceStatus`, enum ResourceStatus value CREATE_FAILED.)
Emulator: the event stream for the rolled-back stack contains only ROLLBACK_COMPLETE and resource DELETE_COMPLETE events — no stack-level CREATE_FAILED anywhere.
Why: the stack-level CREATE_FAILED event is where a client reads why a deploy failed. Without it, a caller sees a rolled-back stack with no recorded cause and cannot report or branch on the failure.
Fix: the cloudformation stack create/rollback event emitter — a failed create must record a CREATE_FAILED event against the stack itself, carrying the failure reason, before rollback begins.
Traces: cloudformation__lambda__sqs.deploy-eventinvokeconfig-retry-attempts-above-the-schema-max-rejected-and-rolls-back

## Evidence
Cause 1 — actual, verbatim:
  AssertionError: unexpected CREATE_FAILED reason prefix: 'Resource handler returned message: "Invalid filter pattern definition. (Service: Lambda, Status Code: 400, Request ID: <id>) (SDK Attempt Count: 1)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'
Expected prefix (fixed text): 'Resource handler returned message: "Invalid request provided: Invalid filter pattern definition. (Service: Lambda, Status Code: 400, Request ID: ' — the `Invalid request provided: ` segment is missing. (Request ID / RequestToken are per-run.)
Cause 2 — actual, verbatim:
  AssertionError: Fn's failure reason ('The provided execution role does not have permissions to call SendMessage on SQS (Service: Lambda, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest'), expected ('Error occurred while SendMessage. SQS Error Code: AWS.SimpleQueueService.NonExistentQueue. SQS Error Message: The specified queue does not exist. (Service: Lambda, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest')
Cause 3 — actual, verbatim:
  AssertionError: expected the schema-validation message, got 'Resource handler returned message: "1 validation error detected: Value \'1\' at \'scalingConfig.maximumConcurrency\' failed to satisfy constraint: Member must have value greater than or equal to 2 (Service: Lambda, Status Code: 400, Request ID: <id>) (SDK Attempt Count: 1)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'
Expected: 'Resource handler returned message: "Model validation failed (#/ScalingConfig/MaximumConcurrency: failed validation constraint for keyword [minimum])" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'.
Cause 4 — actual, verbatim:
  AssertionError: expected a stack-level CREATE_FAILED event; events were [... ResourceStatus 'ROLLBACK_COMPLETE' for ResourceType 'AWS::CloudFormation::Stack' ... ResourceStatus 'DELETE_COMPLETE' for the SQS queue ...]
No StackEvents entry has ResourceStatus CREATE_FAILED for the stack's own logical id.

## Steps to Reproduce
Cause 1: CreateStack with an AWS::Lambda::EventSourceMapping over an SQS queue whose FilterCriteria pattern is malformed; read the Esm resource's CREATE_FAILED reason. Observed reason lacks the `Invalid request provided: ` segment.
Cause 2: CreateStack with an AWS::Lambda::Function whose DeadLetterConfig.TargetArn names a queue that was never created; read Fn's failure reason. Observed the role-permission message; expected the `AWS.SimpleQueueService.NonExistentQueue` message.
Cause 3: CreateStack with an AWS::Lambda::EventSourceMapping whose ScalingConfig.MaximumConcurrency is 1; read the Mapping resource's failure reason. Observed the Lambda API validation message; expected `Model validation failed (#/ScalingConfig/MaximumConcurrency: failed validation constraint for keyword [minimum])`.
Cause 4: CreateStack with an AWS::Lambda::EventInvokeConfig whose MaximumRetryAttempts exceeds the schema maximum; call DescribeStackEvents. Observed no stack-level CREATE_FAILED event; expected one carrying the reason.
