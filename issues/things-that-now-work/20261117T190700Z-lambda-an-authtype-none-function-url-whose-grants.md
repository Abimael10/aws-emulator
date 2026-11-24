Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.url-none-auth-grants-to-a-named-principal-do-not-admit-anonymous-callers
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.url-none-auth-grants-to-a-named-principal-do-not-admit-anonymous-callers|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Commit: 85ee122b95e0a025d1600c3d9babe834193f0ff3

# lambda: an AuthType=NONE function URL whose grants name one principal never produces the anonymous-GET refusal — the check exhausts its budget with no verdict

> _Note: this issue was identified against the code base at commit `85ee122b95e0a025d1600c3d9babe834193f0ff3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InvokeFunctionUrl

## Description
Cause 1 of 1: the function-URL data plane never settles the AuthType=NONE authorization contract. A function URL is created with AuthType=NONE and the only resource-policy statements on the function are two grants that name this caller's own IAM principal (never "*"): lambda:InvokeFunctionUrl with FunctionUrlAuthType=NONE and lambda:InvokeFunction with InvokedViaFunctionUrl=true. An anonymous (unsigned) GET of that URL is then polled for the refusal, and after switching the config to AWS_IAM the same principal's SigV4-signed GET is polled for a 200. Neither leg ever reaches a graded assertion.

AWS: Smithy (lambda) models CreateFunctionUrlConfig.AuthType and UpdateFunctionUrlConfig.AuthType as the enum FunctionUrlAuthType with exactly the values NONE and AWS_IAM, and AddPermission carries FunctionUrlAuthType plus InvokedViaFunctionUrl, so the URL authorizer is decided by the function's resource policy and not by AuthType alone. AuthType=NONE means Lambda does not require SigV4 on the request; it does NOT mean every caller is admitted — the resource policy must still allow lambda:InvokeFunctionUrl for the requesting principal. With no statement naming "*", an anonymous caller is not allowed, and AWS refuses the request at the URL authorizer, before the handler runs, with HTTP 403, response header x-amzn-ErrorType: AccessDeniedException and the JSON body {"Message":"Forbidden. For troubleshooting Function URL authorization issues, see: https://docs.aws.amazon.com/lambda/latest/dg/urls-auth.html"}. The fixed text of that message is the whole string; it has no variable parts. After UpdateFunctionUrlConfig(AuthType=AWS_IAM) with the same principal re-granted lambda:InvokeFunctionUrl for FunctionUrlAuthType=AWS_IAM, that principal's correctly signed GET must be served 200 with the handler's own body, and an unsigned GET must stay refused 403/AccessDeniedException. Reference: https://docs.aws.amazon.com/lambda/latest/dg/urls-auth.html

Emulator: the replay reports only `Error: Check timed out after 60.0s`. Inside the 60 s budget the emulator never produced three consecutive 403/AccessDeniedException answers for the anonymous GET, and it never produced the 200 for the signed GET either — no status, header or body is observable at all for this scenario. Hypothesis: AuthType=NONE is treated as "admit everyone" and the resource policy is not consulted, so the anonymous GET is answered with the handler's 200 instead of the authorizer's 403, and the poll for the refusal can never be satisfied; a URL endpoint that does not answer at all would produce the same timeout.

Why: a client cannot distinguish "authorization is off" from "authorization is enforced" on a NONE function URL. Code that relies on AuthType=NONE plus a narrowly scoped resource policy to keep a URL private either silently admits anonymous callers (a leak of the handler and its response) or hangs; either way, the documented Forbidden body and the AccessDeniedException error type that clients match on never arrive, and a caller that waits for the refusal waits forever.

Fix: the function-URL data-plane authorizer and the function resource-policy evaluation it uses — the URL front door must evaluate lambda:InvokeFunctionUrl (with the statement's FunctionUrlAuthType) and lambda:InvokeFunction (InvokedViaFunctionUrl) against the requesting principal for AuthType=NONE as well as AWS_IAM, must refuse a principal the policy does not allow before dispatching to the handler, and must emit the 403 / x-amzn-ErrorType: AccessDeniedException / Forbidden-troubleshooting body for that refusal. Related open issue 20261115T213820Z-lambda-reopens-the-function-url-data-plane-an-aws.md covers the AWS_IAM signed-request refusal and does not cover this NONE-auth resource-policy path; the earlier fixed issue 20261114T170134Z-lambda-a-function-url-authorizer-refusal-serves-the.md addressed only the wording of that refusal body, not whether the refusal is produced for AuthType=NONE.

Traces: lambda.url-none-auth-grants-to-a-named-principal-do-not-admit-anonymous-callers.

## Evidence
Expected (AWS): an unsigned GET of an AuthType=NONE function URL whose resource policy grants lambda:InvokeFunctionUrl / lambda:InvokeFunction only to a named principal is refused by the URL authorizer with HTTP 403, header `x-amzn-ErrorType: AccessDeniedException` and body {"Message":"Forbidden. For troubleshooting Function URL authorization issues, see: https://docs.aws.amazon.com/lambda/latest/dg/urls-auth.html"}, and the handler is never invoked (its marker is absent from the body). After UpdateFunctionUrlConfig(AuthType=AWS_IAM) with the same principal granted lambda:InvokeFunctionUrl for AWS_IAM, that principal's SigV4-signed GET is served 200 with the handler's body, while an unsigned GET stays 403/AccessDeniedException.
Actual (emulator): `Error: Check timed out after 60.0s` — within the budget the emulator produced neither the 403/AccessDeniedException refusal for the anonymous GET nor the 200 for the signed GET, so no status, error type or body for this URL is observable.

## Steps to Reproduce
1. CreateFunction (python3.12) with a handler that returns {"statusCode": 200, "body": <a recognisable marker>} and wait for State=Active.
2. CreateFunctionUrlConfig(FunctionName=fn, AuthType="NONE").
3. AddPermission(StatementId="s-url", Action="lambda:InvokeFunctionUrl", Principal=<this caller's own role ARN>, FunctionUrlAuthType="NONE") and AddPermission(StatementId="s-invoke", Action="lambda:InvokeFunction", Principal=<the same role ARN>, InvokedViaFunctionUrl=True). Add no statement naming "*".
4. Issue an UNSIGNED GET to the published FunctionUrl.
Observed: no 403 with x-amzn-ErrorType: AccessDeniedException and the Forbidden troubleshooting body is ever returned; the scenario exhausts its 60 s budget with no gradable response.
5. UpdateFunctionUrlConfig(AuthType="AWS_IAM"), RemovePermission("s-url"), re-AddPermission the same principal with FunctionUrlAuthType="AWS_IAM", then issue a SigV4-signed GET.
Observed: the signed GET is not served 200 with the handler's body either.
