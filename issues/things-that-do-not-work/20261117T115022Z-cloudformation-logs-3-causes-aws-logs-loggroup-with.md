Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__logs.cfn-logs-loggroup-deletion-protection-blocks-stack-delete-then-update-or-live-flip-allows-it cloudformation__logs.cfn-logs-loggroup-class-update-misclassified-in-place-fails-rollback-then-delete cloudformation__logs.unnamed-metric-filter-pattern-update-in-place-then-invalid-rename-rolls-back
Issue-Key: cloudformation__logs.cfn-logs-loggroup-deletion-protection-blocks-stack-delete-then-update-or-live-flip-allows-it|run#7|AssertionError|assert _settle(cfn, sid) == "CREATE_COMPLETE", f"create ended {_stack(cfn, sid)!r}"
Issue-Key: cloudformation__logs.cfn-logs-loggroup-class-update-misclassified-in-place-fails-rollback-then-delete|run#22|AssertionError|assert final_status == "UPDATE_ROLLBACK_FAILED", (
Issue-Key: cloudformation__logs.unnamed-metric-filter-pattern-update-in-place-then-invalid-rename-rolls-back|run#30|AssertionError|assert statuses == want_statuses, (
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudformation__logs: 3 causes — AWS::Logs::LogGroup with DeletionProtectionEnabled fails to create, a misclassified LogGroupClass update rolls back cleanly, and a failed metric-filter rename emits no replacement events

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Three net-new root causes in the CloudFormation CloudWatch Logs resource handlers.

Cause 1 of 3: a log group declaring DeletionProtectionEnabled cannot be created at all.
AWS: recorded behavior only — AWS::Logs::LogGroup accepts DeletionProtectionEnabled, the resource reaches CREATE_COMPLETE and DescribeLogGroups then reports deletionProtectionEnabled true for it; DeleteStack on it ends DELETE_FAILED with the handler's generic rejection 'Resource handler returned message: "Invalid request provided: AWS::Logs::LogGroup" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)' (the token is a per-run value), and UpdateStack on the DELETE_FAILED stack is refused ValidationError 'Stack:<stackId> is in DELETE_FAILED state and can not be updated.'
Emulator: the create itself fails — the stack ends ROLLBACK_COMPLETE with StackStatusReason 'The following resource(s) failed to create: [Group]. ', so none of the protection semantics is reachable.
Why: deletion protection is the documented way to keep a log group from being destroyed by a stack delete; a template using it cannot be deployed at all.
Fix: the AWS::Logs::LogGroup resource handler's create path and its DeletionProtectionEnabled property support (and the delete path that must refuse while protection is on).
Failing check: cloudformation__logs.cfn-logs-loggroup-deletion-protection-blocks-stack-delete-then-update-or-live-flip-allows-it

Cause 2 of 3: a LogGroupClass change the change set predicted as in-place rolls back cleanly instead of failing its rollback.
AWS: recorded behavior only — a LogGroupClass change that the change set classifies as an in-place update cannot actually be applied by the service, the update fails, and the rollback of that same resource also fails, leaving the stack UPDATE_ROLLBACK_FAILED.
Emulator: the stack settles UPDATE_ROLLBACK_COMPLETE, i.e. the rollback is reported as having succeeded.
Why: a client is told the stack is back in a good state when AWS would have left it needing a ContinueUpdateRollback; automation that branches on UPDATE_ROLLBACK_FAILED never fires.
Fix: the AWS::Logs::LogGroup handler's update/rollback path for LogGroupClass (the update must fail, and the rollback of the failed resource must be reported as failed).
Failing check: cloudformation__logs.cfn-logs-loggroup-class-update-misclassified-in-place-fails-rollback-then-delete

Cause 3 of 3: a failed rename of an unnamed metric filter emits no replacement lifecycle events.
AWS: recorded behavior only — changing the FilterName of an AWS::Logs::MetricFilter is a replacement, so the resource's event stream carries the full replacement cycle (UPDATE_IN_PROGRESS, then DELETE_IN_PROGRESS/DELETE_COMPLETE and CREATE_IN_PROGRESS/CREATE_FAILED for the invalid new name, then UPDATE_FAILED, then the rollback's own delete/create pair ending CREATE_COMPLETE and UPDATE_COMPLETE).
Emulator: the resource emits only ['UPDATE_IN_PROGRESS', 'UPDATE_FAILED'] — no delete/create pair for either the attempted replacement or the rollback.
Why: the event stream is the only record of what CloudFormation did to a resource; a client cannot see that the old filter was removed and re-created, and a rollback that re-created the resource is indistinguishable from one that never touched it.
Fix: the AWS::Logs::MetricFilter handler's replacement path and the stack-event emission for a replacing update and its rollback.
Failing check: cloudformation__logs.unnamed-metric-filter-pattern-update-in-place-then-invalid-rename-rolls-back

## Evidence
Cause 1: [mismatch] cloudformation__logs.cfn-logs-loggroup-deletion-protection-blocks-stack-delete-then-update-or-live-flip-allows-it — AssertionError: create ended {... 'StackStatus': 'ROLLBACK_COMPLETE', 'StackStatusReason': 'The following resource(s) failed to create: [Group]. ' ...}; expected CREATE_COMPLETE with deletionProtectionEnabled true on the live group.

Cause 2: [mismatch] cloudformation__logs.cfn-logs-loggroup-class-update-misclassified-in-place-fails-rollback-then-delete — AssertionError: 'a LogGroupClass change the change set predicted as in-place must still fail to apply AND fail to roll back cleanly; got UPDATE_ROLLBACK_COMPLETE' (expected UPDATE_ROLLBACK_FAILED).

Cause 3: [mismatch] cloudformation__logs.unnamed-metric-filter-pattern-update-in-place-then-invalid-rename-rolls-back — AssertionError: failed rename produced events ['UPDATE_IN_PROGRESS', 'UPDATE_FAILED'], expected ['UPDATE_IN_PROGRESS', 'DELETE_IN_PROGRESS', 'DELETE_COMPLETE', 'CREATE_IN_PROGRESS', 'CREATE_FAILED', 'UPDATE_FAILED', 'UPDATE_IN_PROGRESS', 'DELETE_IN_PROGRESS', 'DELETE_COMPLETE', 'CREATE_IN_PROGRESS', 'CREATE_COMPLETE', 'UPDATE_COMPLETE'].

## Steps to Reproduce
1. CreateStack with an AWS::Logs::LogGroup carrying DeletionProtectionEnabled: true and poll the stack status.
2. CreateStack with an AWS::Logs::LogGroup, create a change set that alters LogGroupClass and is classified in-place, execute it, and poll for the terminal stack status.
3. CreateStack with an unnamed AWS::Logs::MetricFilter, update its FilterPattern in place, then update it again with an invalid FilterName and read DescribeStackEvents for that logical resource.
