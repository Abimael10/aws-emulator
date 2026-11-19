Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__lambda__logs.e2e-diagnose-onyx-lambda-processing-failure-lifecycle
Issue-Key: dynamodb__lambda__logs.e2e-diagnose-onyx-lambda-processing-failure-lifecycle|_reap_log_group#10|Timeout|return
Issue-Commit: 73f43511fc01379d7b1558031a9504e64eee8ef3

# dynamodb__lambda__logs: the two-stream Onyx stream-processing lifecycle reaches no verdict — the deploy phase never converges and the check hangs to the 60 s cap

> _Resolution: the cause as REPORTED — "the deploy phase never converges", i.e. no terminal
> stack status and no stream delivery — is not present at this commit, and
> `core/router/tests/all/cfn_multi_stream_lambda_logged_lifecycle.rs` now pins what the report
> says AWS does: the CreateStack of this estate reaches a terminal `CREATE_COMPLETE` with every
> declared resource created, `ListEventSourceMappings` reports one mapping per declared table
> stream with the declared `StartingPosition`/`BatchSize`/`MaximumRetryAttempts`, and a write to
> each table is delivered to the handler whose per-record line lands in `/aws/lambda/<function>`
> — over one, two and three streams. What remains is the recorded run's WALL CLOCK: the
> six-phase scenario does not finish inside the harness's own 60 s cap, which this report itself
> records as "recorded behavior only" with no AWS-published latency, so no AWS rule fixes it.
> That residual is NOT claimed to be free of emulator-side contributors: one measured on the way
> — a stack bringing up its INDEPENDENT resources one coming-up window apart instead of together
> — is filed as
> `issues/things-that-do-not-work/20261119T000000Z-cloudformation-independent-stack-resources-come-up-one-window-apart.md`,
> together with the companion fix in the stack's failed-create bookkeeping that it needs, and the
> replay of this check will keep timing out until a later change makes the whole lifecycle fit
> the cap._

