Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.execute-stack-refactor-rejects-re-execution-once-complete cloudformation.stack-refactor-actions-ordering
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.execute-stack-refactor-rejects-re-execution-once-complete|run#21|AssertionError|assert message == (
Issue-Key: cloudformation.stack-refactor-actions-ordering|run#35|AssertionError|assert topic_a_status == "CREATE_COMPLETE", (
Issue-Commit: 622c214bd0a84106899ec4445dc3396f8892f16d

# cloudformation: 2 net-new stack-refactor causes — the re-execution refusal is worded differently, and a cross-stack MOVE leaves the destination resource at IMPORT_COMPLETE

> _Note: this issue was identified against the code base at commit `622c214bd0a84106899ec4445dc3396f8892f16d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStackRefactor

## Description
Two independent net-new root causes in the CloudFormation stack-refactor surface. Each must be closed separately.

=== Cause 1 of 2: ExecuteStackRefactor on an already-executed refactor is refused with the emulator's own sentence ===
Failing check: cloudformation.execute-stack-refactor-rejects-re-execution-once-complete
AWS:      Smithy: ExecuteStackRefactor(StackRefactorId: required) -> Unit, no operation-specific errors (the awsQuery common ValidationError applies); DescribeStackRefactor output ExecutionStatus targets enum StackRefactorExecutionStatus. A refactor already in a terminal execution status cannot be executed again; AWS refuses with HTTP 400, Error.Code 'ValidationError' and Error.Message `Cannot execute stack refactor [<StackRefactorId>] as its execution status is [<ExecutionStatus>]`. Fixed text: `Cannot execute stack refactor [`, `] as its execution status is [`, `]`. Variable parts: the StackRefactorId copied from the request and the refactor's current ExecutionStatus from state (EXECUTE_COMPLETE here).
Emulator: the code is right (ValidationError) but the sentence is re-worded: `Stack refactor [<id>] cannot be executed in its current execution status of [EXECUTE_COMPLETE]`.
Why:      a client that matches the refusal text to tell 'already applied, nothing to do' from other ValidationErrors on the same operation (idempotent re-drive in refactor tooling) does not match, and treats an already-complete refactor as an unexpected failure.
Fix:      the ExecuteStackRefactor handler's execution-status guard — the refusal message text, not the code.
Traces:   cloudformation.execute-stack-refactor-rejects-re-execution-once-complete. The open issue 20261102T023139Z-cloudformation-stack-refactor-execution-stackset-import-validation-and.md covers ExecuteStackRefactor never ADVANCING ExecutionStatus (that part now works: the check's first execution reaches EXECUTE_COMPLETE); it does not cover this refusal wording.

=== Cause 2 of 2: a cross-stack MOVE records the destination resource as IMPORT_COMPLETE instead of CREATE_COMPLETE ===
Failing check: cloudformation.stack-refactor-actions-ordering
AWS:      Smithy: CreateStackRefactor(StackDefinitions, ResourceMappings) + ExecuteStackRefactor; ListStackRefactorActions reports Action=MOVE / Entity=RESOURCE per mapping; DescribeStackResource returns StackResourceDetail.ResourceStatus (enum ResourceStatus). Recorded behavior: after a MOVE of a resource from one stack to another, the destination stack's resource keeps the same PhysicalResourceId and its ResourceStatus is CREATE_COMPLETE — a refactor MOVE is not reported as a resource import, even though the physical resource is pre-existing.
Emulator: DescribeStackResource on the destination stack reports ResourceStatus 'IMPORT_COMPLETE' for the moved resource. The move itself happens (the physical id is preserved and the source logical id stops resolving), so only the recorded status of the destination resource diverges.
Why:      a client that keys on ResourceStatus to decide whether a resource was adopted by an import or created/owned by this stack (drift triage, change-set review, any status allow-list that accepts CREATE_COMPLETE/UPDATE_COMPLETE) sees a status it does not expect for a refactored stack and reports the stack as holding imported resources it does not hold.
Fix:      the ExecuteStackRefactor MOVE applier and the resource-status it writes into stack state for the destination stack's resource.
Traces:   cloudformation.stack-refactor-actions-ordering. The open issue 20261122T004908Z-cloudformation-sqs-cdk-refactor-renames-a-construct-by.md describes the same IMPORT_COMPLETE-for-a-rename symptom under the cloudformation__sqs service for the `cdk refactor` check; it does not carry this check's replay key and does not cover the native CreateStackRefactor/ExecuteStackRefactor cross-stack MOVE path.

## Evidence
Cause 1 — cloudformation.execute-stack-refactor-rejects-re-execution-once-complete [mismatch]
After a refactor reaches Status CREATE_COMPLETE, ExecuteStackRefactor succeeds and ExecutionStatus settles EXECUTE_COMPLETE (positive control passes, the renamed logical id still resolves to the original topic ARN). Calling ExecuteStackRefactor(StackRefactorId=<same id>) again:
  expected: ValidationError, message 'Cannot execute stack refactor [<refactor id>] as its execution status is [EXECUTE_COMPLETE]'
  actual:   ValidationError, message 'Stack refactor [<refactor id>] cannot be executed in its current execution status of [EXECUTE_COMPLETE]'
AssertionError at check line 153. The refactor id in any run is a per-run value.

Cause 2 — cloudformation.stack-refactor-actions-ordering [mismatch]
Two stacks, each with a topic to swap: ResourceMappings move TopicA stack1->stack2 and TopicC stack2->stack1. CreateStackRefactor reaches CREATE_COMPLETE, ListStackRefactorActions lists the two MOVE/RESOURCE actions, ExecuteStackRefactor reaches EXECUTE_COMPLETE. Then:
  DescribeStackResource(StackName=<stack2 id>, LogicalResourceId='TopicA').StackResourceDetail.ResourceStatus
  expected: 'CREATE_COMPLETE'
  actual:   'IMPORT_COMPLETE'
AssertionError at check line 95: "TopicA in stack2 expected CREATE_COMPLETE, got IMPORT_COMPLETE".

## Steps to Reproduce
Cause 1: CreateStack a stack with one AWS::SNS::Topic; CreateStackRefactor with a StackDefinitions entry renaming that logical id and a matching ResourceMappings entry; wait for Status CREATE_COMPLETE; ExecuteStackRefactor and wait for ExecutionStatus EXECUTE_COMPLETE; call ExecuteStackRefactor again with the same StackRefactorId and read Error.Code/Error.Message.

Cause 2: CreateStack two stacks (stack1: TopicA + Keep1; stack2: TopicC + Keep2) and wait for CREATE_COMPLETE. CreateStackRefactor with StackDefinitions for both stacks swapped (stack1: Keep1 + TopicC; stack2: Keep2 + TopicA) and ResourceMappings moving TopicA to stack2 and TopicC to stack1. Wait for CREATE_COMPLETE, ExecuteStackRefactor, wait for EXECUTE_COMPLETE, then DescribeStackResource(StackName=<stack2>, LogicalResourceId='TopicA') and read ResourceStatus.
