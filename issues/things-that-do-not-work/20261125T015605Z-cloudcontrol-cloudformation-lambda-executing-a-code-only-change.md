Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudcontrol__cloudformation__lambda.lambda-function-code-zipfile-writeonly-update-hidden-from-cloudcontrol
Issue-Key: cloudcontrol__cloudformation__lambda.lambda-function-code-zipfile-writeonly-update-hidden-from-cloudcontrol|run#53|AssertionError|assert survived == marker_value, (
Issue-Commit: 6bae9ee14af172aae629fb16389ee320a8de1819

# cloudcontrol__cloudformation__lambda: executing a Code-only change set (Replacement=False / RequiresRecreation=Never) on AWS::Lambda::Function does not update in place — the function's out-of-band tags are gone afterwards

> _Note: this issue was identified against the code base at commit `6bae9ee14af172aae629fb16389ee320a8de1819`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteChangeSet

## Description
Cause 1 of 1: a CloudFormation stack update that changes only AWS::Lambda::Function Code.ZipFile is correctly classified as a non-replacing modification, but executing it does not preserve the existing function object: state held on the function outside the template (its tags) is lost. Failing check: cloudcontrol__cloudformation__lambda.lambda-function-code-zipfile-writeonly-update-hidden-from-cloudcontrol.

AWS: Code is not a create-only property of AWS::Lambda::Function, so a change set that touches only Code reports Action=Modify, Replacement=False and Details[0].Target.RequiresRecreation=Never (the emulator already reports exactly this, and the check's assertions on the change set pass). Executing such a change set performs an in-place update of the SAME function object — AWS calls UpdateFunctionCode; it does not delete and recreate the function. Consequently everything attached to that function and not managed by the template survives the update, including tags applied out of band through Lambda's own API (Smithy, lambda: TagResource takes Resource=<function ARN> + Tags map; ListTags returns "The function's tags"). A tag set with TagResource before the update is still returned by ListTags on the same function ARN after the update. For the recorded scenario AWS returns the marker tag's value unchanged; recorded behavior only for the precise retention rule, which the model does not state.

Emulator: the change set is created and classified correctly, the stack reaches UPDATE_COMPLETE, and the new code takes effect (invoke returns 'v2'), but ListTags(Resource=<the function ARN captured before the update>) no longer carries the tag that was applied with TagResource before the update — the member is simply absent from the Tags map. The check fails on `assert survived == marker_value` with: "the out-of-band marker tag must survive an in-place update; expected <marker>, got None (gone would mean the function was actually destroyed and recreated despite Replacement=False)". The expected value is a per-run generated name; the requirement is that whatever value TagResource stored is still reported, not that one literal string. No error status accompanies this: every call returns 2xx, so the loss is silent. Hypothesis: the AWS::Lambda::Function update path implements a Code change as a delete+create of the function (or otherwise rebuilds the function's state record), discarding the tag map instead of calling the in-place code update; an alternative cause with the same symptom is that TagResource/ListTags do not persist tags across any function-configuration mutation. Both live in the same state.

Why: a client is told by the change set that nothing will be recreated (Replacement=False, RequiresRecreation=Never) and then loses resource state that only an in-place update is allowed to keep. Cost-allocation and access-control tags, aliases/permissions and anything else attached out of band silently disappear on an ordinary code deploy; tag-based IAM conditions and tag-driven billing/automation stop matching a function that is still reported healthy and UPDATE_COMPLETE. There is no signal — no failure, no drift, no error code — that the resource identity was not preserved.

Fix: the CloudFormation AWS::Lambda::Function resource provider's update path (the handler that applies a non-replacing Modify to a Lambda function) must mutate the existing function in place via the function's code-update path and must not reconstruct the function record; and the Lambda function state's tag map must be retained across configuration/code updates, so TagResource-applied tags remain visible to ListTags for the same function ARN.

Traces: cloudcontrol__cloudformation__lambda.lambda-function-code-zipfile-writeonly-update-hidden-from-cloudcontrol (create stack with Code.ZipFile v1, TagResource a marker on the function, create+execute a change set whose only diff is Code.ZipFile v2, then ListTags on the same function ARN). The check's later assertions — that GetResource still reports populated FunctionName/Runtime/Handler/Timeout/Role and Code == {} after the update — were never reached and are unverified.

## Evidence
Check cloudcontrol__cloudformation__lambda.lambda-function-code-zipfile-writeonly-update-hidden-from-cloudcontrol, line 273:
  survived = lam.list_tags(Resource=function_arn)["Tags"].get("sim2real-identity-marker")
  assert survived == marker_value
AssertionError: the out-of-band marker tag must survive an in-place update; expected 'sim2real-behav-lamwo-marker-<run id>', got None (gone would mean the function was actually destroyed and recreated despite Replacement=False)

Passing before the failure (so the classification itself is right): change set Action='Modify', Replacement='False', Details[0].Target={Name:'Code', Attribute:'Properties', RequiresRecreation:'Never'}, Evaluation='Static', ChangeSource='DirectModification'; stack reaches UPDATE_COMPLETE; invoke returns 'v2'; GetResource before the update reports Code == {}.

Expected (AWS): ListTags(Resource=<function ARN>) after the update still returns the tag key/value that TagResource stored before it.
Actual (emulator): that key is absent from the Tags map; HTTP 200, no error code, no diagnostic.

## Steps to Reproduce
1. CreateStack with an AWS::IAM::Role (lambda.amazonaws.com trust) and an AWS::Lambda::Function (Runtime python3.12, Handler index.handler, Timeout 10, Code.ZipFile = "def handler(event, context):\n    return 'v1'\n"), Capabilities=[CAPABILITY_IAM]; wait for CREATE_COMPLETE.
2. lambda GetFunctionConfiguration to capture FunctionArn; lambda TagResource(Resource=<FunctionArn>, Tags={"sim2real-identity-marker": "<marker>"}).
3. lambda ListTags(Resource=<FunctionArn>) — the marker is present.
4. CreateChangeSet on the same stack with an identical template except Code.ZipFile = "...return 'v2'\n"; wait CREATE_COMPLETE and note Replacement=False / RequiresRecreation=Never.
5. ExecuteChangeSet; wait for UPDATE_COMPLETE; lambda Invoke returns 'v2'.
6. lambda ListTags(Resource=<FunctionArn>) again — the marker key is gone. AWS still reports it.
