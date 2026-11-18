Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lambda.deploy-sam-function-url-cors-config-round-trips-without-a-public-permission cloudformation__lambda.sam-autopublishalias-function-url-targets-only-the-alias cloudformation__lambda.cfnuf-change-set-on-update-failed-stack-diffs-against-the-rolled-back-state cloudformation__lambda.cfn-lambda-function-invoke-runs-handler-code-and-reports-unhandled-errors
Issue-Key: cloudformation|CreateChangeSet|ValidationError
Issue-Key: cloudformation__lambda.cfnuf-change-set-on-update-failed-stack-diffs-against-the-rolled-back-state|run#5|AssertionError|assert stable["Status"] == "FAILED" and stable["Changes"] == [], (
Issue-Key: cloudformation__lambda.cfn-lambda-function-invoke-runs-handler-code-and-reports-unhandled-errors|run#25|AssertionError|assert message == want, f"unexpected message {message!r}, wanted {want!r}"
Issue-Commit: 6ad539f79015bc27d2d0777d4c5c145ae7d45c50

# cloudformation__lambda: 3 causes — the SAM transform never generates the AWS::Lambda::Url resource for FunctionUrlConfig, a no-change change set on an UPDATE_FAILED stack is CREATE_COMPLETE with phantom Modifies, and Invoke's not-found message drops ':$LATEST'

