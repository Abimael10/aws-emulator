Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails cloudformation__sqs.cdk-rollback-orphan-skips-the-resource-whose-rollback-fails
Issue-Key: cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails|run#18|AssertionError|assert status == "UPDATE_FAILED", f"the --no-rollback update left the stack {status!r}"
Issue-Key: cloudformation__sqs.cdk-rollback-orphan-skips-the-resource-whose-rollback-fails|run#18|AssertionError|assert status == "UPDATE_FAILED", f"the --no-rollback update left the stack {status!r}"
Issue-Commit: 1b14ccf8bc557e9f3ed20a9ce1fb318436d9b18d

# cloudformation__sqs: a `--no-rollback` (DisableRollback) update whose resource fails never opens the stack transition — the stack stays CREATE_COMPLETE instead of settling in UPDATE_FAILED

> _Note: this issue was identified against the code base at commit `1b14ccf8bc557e9f3ed20a9ce1fb318436d9b18d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
One net-new root cause, grounded in two failing checks the suite lists under `cloudformation__sqs`.

Cause 1 of 1: an update submitted with rollback disabled (`cdk deploy --no-rollback`, i.e. DisableRollback=true on the UpdateStack/ExecuteChangeSet call) whose template contains a resource the owning service must refuse does not become a failed update at all. The CLI exits non-zero, but DescribeStacks on the StackId still reports the status the create left behind, CREATE_COMPLETE — no UPDATE_IN_PROGRESS, no UPDATE_FAILED, no resource-level failure event for the refused resource — so neither the DisableRollback semantics nor the follow-on rollback paths (`cdk rollback`, `cdk rollback --force`, `cdk rollback --orphan`, i.e. ContinueUpdateRollback/RollbackStack) can ever be reached. Failing checks: cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails, cloudformation__sqs.cdk-rollback-orphan-skips-the-resource-whose-rollback-fails.

AWS: Smithy (cloudformation): UpdateStackInput.DisableRollback — "Preserve the state of previously provisioned resources when an operation fails. Default: False". The emulator's own UpdateStack contract states the same rule ("Only a caller that asked for the failed state to be kept (`DisableRollback`) stops at `UPDATE_FAILED`, which is the status `ContinueUpdateRollback` and `RollbackStack` act on"). So an update with rollback disabled must OPEN the transition (UPDATE_IN_PROGRESS), attempt the template's resources, record the refusal as a resource-level event on the failing logical id, and settle the stack in UPDATE_FAILED with the previously provisioned resources left as the failed update made them. In the scenario behind these checks the second template keeps one AWS::SQS::Queue (VisibilityTimeout 30 -> 60, an in-place update that succeeds) and adds two AWS::SSM::Parameter resources whose Name is the SAME string (the second takes `!Ref` of the first), so SSM must refuse the second one as a name that already exists and that resource — not the update call — is what fails. The recorded behaviour the checks grade afterwards is also stated by them: the queue's later rollback failure reason is the resource-handler sentence `Resource handler returned message: "<owning service's refusal>. (Service: Sqs, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: NotFound)` — fixed text with the service name, the HTTP status, the error class (HandlerErrorCode) and the owning service's own message as its variable parts, and the request id / token per call.

Emulator: the first `cdk deploy` of the same app succeeds (rc 0, stack CREATE_COMPLETE, queue resolvable). The second deploy with `--no-rollback` exits 1 — so something refused — but `describe_stacks(StackName=<StackId>)["Stacks"][0]["StackStatus"]` is still `'CREATE_COMPLETE'`, where AWS reports `'UPDATE_FAILED'`. Hypothesis: the colliding AWS::SSM::Parameter is rejected synchronously when the update (or its change set execution) is submitted, so the stack is never moved out of CREATE_COMPLETE and no per-resource UPDATE_FAILED event is recorded; the alternative — that the update was accepted and completed — is excluded because the CLI exited 1 and the status is not UPDATE_COMPLETE.

