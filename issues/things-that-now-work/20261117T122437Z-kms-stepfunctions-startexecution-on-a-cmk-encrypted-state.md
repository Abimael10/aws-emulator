Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__stepfunctions.sfn-execution-role-trust-source-account-mismatch-blocks-cmk-execution
Issue-Key: kms__stepfunctions.sfn-execution-role-trust-source-account-mismatch-blocks-cmk-execution|_start_until_permanent_failure#7|Timeout|raise
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms__stepfunctions: StartExecution on a CMK-encrypted state machine whose role trust names another account's aws:SourceAccount neither refuses nor completes (check hangs)

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: an execution whose role can never be assumed is not refused, and the call/execution never settles. Sibling of the open issue 20261113T123838Z-kms-stepfunctions-3-net-new-causes-startexecution-on.md (which carries the source-account/source-arn honouring and the deny/disabled cases); this is the foreign-aws:SourceAccount mismatch case, which that issue's replay key does not cover.
  AWS: recorded behavior only — a state machine whose execution role trust policy requires aws:SourceAccount equal to a DIFFERENT account can never be assumed by Step Functions, so StartExecution on a CMK-encrypted STANDARD machine is refused promptly with KmsAccessDeniedException naming the denial, while the same-account twin succeeds and the execution reaches SUCCEEDED.
  Emulator: the check never gets an answer — it is reported as 'Check timed out after 60.0s'. Neither the expected refusal nor a terminal execution is produced within the budget, so the trust/KMS denial path hangs instead of erroring. Hypothesis: the StartExecution path neither evaluates the role trust's aws:SourceAccount condition nor fails the KMS data-key step, leaving the execution pending forever.
  Why: a client that expects a fast, typed refusal when its execution role cannot be assumed blocks instead: the API call (or the caller's wait for a terminal execution status) hangs, which turns a deterministic permission error into a timeout and stalls anything orchestrating executions.
  Fix: the StartExecution handler's role-assumption and CMK data-key path — evaluate the trust policy's aws:SourceAccount condition against the calling account, refuse with KmsAccessDeniedException, and ensure no execution is left non-terminal.
  Traces: kms__stepfunctions.sfn-execution-role-trust-source-account-mismatch-blocks-cmk-execution.

## Evidence
sfn-execution-role-trust-source-account-mismatch-blocks-cmk-execution — Error: Check timed out after 60.0s. The check creates a CMK, a role whose trust requires aws:SourceAccount = '111122223333' (a foreign account), and a CMK-encrypted STANDARD state machine, then expects StartExecution to raise KmsAccessDeniedException naming the denial while a same-account twin SUCCEEDS; no answer arrived.

## Steps to Reproduce
1. Create a CMK and a Step Functions execution role whose trust policy allows states.amazonaws.com with Condition StringEquals aws:SourceAccount = '111122223333' (an account other than the caller's).
2. Create a STANDARD state machine (definition {"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}}) with that role and encryptionConfiguration CUSTOMER_MANAGED_KMS_KEY on the CMK.
3. sfn.start_execution(stateMachineArn=...) -> expect a prompt KmsAccessDeniedException naming the denial, not a hang.
4. Repeat with the trust's aws:SourceAccount set to the caller's own account -> expect the execution to start and reach SUCCEEDED.
