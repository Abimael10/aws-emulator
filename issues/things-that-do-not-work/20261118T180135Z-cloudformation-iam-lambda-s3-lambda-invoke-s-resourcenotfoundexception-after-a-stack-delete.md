Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__iam__lambda__s3.cfn-delete-stack-removes-iam-role-and-lambda-positive
Issue-Key: cloudformation__iam__lambda__s3.cfn-delete-stack-removes-iam-role-and-lambda-positive|run#26|AssertionError|assert invoke_missing_message == invoke_not_found, (
Issue-Commit: 2586875af58cfe37da2b21b068b5f114b257c74a

# Lambda Invoke's ResourceNotFoundException after a stack delete omits the resolved ':$LATEST' qualifier from the function ARN

> _Note: this issue was identified against the code base at commit `2586875af58cfe37da2b21b068b5f114b257c74a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: lambda:Invoke against a function that no longer exists (here, one torn down by DeleteStack) refuses with the correct status and code but formats the 'Function not found' message with the UNQUALIFIED function ARN, dropping the qualifier the bare Invoke resolves to.

AWS: Invoke is modelled POST /2015-03-31/functions/{FunctionName}/invocations and declares ResourceNotFoundException, errorKind client, httpError 404 (Smithy: Invoke -> ResourceNotFoundException httpError 404; model services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json). The message text itself is recorded behavior only (the model carries no message literal). The recording shows the message is the fixed text 'Function not found: ' followed by the FULLY QUALIFIED function ARN: arn:<partition>:lambda:<region>:<account-id>:function:<name> plus the qualifier the request resolved to — ':$LATEST' when the request carried no Qualifier, otherwise ':<version>' or ':<alias>'. Fixed text: 'Function not found: '. Variable parts: partition, region, account id and function name taken from the request (or re-derived from FunctionName plus the credential scope), and the resolved qualifier. Note that the sibling GetFunction refusal in the same recording uses the UNqualified ARN — that assertion passed in this check — so the qualifier is specific to Invoke's resolved target, not a global ARN-formatting rule.

Emulator: answers 404 ResourceNotFoundException (code and status correct) with 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' — the ':$LATEST' suffix is absent. Replay evidence: the check asserted the Invoke message equals the GetFunction message plus ':$LATEST' and got the bare ARN instead.

Why: a client that reads or parses the refusal — a test harness, deploy tooling, a CloudFormation custom resource, or anything that re-derives which version it invoked from the error — sees a resource identity AWS never reports for a bare Invoke, and loses the information about which version was looked up. Because the status and error code are right, the divergence is silent until the message string is compared.

Fix: the Invoke handler's not-found path in the lambda crate (and the shared helper that formats the 'Function not found: ' message) must resolve the qualifier for the invocation target before formatting the ARN and append it, leaving GetFunction's unqualified wording unchanged.

Related already-open issues that do NOT cover this check: 20261118T165433Z-apigateway-dynamodb-lambda-logs-lambda-invoke-s-resourcenotfoundexception-message-drops-the-resolved.md and 20261118T165433Z-lambda-9-causes-invoke-s-not-found-message.md describe the same wording defect but carry different replay keys (apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive and the lambda-service checks); this check was not marked owned by either, so it is filed here with its own replay key.

Traces: cloudformation__iam__lambda__s3.cfn-delete-stack-removes-iam-role-and-lambda-positive.

## Evidence
Behavioral suite replay against the emulator built from this checkout.

[mismatch] cloudformation__iam__lambda__s3.cfn-delete-stack-removes-iam-role-and-lambda-positive:
  File check_cloudformation__iam__lambda__s3_cfn_delete_stack_removes_iam_role_and_lambda_positive.py, line 124, in run
  assert invoke_missing_message == invoke_not_found
  AssertionError: Invoke message 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-xsvc-fn-d0ce79809490' != 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-xsvc-fn-d0ce79809490:$LATEST'

Expected: HTTP 404, Error.Code 'ResourceNotFoundException', Error.Message = 'Function not found: ' + the qualified function ARN, i.e. arn:<partition>:lambda:<region>:<account>:function:<name>:$LATEST for an Invoke with no Qualifier.
Actual: the same status and the same error code, but Error.Message = 'Function not found: ' + the UNQUALIFIED ARN (no ':$LATEST').
The region, account id and function name shown above are this run's values filling the template; the requirement is the fixed prefix 'Function not found: ' plus the request-resolved qualified ARN. The same check's GetFunction assertion (unqualified ARN) and the IAM NoSuchEntity assertions passed, so only Invoke's qualifier is diverging.

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this branch.
2. CreateStack with CAPABILITY_IAM for a template containing an AWS::IAM::Role (lambda.amazonaws.com trust) and an AWS::Lambda::Function (python3.12, inline ZipFile code) whose Role is Fn::GetAtt of that role; wait for CREATE_COMPLETE.
3. DeleteStack and wait for DELETE_COMPLETE, so both the function and the role are gone.
4. Call lambda:GetFunction on the deleted function name — observe 404 ResourceNotFoundException with 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' (correct).
5. Call lambda:Invoke with FunctionName=<that name>, Payload=b'{}' and NO Qualifier.
6. Observe Error.Message 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' where AWS reports 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>:$LATEST'.
7. Replay check cloudformation__iam__lambda__s3.cfn-delete-stack-removes-iam-role-and-lambda-positive to confirm.
