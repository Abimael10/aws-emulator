Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive
Issue-Key: lambda|Invoke|ResourceNotFoundException
Issue-Commit: 6ad539f79015bc27d2d0777d4c5c145ae7d45c50

# Lambda Invoke's ResourceNotFoundException message drops the resolved ':$LATEST' qualifier from the function ARN

> _Note: this issue was identified against the code base at commit `6ad539f79015bc27d2d0777d4c5c145ae7d45c50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: the 'Function not found' message of Invoke names the UNQUALIFIED function ARN instead of the fully qualified one.

AWS: Invoke against a function that does not exist answers HTTP 404 with error code ResourceNotFoundException (Smithy: Invoke -> ResourceNotFoundException, errorKind client, httpError 404; POST /2015-03-31/functions/{FunctionName}/invocations, model services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json). Its Message is the fixed text 'Function not found: ' followed by the FULLY QUALIFIED function ARN: the name or ARN from the request resolved to arn:<partition>:lambda:<region>:<account-id>:function:<name> plus the qualifier the request resolved to — ':$LATEST' when the request carried no Qualifier, otherwise ':<version>' or ':<alias>'. Fixed text: 'Function not found: '. Variable parts: partition, region, account id and function name taken from the request and the credential scope, and the resolved qualifier. The exact wording is recorded behavior only (the Smithy model carries no message text), and the suite's expected string is the recording.
Emulator: answers 404 ResourceNotFoundException but with 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' — the ':$LATEST' suffix is absent, so the message names a resource identity the service never resolves a bare Invoke to.
Why: a client that compares or parses the refusal (test harnesses, deploy tooling, CloudFormation custom resources, anything that re-derives the qualified ARN it invoked from the error) sees a different ARN than AWS reports, and loses the information about WHICH version was looked up. The code and status are right, so the divergence is silent until the string is read.
Fix: the Invoke handler's not-found path in the lambda crate (and the shared helper that formats the 'Function not found: ' message) must resolve the qualifier before formatting the ARN and append it.
Traces: apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive.

## Evidence
Behavioral suite replay against the emulator built from this checkout.

[mismatch] apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive:
  File check_apigateway__dynamodb__lambda__logs_apigw_http_api_lambda_integration_positive.py, line 219, in _refused
  assert msg == message
  AssertionError: Invoke sim2real-fn-e24a4c984cd0: ResourceNotFoundException message 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-fn-e24a4c984cd0', expected 'Function not found: arn:aws:lambda:us-west-2:000000000000:function:sim2real-fn-e24a4c984cd0:$LATEST'

Expected: HTTP 404, Error.Code 'ResourceNotFoundException', Error.Message 'Function not found: <qualified function ARN>:$LATEST'. Actual: the same status and code with the unqualified ARN. (The ARN's region/account/name values above are this run's; the requirement is the fixed prefix plus the request-resolved qualifier.)

## Steps to Reproduce
1. Clone the repo and build/boot the emulator from this branch.
2. Create a Lambda function, then DeleteFunction it (or pick any name that does not exist).
3. Call lambda:Invoke with FunctionName=<that name> and no Qualifier.
4. Observe Error.Message 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>' instead of AWS's 'Function not found: arn:aws:lambda:<region>:<account>:function:<name>:$LATEST'.
5. Replay check apigateway__dynamodb__lambda__logs.apigw-http-api-lambda-integration-positive to confirm.
