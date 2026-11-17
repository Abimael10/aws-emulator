Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.url-none-auth-grants-to-a-named-principal-do-not-admit-anonymous-callers
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.url-none-auth-grants-to-a-named-principal-do-not-admit-anonymous-callers|run#9|AssertionError|assert json.loads(denied["body"]) == _FORBIDDEN, (
Issue-Commit: 6533a5364fe9b92c8a3d23732cb80de0d1d9d8ab

# lambda: a Function URL authorizer refusal serves the body {"Message":"Forbidden"} instead of AWS's full Forbidden troubleshooting message

> _Note: this issue was identified against the code base at commit `6533a5364fe9b92c8a3d23732cb80de0d1d9d8ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateFunctionUrlConfig

## Description
Cause 1 of 1: a Lambda Function URL refusal carries a truncated refusal document. The status and the error-type header are right; the JSON body's Message value is missing the fixed troubleshooting sentence AWS appends.

AWS: Smithy: lambda CreateFunctionUrlConfig takes AuthType (AWS_IAM|NONE); the Function URL data plane's refusal document is not modeled, so recorded behavior only plus AWS's documented Function-URL authorization behaviour (https://docs.aws.amazon.com/lambda/latest/dg/urls-auth.html). A Function URL request the URL authorizer refuses is answered with HTTP 403, header x-amzn-errortype: AccessDeniedException, and the JSON body:
{"Message":"Forbidden. For troubleshooting Function URL authorization issues, see: https://docs.aws.amazon.com/lambda/latest/dg/urls-auth.html"}
The whole body is fixed text — there is no variable part taken from the request or from state — and the handler never runs, so the response can never contain handler output. The recording shows this body on three consecutive refusals of an anonymous GET to a URL configured AuthType=NONE whose only lambda:InvokeFunctionUrl grant names a specific principal.

Emulator: the refusal is correctly a 403 with x-amzn-errortype: AccessDeniedException and does not leak handler output, but the body is b'{"Message":"Forbidden"}' — the Message value stops after the word "Forbidden" and omits the fixed remainder ". For troubleshooting Function URL authorization issues, see: https://docs.aws.amazon.com/lambda/latest/dg/urls-auth.html".

Why: the Function URL refusal body is the only machine-readable description a plain HTTP client (no SDK, no error envelope) gets. A client that matches the refusal on its documented body — or that surfaces the body to an operator so the cause can be diagnosed — gets a string AWS never sends, so an equality match fails and the operator loses the pointer AWS puts in the message. The divergence is exactly the kind that passes against the emulator and fails against AWS.

Fix: the Function URL request path's authorization refusal serializer — the 403 it writes must carry AWS's complete fixed Forbidden document for both AuthType=NONE principal-scoped refusals and AWS_IAM refusals, not just the word "Forbidden". Note the related open issue 20261111T064629Z-cloudwatch-lambda-an-unsigned-aws-iam-lambda-function-url-request.md, which asked for the short {"Message":"Forbidden"} body and does not carry this check; the body AWS serves is the long form quoted above. Fixed issue 20261101T210635Z-lambda-weighted-alias-traffic-is-never-observable-across.md made this URL answer at all (it previously hung); the refusal it now answers with is the remaining divergence.

Traces: lambda.url-none-auth-grants-to-a-named-principal-do-not-admit-anonymous-callers.

## Evidence
Check lambda.url-none-auth-grants-to-a-named-principal-do-not-admit-anonymous-callers [mismatch]:

AssertionError: the refusal must be the URL authorizer's Forbidden body, got b'{"Message":"Forbidden"}'

Expected (three consecutive anonymous GETs to a Function URL with AuthType=NONE whose lambda:InvokeFunctionUrl grant names one principal): HTTP 403, header x-amzn-errortype: AccessDeniedException, body exactly
{"Message": "Forbidden. For troubleshooting Function URL authorization issues, see: https://docs.aws.amazon.com/lambda/latest/dg/urls-auth.html"}
Actual: HTTP 403 with x-amzn-errortype: AccessDeniedException (both correct) and body {"Message":"Forbidden"}.

The refusal classification itself is right — the check's _authorizer_refusal predicate (status 403 + x-amzn-errortype AccessDeniedException) matched three times in a row, and the handler marker sim2real-url-handler-ran was absent from the body.

## Steps to Reproduce
1. Create an execution role and a python3.12 function whose handler returns {"statusCode":200, "body": json with marker "sim2real-url-handler-ran"}.
2. CreateFunctionUrlConfig(FunctionName=<fn>, AuthType="NONE").
3. AddPermission(FunctionName=<fn>, StatementId="sim2real-url", Action="lambda:InvokeFunctionUrl", Principal=<the caller's own principal>, FunctionUrlAuthType="NONE") — i.e. a grant that names one principal rather than "*".
4. Issue a plain, unsigned HTTPS GET to the returned FunctionUrl.
5. Read the response status, the x-amzn-errortype header and the body. AWS answers 403 / AccessDeniedException with the full Forbidden troubleshooting document; the emulator answers 403 / AccessDeniedException with {"Message":"Forbidden"}.
