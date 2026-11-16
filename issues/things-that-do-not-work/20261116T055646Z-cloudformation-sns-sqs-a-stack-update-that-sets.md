Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sns__sqs.obs-cloudformation-3-notification-arns-stream-update-rollback-delete-and-stop-when-cleared
Issue-Key: cloudformation__sns__sqs.obs-cloudformation-3-notification-arns-stream-update-rollback-delete-and-stop-when-cleared|run#38|AssertionError|assert status == "UPDATE_ROLLBACK_COMPLETE", f"U2: expected UPDATE_ROLLBACK_COMPLETE, got {status!r}"
Issue-Commit: 911c830f7d5d4e7f71004d0c38e62fd15ce28281

# cloudformation__sns__sqs: a stack update that sets AWS::SSM::Parameter Value to the empty string reaches UPDATE_COMPLETE instead of UPDATE_FAILED + rollback

> _Note: this issue was identified against the code base at commit `911c830f7d5d4e7f71004d0c38e62fd15ce28281`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1: an UpdateStack whose only change is an invalid property VALUE on an existing AWS::SSM::Parameter (Value resolved from a template parameter to the empty string) is applied successfully instead of being refused by the resource handler, so the stack never enters its update-rollback path.

AWS: the template change passes template validation (Value is a String property fed by !Ref of a String stack parameter), so the refusal happens in the resource handler, which calls SSM PutParameter. Smithy (ssm 2014-11-06): PutParameter$Value is required and targets PSParameterValue (string, sensitive; no modeled length), and AWS refuses an empty value with a ValidationException whose message is the standard constraint text 'N validation error(s) detected: Value '' at 'value' failed to satisfy constraint: Member must have length greater than or equal to 1' — recorded behavior only for the length floor, since the model does not state it. CloudFormation therefore records, for the parameter resource, the status sequence UPDATE_IN_PROGRESS -> UPDATE_FAILED -> UPDATE_IN_PROGRESS -> UPDATE_COMPLETE (the rollback restoring the previous value), with the UPDATE_FAILED event's ResourceStatusReason beginning with the fixed text `Resource handler returned message: "Error occurred during operation 'PutParameter'."` and containing `HandlerErrorCode: GeneralServiceException` (the RequestToken inside it is a per-run value, not part of the requirement); the stack itself goes UPDATE_IN_PROGRESS -> UPDATE_ROLLBACK_IN_PROGRESS (ResourceStatusReason the fixed text 'The following resource(s) failed to update: [<LogicalResourceId>]. ', the logical id taken from the template) -> UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS -> UPDATE_ROLLBACK_COMPLETE, and DescribeStacks keeps the PREVIOUS ParameterValue. Each of those events is also published to the stack's NotificationARNs.

Emulator: the same UpdateStack converges to StackStatus 'UPDATE_COMPLETE'. The empty Value is accepted — no UPDATE_FAILED event for the parameter resource, no UPDATE_ROLLBACK_* stack transition, and consequently none of the rollback notifications. The check stops at the stack status assertion, so the downstream event/notification and retained-value assertions were never reached; what is established is that the emulator reports success where AWS reports a handler failure followed by a completed rollback.

Why: a client gets the opposite verdict for a deploy AWS refuses. The stack is reported healthy while the SSM parameter it owns was written with a value AWS would never store, drift/rollback-based release gates ("did the bad deploy roll back?") pass vacuously, and no UPDATE_FAILED/UPDATE_ROLLBACK_* event or notification ever reaches the operator's SNS topic, so the failure is invisible to monitoring built on the stack-event stream.

Fix: two components, in order — SSM's PutParameter input validation must refuse an empty Value with the ValidationException above instead of storing it, and CloudFormation's AWS::SSM::Parameter resource-handler update path must turn that refusal into an UPDATE_FAILED resource event carrying the handler text and HandlerErrorCode, which then drives the stack's existing update-rollback machinery (UPDATE_ROLLBACK_IN_PROGRESS -> UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS -> UPDATE_ROLLBACK_COMPLETE) and the notification publication for those events.

Traces: cloudformation__sns__sqs.obs-cloudformation-3-notification-arns-stream-update-rollback-delete-and-stop-when-cleared (leg U2).

## Evidence
Check cloudformation__sns__sqs.obs-cloudformation-3-notification-arns-stream-update-rollback-delete-and-stop-when-cleared, leg U2: after create + a clean update (U1, UPDATE_COMPLETE), UpdateStack(UsePreviousTemplate=True, Parameters=[ParameterName=<name>, ParameterValue='']) on a stack whose single resource is
  Notified:
    Type: AWS::SSM::Parameter
    Properties: { Name: !Ref ParameterName, Type: String, Value: !Ref ParameterValue }

Expected (AWS): DescribeStacks StackStatus 'UPDATE_ROLLBACK_COMPLETE'; resource 'Notified' events UPDATE_IN_PROGRESS, UPDATE_FAILED (ResourceStatusReason starting `Resource handler returned message: "Error occurred during operation 'PutParameter'."` and containing `HandlerErrorCode: GeneralServiceException`), UPDATE_IN_PROGRESS, UPDATE_COMPLETE; stack events UPDATE_IN_PROGRESS, UPDATE_ROLLBACK_IN_PROGRESS (reason 'The following resource(s) failed to update: [Notified]. '), UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS, UPDATE_ROLLBACK_COMPLETE; DescribeStacks ParameterValue still U1's value.

Actual (emulator): AssertionError: U2: expected UPDATE_ROLLBACK_COMPLETE, got 'UPDATE_COMPLETE' (check file line 153, assertion `assert status == "UPDATE_ROLLBACK_COMPLETE"` after polling DescribeStacks out of *_IN_PROGRESS).

## Steps to Reproduce
1. CreateStack with a template containing one AWS::SSM::Parameter whose Name and Value come from String template parameters, passing a non-empty ParameterValue; wait for CREATE_COMPLETE.
2. UpdateStack(UsePreviousTemplate=True) with the same ParameterName and ParameterValue='' (empty string).
3. Poll DescribeStacks until StackStatus leaves *_IN_PROGRESS: AWS reports UPDATE_ROLLBACK_COMPLETE, the emulator reports UPDATE_COMPLETE.
4. DescribeStackEvents: on AWS there is exactly one UPDATE_FAILED event for the parameter's logical id carrying the PutParameter handler text; on the emulator there is none.
5. DescribeStacks Parameters: on AWS the pre-update value is retained by the rollback; on the emulator the empty value is in force.
