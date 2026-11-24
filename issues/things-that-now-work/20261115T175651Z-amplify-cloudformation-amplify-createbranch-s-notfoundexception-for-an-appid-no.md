Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: amplify__cloudformation.deploy-amplify-branch-with-unknown-app-id-rejected-and-rolled-back
Issue-Key: amplify__cloudformation.deploy-amplify-branch-with-unknown-app-id-rejected-and-rolled-back|run#11|AssertionError|assert got == _REJECTED, f"expected (message, handler code) {_REJECTED!r}, got {got!r}"
Issue-Commit: 7159003333263c232287cf539a8a92dd610dcec6

# amplify: CreateBranch's NotFoundException for an appId no app owns is an emulator-authored message instead of AWS's "Resource of type 'AWS::Amplify::Branch' with identifier 'null' was not found."

> _Note: this issue was identified against the code base at commit `7159003333263c232287cf539a8a92dd610dcec6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBranch

## Description
One cause (Cause 1 of 1).

Cause 1 of 1: the not-found refusal Amplify makes when CreateBranch names an appId that exists in shape but belongs to no app carries emulator-authored prose, so the CloudFormation AWS::Amplify::Branch handler reports a failure reason AWS never words that way. Failing check: amplify__cloudformation.deploy-amplify-branch-with-unknown-app-id-rejected-and-rolled-back

AWS: Smithy (com-amazonaws-amplify-amplify): CreateBranch binds POST /apps/{appId}/branches with appId @httpLabel @required, @length 1..20, @pattern ^d[a-z0-9]+$, and declares NotFoundException (errorKind client, httpError 404) among its errors. An appId that satisfies that pattern but names no application therefore reaches the service and is refused with NotFoundException, not rejected as malformed. Recorded behavior: that refusal's message is the fixed sentence "Resource of type 'AWS::Amplify::Branch' with identifier 'null' was not found." — the quoted type is the resource type the call is creating and the quoted identifier is the branch identifier Amplify has at that point (literally null, because no branch was created); the rest of the sentence is fixed text. CloudFormation wraps the service error into the resource's CREATE_FAILED ResourceStatusReason and the AWS::Amplify::Branch handler classifies it with HandlerErrorCode NotFound, so the stack ends ROLLBACK_COMPLETE with that message.

Emulator: the handler code is right (NotFound) but the message is replaced by emulator-authored text that names the request's app id: the replay recorded 'App d0e8e2c3e2eef6 not found.' where the app id is the value taken from the request. The HTTP status and error shape are otherwise as expected; only the message diverges.

Why: a client (or a CloudFormation consumer reading ResourceStatusReason) that matches AWS's documented not-found wording — the "Resource of type '<type>' with identifier '<id>' was not found." form Amplify uses — never matches, so error-classification and retry/rollback-diagnosis logic keyed on that sentence silently falls through to a generic failure path even though the status and code are correct.

Fix: the amplify CreateBranch handler's app-lookup refusal path — the NotFoundException message it constructs (not the status or the error shape, which are already right).

Traces: amplify__cloudformation.deploy-amplify-branch-with-unknown-app-id-rejected-and-rolled-back (the check's positive control, the same template given its own app's Fn::GetAtt AppId, passes, so only the refusal wording is at issue).

## Evidence
Check amplify__cloudformation.deploy-amplify-branch-with-unknown-app-id-rejected-and-rolled-back, [mismatch]:

AssertionError: expected (message, handler code) ("Resource of type 'AWS::Amplify::Branch' with identifier 'null' was not found.", 'NotFound'), got ('App d0e8e2c3e2eef6 not found.', 'NotFound')

Expected (AWS): CREATE_FAILED ResourceStatusReason wrapping Amplify's 404 NotFoundException whose message is "Resource of type 'AWS::Amplify::Branch' with identifier 'null' was not found."; HandlerErrorCode NotFound.
Actual (emulator): same HandlerErrorCode NotFound, message 'App <appId> not found.' where <appId> is the unknown app id from the request ('d0e8e2c3e2eef6' in this run — a per-run generated value, not part of the requirement).

## Steps to Reproduce
1. CreateStack with a template declaring AWS::Amplify::App and AWS::Amplify::Branch, where the Branch's AppId is a value matching ^d[a-z0-9]+$ that no app owns (e.g. "d0<suffix>") and BranchName is "main".
2. Wait for the stack to reach a terminal status: it correctly reaches ROLLBACK_COMPLETE.
3. DescribeStackEvents and read the ResourceStatusReason on the Branch resource's single CREATE_FAILED row.
4. Expected: the resource-handler message is "Resource of type 'AWS::Amplify::Branch' with identifier 'null' was not found." with HandlerErrorCode NotFound. Observed: the message is 'App <appId> not found.'
5. Equivalent direct call: amplify CreateBranch with appId=<unknown-but-well-formed id>, branchName=main — the 404 NotFoundException body carries the same divergent message.
