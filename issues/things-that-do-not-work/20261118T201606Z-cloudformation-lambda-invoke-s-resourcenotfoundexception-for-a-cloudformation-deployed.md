Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.deploy-lambda-function-invokable
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.deploy-lambda-function-invokable|run#23|AssertionError|assert invoke_got == invoke_expected, (
Issue-Commit: 7e4414010feb5d6e6de4259f326eb0449b52de38

# Lambda Invoke's ResourceNotFoundException for a CloudFormation-deployed function removed by a stack delete drops the resolved ':$LATEST' qualifier from the function ARN

> _Note: this issue was identified against the code base at commit `7e4414010feb5d6e6de4259f326eb0449b52de38`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One net-new root cause in the CloudFormation deploy-then-invoke path.

Cause 1 of 1: Invoke's not-found refusal names the function ARN without the resolved version qualifier.
Failing check: cloudformation.deploy-lambda-function-invokable

AWS: Invoke on a function name that does not exist is refused with ResourceNotFoundException. Smithy: Invoke declares error ResourceNotFoundException (errorKind client, httpError 404); the Invoke input member FunctionName targets NamespacedFunctionName, whose pattern explicitly admits a trailing ':(\$LATEST(\.PUBLISHED)?|[a-zA-Z0-9-_]+)' qualifier, and when no Qualifier is supplied AWS resolves the request to $LATEST before reporting it. The message is 'Function not found: <resolved function ARN>', where 'Function not found: ' is the fixed text and the variable part is the ARN AWS resolved the request to -- arn:<partition>:lambda:<region>:<account>:function:<function name>:$LATEST. The ':$LATEST' suffix is part of the resolved ARN AWS reports whenever the caller passed no Qualifier, not a per-run value: the partition, region, account, function name and request id vary per environment, the ':$LATEST' segment does not.

Emulator: after the stack that declared the AWS::Lambda::Function is deleted, Invoke on the function name is correctly refused 404 ResourceNotFoundException, but the message stops at the unqualified function ARN -- 'Function not found: arn:aws:lambda:<region>:<account>:function:<function name>' -- with no ':$LATEST'. Error code and HTTP status already match; only the ARN in the message is truncated.

Why: a client that parses the message to learn which version of the function AWS looked for -- the documented way to tell an unqualified miss from a miss on a specific published version or alias -- cannot distinguish them, and any test or tool that asserts the AWS message text for a deleted-function invoke fails against the emulator.

Fix: the Lambda Invoke handler's not-found refusal path, specifically the function-ARN formatting used to build the ResourceNotFoundException message: it must serialize the ARN the request resolved to (including the ':$LATEST' qualifier when the caller supplied no Qualifier), not the unqualified function ARN. This is the same root cause already reported for other surfaces by the open issues 20261118T165433Z-apigateway-dynamodb-lambda-logs-lambda-invoke-s-resourcenotfoundexception-message-drops-the-resolved.md, 20261118T180135Z-cloudformation-iam-lambda-s3-lambda-invoke-s-resourcenotfoundexception-after-a-stack-delete.md and cause 1 of 20261118T165433Z-lambda-9-causes-invoke-s-not-found-message.md; none of those carries this check's replay key, so this check is not covered by them.

Traces: cloudformation.deploy-lambda-function-invokable (mismatch) -- the assertion fires at check_cloudformation_deploy_lambda_function_invokable.py line 87, after the stack delete, when the check invokes the now-deleted function.

## Evidence
cloudformation.deploy-lambda-function-invokable (mismatch):
  AssertionError: invoking the deleted function: expected ('ResourceNotFoundException', 'Function not found: arn:aws:lambda:<region>:<account>:function:<stack>-AddFn-<suffix>:$LATEST'), got ('ResourceNotFoundException', 'Function not found: arn:aws:lambda:<region>:<account>:function:<stack>-AddFn-<suffix>')

Error code (ResourceNotFoundException) and status (404) match. The only divergence is the message's ARN: AWS's fixed text is 'Function not found: ' and its variable part is the resolved ARN, which ends in ':$LATEST' for an Invoke carrying no Qualifier; the emulator emits the unqualified ARN. The region, account id, stack name and CloudFormation-generated logical-id suffix in the recorded ARN are per-environment values and are not part of the requirement.

## Steps to Reproduce
1. CreateStack with a template declaring an AWS::IAM::Role (lambda.amazonaws.com trust, AWSLambdaBasicExecutionRole) and an AWS::Lambda::Function (Runtime python3.12, Handler index.handler, inline Code.ZipFile) with an Output that Refs the function.
2. Wait for CREATE_COMPLETE and read the function name from the stack Output. Invoke it to confirm it answers (positive control).
3. DeleteStack and wait for DELETE_COMPLETE.
4. Call lambda Invoke with FunctionName set to that same function name and no Qualifier.
5. Observe the ClientError: the code is ResourceNotFoundException (correct) but the message names the unqualified function ARN. AWS's message names the resolved ARN with the ':$LATEST' qualifier appended.