Why: a client that deploys with rollback disabled precisely so it can inspect and then resume the failure sees a stack that claims it is healthy and unchanged (CREATE_COMPLETE) after a deployment that failed. Every recovery path keyed on that status breaks: `cdk rollback`/RollbackStack and ContinueUpdateRollback refuse a stack that is not in a *_FAILED/*_ROLLBACK_FAILED status, DescribeStackEvents names no failing resource so nothing says WHY the deploy failed, and automation that gates on StackStatus cannot distinguish a failed update from an update that never ran.

Fix: cloudformation's UpdateStack / ExecuteChangeSet stack-transition and resource-provisioning path — a resource an owning service refuses must fail the opened stack operation (resource event + UPDATE_FAILED under DisableRollback) rather than be refused synchronously as the update call; and the AWS::SSM::Parameter resource handler's duplicate-name refusal must surface through that path.

Traces: cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails, cloudformation__sqs.cdk-rollback-orphan-skips-the-resource-whose-rollback-fails (both stop on the same assertion at check line 53).

## Evidence
cloudformation__sqs.cdk-rollback-force-completes-past-the-resource-whose-rollback-fails (check line 53):
  assert status == "UPDATE_FAILED", f"the --no-rollback update left the stack {status!r}"
  AssertionError: the --no-rollback update left the stack 'CREATE_COMPLETE'

cloudformation__sqs.cdk-rollback-orphan-skips-the-resource-whose-rollback-fails (check line 53): identical assertion, identical message:
  AssertionError: the --no-rollback update left the stack 'CREATE_COMPLETE'

Expected vs actual:
  expected DescribeStacks(StackName=<StackId>).Stacks[0].StackStatus = "UPDATE_FAILED" after a `--no-rollback` update whose added AWS::SSM::Parameter duplicates another parameter's Name, with DescribeStackEvents holding a *_FAILED event for the offending logical id and its ResourceStatusReason naming the owning service's refusal.
  actual   StackStatus = "CREATE_COMPLETE" (the status the preceding create left), i.e. no UPDATE_IN_PROGRESS and no UPDATE_FAILED transition was recorded at all.

What matched before the failure: the first deploy of variant 1 exited 0 and the stack reached CREATE_COMPLETE; GetQueueUrl resolved the queue the template names; the failing `--no-rollback` deploy did exit 1 (the check asserts rc == 1 and that assertion passed), so the CLI was told the deployment failed while the stack's own status says nothing happened.

Not reached because the run stops here: the queue's single `('Queue','UPDATE_FAILED')` event and its `Resource handler returned message: "The specified queue does not exist. (Service: Sqs, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: NotFound)` reason, the UPDATE_ROLLBACK_COMPLETE after `cdk rollback --force` (resp. after `cdk rollback --orphan Queue`), and the DELETE_COMPLETE of the parameter the failed update added.

## Steps to Reproduce
1. Deploy a stack holding one AWS::SQS::Queue with an explicit QueueName and VisibilityTimeout 30 (CreateStack or `cdk deploy`); wait for CREATE_COMPLETE and record StackId.
2. Submit the SECOND template with rollback disabled — UpdateStack(StackName=<StackId>, DisableRollback=true, TemplateBody=...) or `cdk deploy <stack> --no-rollback` — where the template (a) raises the same queue's VisibilityTimeout to 60 and (b) adds two AWS::SSM::Parameter resources, the first with Name=<some name> and the second with Name=!Ref <first> (so both ask for the same parameter name), the second depending on the queue so the queue update completes first.
3. Poll DescribeStacks(StackName=<StackId>).
   Expected: the stack goes UPDATE_IN_PROGRESS and settles at UPDATE_FAILED (rollback disabled), DescribeStackEvents carries a *_FAILED event for the duplicate parameter naming SSM's already-exists refusal, and the queue's VisibilityTimeout change stands.
   Emulator: the CLI/API call reports failure but StackStatus is still CREATE_COMPLETE and no update events exist.
4. (Blocked by step 3 today) Delete the queue out of band, then call RollbackStack / `cdk rollback --force` and `cdk rollback --orphan Queue`: the rollback must fail on the deleted queue with the resource-handler sentence above and reach UPDATE_ROLLBACK_COMPLETE (forced/orphaned), deleting the parameter the failed update added.