> _Note: this issue was identified against the code base at commit `6ad539f79015bc27d2d0777d4c5c145ae7d45c50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChangeSet

## Description
Three independent net-new root causes on CloudFormation stacks that carry Lambda resources. ALL THREE must be closed before this issue is submitted.

Cause 1 of 3: the AWS::Serverless-2016-10-31 transform does not expand a function's FunctionUrlConfig into the generated AWS::Lambda::Url resource, so any Fn::GetAtt on it is rejected as an undefined resource.
AWS: for an AWS::Serverless::Function with logical id <Fn> and a FunctionUrlConfig property, the SAM transform generates an AWS::Lambda::Url resource with logical id <Fn>Url (AWS SAM developer guide, 'Generated AWS CloudFormation resources for Lambda functions'). Fn::GetAtt <Fn>Url.FunctionUrl therefore resolves, CreateChangeSet succeeds, and the stack's changes list the generated AWS::Lambda::Url alongside the AWS::Lambda::Function; an AuthType AWS_IAM URL generates NO AWS::Lambda::Permission beside it. This is documented transform behavior; the CloudFormation Smithy model does not describe the transform.
Emulator: CreateChangeSet is refused with HTTP 400 ValidationError and the message 'Template error: instance of Fn::GetAtt references undefined resource FnUrl' (fixed text 'Template error: instance of Fn::GetAtt references undefined resource ', variable part the generated logical id SAM should have created — here <Fn>Url). Nothing is deployed at all.
Why: every SAM application that exposes a function URL fails at change-set creation. The caller cannot deploy, cannot read the URL back from an Output, and cannot test the Cors/AuthType round trip — the whole SAM function-URL surface is unreachable.
Fix: the SAM transform in CloudFormation — expand AWS::Serverless::Function.FunctionUrlConfig into an AWS::Lambda::Url resource named <FunctionLogicalId>Url (carrying AuthType, Cors and the TargetFunctionArn, and the alias as Qualifier when AutoPublishAlias is set), register it so Ref/Fn::GetAtt resolve, and emit no AWS::Lambda::Permission for AuthType AWS_IAM.
Failing check ids: cloudformation__lambda.deploy-sam-function-url-cors-config-round-trips-without-a-public-permission, cloudformation__lambda.sam-autopublishalias-function-url-targets-only-the-alias.

Cause 2 of 3: a change set whose template already matches the live (rolled-back) state of an UPDATE_FAILED stack is computed as CREATE_COMPLETE with changes, instead of FAILED with no changes.
AWS: CreateChangeSet that yields no difference against the live stack state leaves the change set in Status FAILED with an empty Changes list, and its StatusReason is the fixed text "The submitted information didn't contain changes. Submit different information to create a change set." (Smithy: DescribeChangeSet output members Status/StatusReason/Changes, services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json; ExecuteChangeSet on it then answers InvalidChangeSetStatus 'ChangeSet [<change-set-id>] cannot be executed in its current status of [FAILED]'). After an UPDATE_FAILED the live state is the rolled-back one, so the last stable template is a no-change diff.
Emulator: the same request produces Status 'CREATE_COMPLETE' with four phantom in-place Modify changes [('Role','Modify','False',[None]), ('Fn0','Modify','False',[None]), ('Fn0Version','Modify','False',[None]), ('Fn0Alias','Modify','False',[None])] — every resource reported as modified with no changed property named.
Why: the change set is CloudFormation's preview contract. A caller reviewing the diff before executing sees four resources about to be touched when nothing will change (and, in the inverse case, cannot rely on the no-change refusal to detect drift-free state). Executing it is permitted where AWS refuses it, so a client's 'is there anything to deploy?' gate always answers yes.
Fix: CloudFormation's change-set diff engine — diff the submitted template against the stack's live resource state (the rolled-back state after UPDATE_FAILED), emit a Modify row only when a property actually differs and name it in the row, and when the diff is empty finish the change set as FAILED with the no-changes StatusReason.
Failing check ids: cloudformation__lambda.cfnuf-change-set-on-update-failed-stack-diffs-against-the-rolled-back-state.

Cause 3 of 3: Lambda Invoke's 'Function not found' message names the UNQUALIFIED function ARN.
AWS: Invoke on a function that does not exist answers HTTP 404 ResourceNotFoundException (Smithy: Invoke -> ResourceNotFoundException, errorKind client, httpError 404) with Message = fixed text 'Function not found: ' + the fully qualified ARN arn:<partition>:lambda:<region>:<account-id>:function:<name> plus the resolved qualifier (':$LATEST' when the request carried no Qualifier). Variable parts come from the request and credential scope; the wording is recorded behavior only.
Emulator: same status and code, Message 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' with no ':$LATEST'.
Why: a client asserting that a stack-managed function is gone, or deriving the qualified ARN it invoked from the error, reads a different identity than AWS reports.
Fix: the Invoke handler's not-found path in the lambda crate (and the shared 'Function not found: ' message helper) must append the resolved qualifier.
Failing check ids: cloudformation__lambda.cfn-lambda-function-invoke-runs-handler-code-and-reports-unhandled-errors.

## Evidence
Behavioral suite replay against the emulator built from this checkout; service cloudformation__lambda 38/60 (63%).

Cause 1 — [error] cloudformation__lambda.deploy-sam-function-url-cors-config-round-trips-without-a-public-permission and [error] cloudformation__lambda.sam-autopublishalias-function-url-targets-only-the-alias:
  botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateChangeSet operation: Template error: instance of Fn::GetAtt references undefined resource FnUrl
The template is 'Transform: AWS::Serverless-2016-10-31' with resource 'Fn' of type AWS::Serverless::Function carrying FunctionUrlConfig {AuthType: AWS_IAM, Cors: {AllowOrigins: [https://example.com], AllowMethods: [GET], MaxAge: 300}} and Output 'FunctionUrl' = !GetAtt FnUrl.FunctionUrl, submitted with Capabilities CAPABILITY_IAM + CAPABILITY_AUTO_EXPAND. Expected instead: CreateChangeSet accepted, the change set listing 'Fn' as AWS::Lambda::Function and 'FnUrl' as AWS::Lambda::Url with no AWS::Lambda::Permission, and the Output equal to lambda:GetFunctionUrlConfig's FunctionUrl.

Cause 2 — [mismatch] cloudformation__lambda.cfnuf-change-set-on-update-failed-stack-diffs-against-the-rolled-back-state:
  File ...cfnuf_change_set_on_update_failed_stack..., line 31, in run
  assert stable["Status"] == "FAILED" and stable["Changes"] == []
  AssertionError: a change set of the last stable template is 'CREATE_COMPLETE' with changes [('Role', 'Modify', 'False', [None]), ('Fn0', 'Modify', 'False', [None]), ('Fn0Version', 'Modify', 'False', [None]), ('Fn0Alias', 'Modify', 'False', [None])]; the live state already matches it, so it must FAIL with no change
Expected instead: DescribeChangeSet Status 'FAILED', Changes [], StatusReason "The submitted information didn't contain changes. Submit different information to create a change set."; and ExecuteChangeSet on it refused with InvalidChangeSetStatus 'ChangeSet [<id>] cannot be executed in its current status of [FAILED]'.

Cause 3 — [mismatch] cloudformation__lambda.cfn-lambda-function-invoke-runs-handler-code-and-reports-unhandled-errors:
  AssertionError: unexpected message 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-cfninv-fn-cd9fcff73a4c', wanted 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-cfninv-fn-cd9fcff73a4c:$LATEST'
Expected instead: 404 ResourceNotFoundException with the ':$LATEST'-qualified ARN. (Region/account/name above are this run's values.)

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this branch.
2. Cause 1: CreateChangeSet (ChangeSetType CREATE, Capabilities CAPABILITY_IAM + CAPABILITY_AUTO_EXPAND) with the SAM template above (AWS::Serverless::Function 'Fn' + FunctionUrlConfig, Output !GetAtt FnUrl.FunctionUrl). Observe 400 ValidationError 'Template error: instance of Fn::GetAtt references undefined resource FnUrl'. Replay cloudformation__lambda.deploy-sam-function-url-cors-config-round-trips-without-a-public-permission and cloudformation__lambda.sam-autopublishalias-function-url-targets-only-the-alias.
3. Cause 2: create a stack with a role, a function, a version and an alias; drive an UpdateStack that fails so the stack settles UPDATE_FAILED; then CreateChangeSet with the ORIGINAL (last stable) template and DescribeChangeSet. Observe CREATE_COMPLETE with four Modify rows instead of FAILED with no changes. Replay cloudformation__lambda.cfnuf-change-set-on-update-failed-stack-diffs-against-the-rolled-back-state.
4. Cause 3: delete the stack's function and Invoke it with no Qualifier; observe the message without ':$LATEST'. Replay cloudformation__lambda.cfn-lambda-function-invoke-runs-handler-code-and-reports-unhandled-errors.
