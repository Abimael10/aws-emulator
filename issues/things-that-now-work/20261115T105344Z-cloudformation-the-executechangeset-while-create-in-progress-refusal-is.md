Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.changeset-execute-invalid-status-rejected
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.changeset-execute-invalid-status-rejected|_wait_change_set_status#21|Timeout|status = cfn.describe_change_set(ChangeSetName=cs_name, StackName=stack_name)["Status"]
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# The ExecuteChangeSet-while-CREATE_IN_PROGRESS refusal is never reached — the macro-held change-set scenario burns its whole budget without grading a response

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteChangeSet

## Description
One net-new cause.

Cause 1 of 1: the scenario that holds a change set in CREATE_IN_PROGRESS with an AWS::CloudFormation::Macro and then calls ExecuteChangeSet never reaches a verdict; the check is cut off at its 60 s budget, so the InvalidChangeSetStatus refusal, the held-window read and the REVIEW_IN_PROGRESS no-effect read are all ungraded.

AWS: ExecuteChangeSet against a change set that is still CREATE_IN_PROGRESS is refused with HTTP 400 and the wire error code `InvalidChangeSetStatus` (the awsQuery code of InvalidChangeSetStatusException, i.e. without the `Exception` suffix), message `ChangeSet [<change-set-arn>] cannot be executed in its current status of [<status>]` — fixed text `ChangeSet [`, `] cannot be executed in its current status of [`, `]`, with the change set's own ARN and its current status as the variable parts. Smithy (cloudformation): ExecuteChangeSet declares InvalidChangeSetStatusException, errorKind client, httpError 400, awsQueryError code `InvalidChangeSetStatus` / httpResponseCode 400. The refused call must leave the change set CREATE_IN_PROGRESS and its CREATE-type stack in REVIEW_IN_PROGRESS; once the macro returns and the change set reaches CREATE_COMPLETE the identical ExecuteChangeSet succeeds and the templated resource is created.

Emulator: no response was graded at all — the replay reports `Error: Check timed out after 60.0s`. Before it can call ExecuteChangeSet the scenario must (a) deploy a macro stack of AWS::IAM::Role + AWS::Lambda::Function + AWS::CloudFormation::Macro to CREATE_COMPLETE and (b) create a change set whose template carries `Transform: <macro>` and observe it in CREATE_IN_PROGRESS while the macro's Lambda sleeps. Hypothesis: one of those two states is never reached — the macro stack never settles, or the macro-transformed change set never enters/reports CREATE_IN_PROGRESS — so the entire budget is spent polling. This is consistent with the already-open reports that lambda-backed CloudFormation providers never complete (20261112T140000Z-cloudformation-hooks-are-never-enforced-and-a-lambda-backed-custom-resource-never-completes.md) and that the macro transform path is broken (20261029T211043Z-cloudformation-lambda-ssm-cloudformation-a-failed-ssm-dynamic-reference-produces-no.md, which carries cfn-macro-transform-injects-working-resource); neither issue carries this check's replay key.

Why: a client that executes a change set too early gets no verdict from the emulator for the one refusal that tells it to wait — and because the macro-held window is never established, no caller can even observe a change set in CREATE_IN_PROGRESS. A deployment tool that polls a change set and retries ExecuteChangeSet on `InvalidChangeSetStatus` cannot be exercised at all.

Fix: the AWS::CloudFormation::Macro invocation path used by CreateChangeSet (the change set must stay CREATE_IN_PROGRESS while the macro's function runs and then reach CREATE_COMPLETE), and the ExecuteChangeSet status guard that must answer 400 / `InvalidChangeSetStatus` with the message above while the change set is non-terminal.

Traces: cloudformation.changeset-execute-invalid-status-rejected

## Evidence
cloudformation.changeset-execute-invalid-status-rejected — Error: Check timed out after 60.0s (verdict: mismatch). No HTTP response was graded.

Expected (AWS, per the recorded scenario and the Smithy model):
  ExecuteChangeSet(ChangeSetName=<cs>, StackName=<stack>) while the change set is CREATE_IN_PROGRESS -> HTTP 400, Error.Code `InvalidChangeSetStatus`, Error.Message `ChangeSet [<change-set-arn>] cannot be executed in its current status of [CREATE_IN_PROGRESS]`; DescribeChangeSet still CREATE_IN_PROGRESS; DescribeStacks still REVIEW_IN_PROGRESS; after the macro returns, the same call succeeds and the templated AWS::SNS::Topic is CREATE_COMPLETE.
Actual (emulator): the scenario is cut off before any of those reads — the macro stack (AWS::IAM::Role + AWS::Lambda::Function + AWS::CloudFormation::Macro) and/or the macro-transformed change set never reach the state the scenario waits for.

## Steps to Reproduce
1. CreateStack (Capabilities=[CAPABILITY_IAM]) with AWS::IAM::Role, AWS::Lambda::Function (python3.12, handler sleeps ~90 s then returns {requestId, status: success, fragment}) and AWS::CloudFormation::Macro pointing at that function; wait for CREATE_COMPLETE.
2. CreateChangeSet(ChangeSetType=CREATE) for a new stack whose template has `Transform: <macro name>` and one AWS::SNS::Topic.
3. Poll DescribeChangeSet for Status == CREATE_IN_PROGRESS (the macro's sleep holds it there).
4. Call ExecuteChangeSet in that window and grade the refusal; then DescribeChangeSet (still CREATE_IN_PROGRESS) and DescribeStacks (still REVIEW_IN_PROGRESS).
5. Observe that steps 1-3 never reach their awaited state, so the check is cut off before step 4.
6. Re-replay cloudformation.changeset-execute-invalid-status-rejected.
