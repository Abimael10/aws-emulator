Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.redeploy-with-changed-template-applies-the-change cloudformation.redeploy-broken-importvalue-rolls-back-preserving-original
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.redeploy-with-changed-template-applies-the-change|_deploy#15|Timeout|result = subprocess.run(
Issue-Key: cloudformation.redeploy-broken-importvalue-rolls-back-preserving-original|_deploy_allow_failure#16|Timeout|result = subprocess.run(
Issue-Commit: 0c065617979d1ee76fa77b423bd58183fc04c861

# cloudformation: a second `aws cloudformation deploy` onto an existing stack never settles — the CLI blocks until the check times out

> _Resolution note (added when this report was moved): the reported defect does NOT exist —
> the stack-update state machine behind `ExecuteChangeSet` already drives an executed `UPDATE`
> change set to `UPDATE_COMPLETE`, and one whose `Fn::ImportValue` names an export nothing
> publishes to `UPDATE_ROLLBACK_COMPLETE`, both published by `DescribeStacks`. The hypothesis
> in the Description below ("the stack is parked in `UPDATE_IN_PROGRESS`") was measured and is
> wrong. Two suites added with this note pin the behaviour end to end —
> `services/service-com-amazonaws-cloudformation-cloudformation/tests/all/redeployed_stack_terminal_status.rs`
> and `core/router/tests/all/cfn_redeploy_reaches_a_terminal_status.rs`, the latter on the
> whole assembled router with the added queue read back through Amazon SQS's own wire._
>
> _What the two checks time out on is the CLIENT's poll cadence against the harness's 60 s
> budget, not a status the emulator withholds. Instrumenting the running emulator gave this
> call timeline (seconds from the check's first CloudFormation request; the client was the
> harness's own AWS CLI, whose boto3 reported `Boto3/1.43.108`):_
>
> | t (s) | call | what the emulator answered |
> |---|---|---|
> | 0.0 | `DescribeStacks` | the stack does not exist (the CLI's `has_stack`) |
> | 0.0 | `CreateChangeSet` | the `CREATE` change set |
> | 0.0 | `DescribeChangeSet` | `CREATE_PENDING` — the waiter's first poll |
> | 5.0 | `DescribeChangeSet` | `CREATE_COMPLETE` — its second poll, 5 s later |
> | 5.0 | `ExecuteChangeSet` | accepted |
> | 5.0 | `DescribeStacks` | `CREATE_IN_PROGRESS` — the waiter's first poll |
> | 35.1 | `DescribeStacks` | `CREATE_COMPLETE` — its second poll, **30 s** later |
> | 35.7 | `DescribeStacks` / `CreateChangeSet` | the second `deploy` starts |
> | 35.7, 40.7 | `DescribeChangeSet` | `CREATE_PENDING`, then `CREATE_COMPLETE` |
> | 40.7 | `ExecuteChangeSet` | accepted; the stack reads `UPDATE_IN_PROGRESS` with `UPDATE_COMPLETE` due in 494 ms, no wait conditions, no open stabilisations, nothing owed |
> | 40.7 | `DescribeStacks` | `UPDATE_IN_PROGRESS`, 8 ms from its terminal status — and the waiter's next poll falls at 70.7 s, past the 60 s budget |
>
> _So the CLI's `stack_create_complete` / `stack_update_complete` waiter polls immediately —
> ~12 ms after `ExecuteChangeSet` — and then every 30 s, while its change-set waiter polls
> every 5 s. The emulator publishes a stack operation's terminal status one
> `STATUS_TRANSITION_DELAY` (500 ms) after the call that opens it, so that first poll
> necessarily misses and every deploy costs the client one whole 30 s interval: two deploys
> need ≥60 s of client-side sleep before any CLI start-up. Shrinking the span to 20 ms was
> tried: the change-set waiter then hit on its first poll, but the stack waiter's first poll
> still arrived 13 ms after the operation opened with 7 ms to run, so the 30 s sleep remained
> and both checks still timed out. Making that first poll hit needs a ~0 span, which removes
> the `*_IN_PROGRESS` status this emulator publishes on purpose (see the doc on
> `STATUS_TRANSITION_DELAY` in `src/state.rs`) and which many of this crate's suites pin. The
> cause is therefore declined rather than fixed, and `replay_issue_units` still reports both
> units as `Timeout`._

> _Note: this issue was identified against the code base at commit `0c065617979d1ee76fa77b423bd58183fc04c861`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteChangeSet

## Description
One net-new root cause, grounded in two failing checks.

Cause 1 of 1: the update path `aws cloudformation deploy` takes against a stack that already exists (create change set, execute it, wait for a terminal stack status) never reaches a terminal state, so the CLI never returns.

AWS: recorded behavior only (the CLI's `deploy` is a composite of CreateChangeSet, ExecuteChangeSet and DescribeStacks/DescribeStackEvents; nothing in the Smithy model governs the composite). The recording shows a re-`deploy` of a changed template against the stack the first `deploy` created reaching UPDATE_COMPLETE, with the added resource really usable; and a re-`deploy` whose new resource imports a never-declared export exiting non-zero, rolling the stack back to UPDATE_ROLLBACK_COMPLETE with a reason that names the missing export, leaving the original resource untouched and the new one never created, after which a third valid `deploy` still succeeds. In both cases the command terminates.

Emulator: neither check reaches any AWS-level assertion — the harness reports `Error: Check timed out after 60.0s` for both. The first `deploy` is not what hangs: other CLI-driven CloudFormation checks in this suite do reach assertions (cloudformation.cli-script-cfn-deploy-and-verify fails on a real comparison, not a timeout). It is specifically the second `deploy` — the change-set-driven UPDATE of an existing stack — that never settles. Hypothesis: the executed UPDATE change set leaves the stack parked in UPDATE_IN_PROGRESS (and, in the broken-ImportValue case, never transitions to UPDATE_ROLLBACK_COMPLETE), so the CLI's built-in waiter polls forever.

Why: a client's deployment pipeline blocks indefinitely on every stack update and never learns the outcome. Because no terminal status is ever published, the caller cannot distinguish success from failure, cannot roll back, and cannot retry without risking a double update. A failed update also never surfaces the missing-export reason a client needs to fix its template.

Fix: the CloudFormation stack-update state machine behind ExecuteChangeSet — an executed UPDATE change set must drive the stack to UPDATE_COMPLETE, and an update whose resource cannot be resolved (an Fn::ImportValue of an export that does not exist) must drive it to UPDATE_ROLLBACK_COMPLETE with the failing resource's reason naming the missing export, in both cases publishing the terminal status DescribeStacks reports.

Traces: cloudformation.redeploy-with-changed-template-applies-the-change, cloudformation.redeploy-broken-importvalue-rolls-back-preserving-original.

## Evidence
Expected (happy path): the second `aws cloudformation deploy` returns 0 and DescribeStacks reports UPDATE_COMPLETE, with the added queue able to send and receive.
Expected (rejection path): the second `aws cloudformation deploy` exits non-zero, DescribeStacks reports UPDATE_ROLLBACK_COMPLETE, the failing resource's ResourceStatusReason names the never-declared export, the pre-existing resource is untouched and the new one was never created.

Actual (suite report, both checks, verdict mismatch):
```
cloudformation.redeploy-with-changed-template-applies-the-change
Error: Check timed out after 60.0s

cloudformation.redeploy-broken-importvalue-rolls-back-preserving-original
Error: Check timed out after 60.0s
```
No stack status, no exit code and no reason text were ever observed — the commands did not terminate.

## Steps to Reproduce
1. `aws cloudformation deploy` a template with one SQS queue; it reaches CREATE_COMPLETE.
2. `aws cloudformation deploy` the same stack again with a template that adds a second queue. The CLI never returns; DescribeStacks stays in UPDATE_IN_PROGRESS.
3. Repeat step 2 with a template whose added resource has a tag value of `Fn::ImportValue` naming an export that was never declared. The CLI never returns and the stack never reaches UPDATE_ROLLBACK_COMPLETE.