> _Note: this issue was identified against the code base at commit `73f43511fc01379d7b1558031a9504e64eee8ef3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the CloudFormation-deployed, two-stream DynamoDB->Lambda estate never becomes observable: the check's first phase (CreateStack of the Onyx stream-handler template, polled with DescribeStacks to a terminal status, then arming the two StartingPosition=LATEST event source mappings) does not converge, and the whole scenario is cut off with no verdict. Failing check: dynamodb__lambda__logs.e2e-diagnose-onyx-lambda-processing-failure-lifecycle.

AWS: a CreateStack of this template — two AWS::DynamoDB::Table resources with StreamSpecification NEW_AND_OLD_IMAGES, an AWS::IAM::Role, one AWS::Lambda::Function with inline Code.ZipFile, and two AWS::Lambda::EventSourceMapping resources (StartingPosition LATEST, BatchSize 1, MaximumRetryAttempts 3, one per table stream) — reaches a terminal status promptly, and that status is CREATE_COMPLETE on success (Smithy: com.amazonaws.cloudformation#StackStatus enumerates CREATE_IN_PROGRESS -> CREATE_COMPLETE, with ROLLBACK_COMPLETE only after a resource create fails, in which case DescribeStackEvents carries the failing resource's ResourceStatusReason). ListEventSourceMappings(FunctionName=<function>) then returns the two mappings for the two tables' stream ARNs (Smithy: lambda#ListEventSourceMappings, GET /2015-03-31/event-source-mappings, 200, output member EventSourceMappings), each armed mapping delivers subsequent table writes to the handler, and the handler's per-record lines land in /aws/lambda/<function-name>. AWS bounds none of this with a published latency, so the time budget itself is recorded behavior only: the recording shows every other check in this service group settling well inside the harness's 60 s cap (8 of 9 pass).

Emulator: the check produces no phase outcome at all. The only diagnostic the replay captured is `Error: Check timed out after 60.0s` — no status, no error code, and not even a phase line from the journal the check dumps on cleanup, so the deploy phase is still polling when the cap fires. Nothing after it (the seeded-estate scan, the two reproducible BAT/bulk probes, the live-derived diagnosis reads GetFunction/GetFunctionConfiguration/ListEventSourceMappings/DescribeTable, cleanup and destroy) is ever exercised. Hypothesis: either DescribeStacks never leaves a non-terminal CREATE_IN_PROGRESS for this template, or the LATEST mappings never deliver a record so the arming loop spins to the cap; the captured output does not distinguish them.

Why: a client that deploys a DynamoDB-Streams consumer by template and then waits for it gets a call that neither succeeds nor is refused. There is no terminal stack status to branch on and no error to handle, so every automation built on "create the stack, wait for it, then write to the table" blocks forever instead of either working or failing with a reason it can read.

Fix: the CloudFormation stack-provisioning path for this resource set (the AWS::Lambda::EventSourceMapping provider including two mappings in one stack with StartingPosition LATEST, and the AWS::DynamoDB::Table stream attributes they depend on) must drive the stack to a terminal status, and the DynamoDB-Streams -> Lambda event-source delivery path must arm and deliver, so the deploy phase terminates one way or the other; the create-failure path must also record a ResourceStatusReason on the failing resource's stack event. Related issues do not cover this failure: open 20261106T020636Z-dynamodb-lambda-logs-the-cloudformation-create-of-the.md describes the same check failing FAST with DescribeStacks reporting ROLLBACK_COMPLETE (a terminal refusal, a different replay key); the behavior now recorded is a non-terminating deploy with no status at all. Fixed 20261104T044826Z-dynamodb-lambda-logs-dynamodb-streams-records-never-reach-the-mapped-lambda.md covers stream delivery for API-created mappings, not this template-created pair.

Traces: dynamodb__lambda__logs.e2e-diagnose-onyx-lambda-processing-failure-lifecycle (phase 1/6, deploy).

## Evidence
Expected (AWS): CreateStack of the Onyx template -> DescribeStacks reaches StackStatus "CREATE_COMPLETE"; Outputs name the two tables (<tok>-BasaltRequests-alpha, <tok>-BasaltRegressionRequests-alpha), the function and the log group /aws/lambda/<function>; ListEventSourceMappings(FunctionName=<function>) -> two mappings whose EventSourceArns are the two tables' stream ARNs with {(StartingPosition, BatchSize, MaximumRetryAttempts)} == {("LATEST", 1, 3)}; once armed, a table write is delivered to the handler and its per-record line appears in the function's log group. The stack name, table-name token and function name are per-run variable parts; the status value and the mapping members are the requirement.

Actual (emulator): the check reaches no verdict. Replay detail, in full: "Error: Check timed out after 60.0s". No HTTP status, no error code, no error message and no phase line are produced — the deploy phase is still polling (DescribeStacks to a terminal status, then the LATEST-mapping arming loop) when the harness cap fires, so phases 2-6 (setup, pre_invoke, solve, cleanup, destroy) never run. 8 of the 9 checks in this service group settle inside the same cap, so the cap is not the obstacle for this emulator in general.

## Steps to Reproduce
1. Boot the emulator. 2. CreateStack with the Onyx stream-handler template: two AWS::DynamoDB::Table resources with StreamSpecification NEW_AND_OLD_IMAGES, an AWS::IAM::Role for the function, one AWS::Lambda::Function with inline Code.ZipFile, and two AWS::Lambda::EventSourceMapping resources (one per table stream) with StartingPosition LATEST, BatchSize 1, MaximumRetryAttempts 3. 3. Poll DescribeStacks for the stack: observe that it does not settle on a terminal StackStatus. 4. If it does settle, call ListEventSourceMappings(FunctionName=<function>) and then PutItem into each table, polling the function's log group for a per-record line: observe no delivery, so an arming loop never completes. 5. Re-replay dynamodb__lambda__logs.e2e-diagnose-onyx-lambda-processing-failure-lifecycle and confirm it now reaches a verdict instead of timing out at the 60 s cap.
