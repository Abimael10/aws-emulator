Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__stepfunctions.nested-sync-parent-role-without-child-key-decrypt-denied
Issue-Key: kms__stepfunctions.nested-sync-parent-role-without-child-key-decrypt-denied|_run_control_past_propagation#10|Timeout|time.sleep(5.0)
Issue-Commit: 15eaad9fa74cff8f1dc14d7f5abd0b99014b7462

# kms__stepfunctions: a nested states:startExecution.sync:2 parent over a CMK-encrypted child never settles, so neither the granted control nor the kms:Decrypt denial reaches a verdict (check exhausts its 60 s budget)

> _Note: this issue was identified against the code base at commit `15eaad9fa74cff8f1dc14d7f5abd0b99014b7462`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: the optimized nested-execution integration `arn:aws:states:::states:startExecution.sync:2` over a CMK-encrypted child state machine makes no observable progress, so the parent's kms:Decrypt authorization on the child's key is never exercised.

AWS: recorded behavior only (Smithy models StartExecution/DescribeExecution but not the ASL optimized-integration semantics). A STANDARD parent whose single Task state is Resource "arn:aws:states:::states:startExecution.sync:2" with Parameters {StateMachineArn, Input} starts the child, waits for the child execution to finish and reads the child's result back through the task result (here ResultSelector {"fromChild.$": "$.Output.value"}). With a parent role holding states:StartExecution / states:DescribeExecution / states:StopExecution, the events:PutTargets/PutRule/DescribeRule grants on StepFunctionsGetEventsForStepFunctionsExecutionRule, AND kms:Decrypt on the CMK the child state machine is encrypted with, DescribeExecution on the parent reaches a terminal status SUCCEEDED within seconds and output == {"fromChild": "<seed>-child"}. With an otherwise identical parent role that lacks kms:Decrypt on the child's key, the parent execution reaches terminal status FAILED with error "States.TaskFailed" and a cause whose first sentence is the KMS denial of the parent role's session — fixed text: "User: ", " is not authorized to perform: kms:Decrypt on resource: ", " because no identity-based policy allows the kms:Decrypt action"; variable parts from the request/state: the assumed-role ARN "arn:aws:sts::<account>:assumed-role/<parent role name>/<session name>" and the child key's ARN. Both legs are terminal quickly; nothing stays RUNNING.

Emulator: the scenario produces no verdict at all — the check is killed by the harness with "Check timed out after 60.0s" after creating the CMK, the three roles, the CMK-encrypted child and the two parents. Nothing in the parent/child pair settles inside the budget: the parent's nested .sync:2 task never yields a terminal parent execution (DescribeExecution keeps answering RUNNING until the budget is gone), so neither the granted control's SUCCEEDED/output nor the denied role's FAILED/States.TaskFailed + kms:Decrypt denial cause is ever observable. Hypothesis: the `states:startExecution.sync:2` integration is not driven to completion (the same shape as the emulator's other unsettled .sync integrations), and consequently the parent-role kms:Decrypt gate on reading a CMK-encrypted child's output is never reached.

Why: a client that composes state machines — the standard nested-workflow pattern — hangs forever instead of getting a result: the parent never completes, so orchestration built on nested .sync:2 cannot run at all, and the security-relevant differential (a parent role WITHOUT kms:Decrypt on the child's key must fail the nested task, not read the child's output) is silently unenforced rather than visibly wrong.

Fix: the Step Functions state-machine interpreter's optimized nested-execution integration (`states:startExecution.sync` / `.sync:2`) — the leg that waits for the child execution and surfaces its Output into the parent task result — and the kms:Decrypt authorization check applied in the PARENT role's context when reading a CMK-encrypted child execution's output, which must fail the task States.TaskFailed with the KMS denial cause above.

Related open issues are about other integrations or other operations and do not carry this check: 20261113T123838Z-kms-stepfunctions-3-net-new-causes-startexecution-on.md (StartExecution refusals on a CMK machine), 20261117T122437Z-kms-stepfunctions-startexecution-on-a-cmk-encrypted-state.md (role-trust aws:SourceAccount), 20261116T073653Z-kms-stepfunctions-sendtasksuccess-on-a-cmk-encrypted-execution.md (SendTaskSuccess).

Traces: kms__stepfunctions.nested-sync-parent-role-without-child-key-decrypt-denied.

## Evidence
Check: kms__stepfunctions.nested-sync-parent-role-without-child-key-decrypt-denied
Replay result: [mismatch] Error: Check timed out after 60.0s — no leg of the scenario reached a verdict.

Expected (AWS), granted control: parent execution terminal status "SUCCEEDED", output {"fromChild": "<seed>-child"}.
Expected (AWS), denied parent: terminal status "FAILED", error "States.TaskFailed", cause opening with
  "User: arn:aws:sts::<account>:assumed-role/<parent-role-name>/<session> is not authorized to perform: kms:Decrypt on resource: <child CMK ARN> because no identity-based policy allows the kms:Decrypt action"
(the session name, account, role name and key ARN are values from the request/state; the quoted sentence around them is fixed text).

Actual (emulator): no terminal execution status is ever reported for the parent — DescribeExecution does not leave RUNNING, and the check is cut off at the 60 s harness cap before any of the above can be compared.

## Steps to Reproduce
1. kms:CreateKey (ENCRYPT_DECRYPT).
2. Create a child role allowing kms:GenerateDataKey + kms:Decrypt on that key; CreateStateMachine (STANDARD) for a one-state Pass child with encryptionConfiguration {type: CUSTOMER_MANAGED_KMS_KEY, kmsKeyId: <key ARN>, kmsDataKeyReusePeriodSeconds: 60}.
3. Create two parent roles, both allowing states:StartExecution / states:DescribeExecution / states:StopExecution and events:PutTargets/PutRule/DescribeRule on StepFunctionsGetEventsForStepFunctionsExecutionRule; give ONE of them kms:Decrypt on the child's key.
4. CreateStateMachine twice (one per parent role) with a single Task state: Resource "arn:aws:states:::states:startExecution.sync:2", Parameters {"StateMachineArn.$": "$.childArn", "Input": {"seed.$": "$.seed"}}, ResultSelector {"fromChild.$": "$.Output.value"}.
5. StartExecution on the granted parent with {"childArn": <child ARN>, "seed": "<seed>"} and poll DescribeExecution: it never leaves RUNNING (AWS: SUCCEEDED with output {"fromChild": "<seed>-child"}).
6. StartExecution on the non-granted parent with the same input and poll DescribeExecution: it never leaves RUNNING (AWS: FAILED / States.TaskFailed with the kms:Decrypt denial cause).
