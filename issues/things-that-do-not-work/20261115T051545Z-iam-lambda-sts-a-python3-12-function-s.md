Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__sts.function-runs-as-an-assumed-role-session-named-for-the-function
Issue-Key: iam__lambda__sts.function-runs-as-an-assumed-role-session-named-for-the-function|run#9|AssertionError|assert out["keyPrefix"] == "ASIA", (
Issue-Commit: 5bda3f4959ddaaecd7545612991d55d37567ef10

# iam__lambda__sts: a python3.12 function's environment carries no AWS_ACCESS_KEY_ID, so the execution role's credentials are never injected into the sandbox

> _Note: this issue was identified against the code base at commit `5bda3f4959ddaaecd7545612991d55d37567ef10`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1 — the invoked function's environment has no execution-role credentials at all (failing check: iam__lambda__sts.function-runs-as-an-assumed-role-session-named-for-the-function).
AWS: recorded behavior only for the variable values, plus AWS's documented Lambda runtime environment (https://docs.aws.amazon.com/lambda/latest/dg/configuration-envvars.html): every managed-runtime invocation runs as an assumed-role session of the function's execution role, and the sandbox environment always carries AWS_ACCESS_KEY_ID (an STS temporary key id, so beginning with the four characters ASIA), AWS_SECRET_ACCESS_KEY and AWS_SESSION_TOKEN, which the bundled SDK picks up as its default credentials, together with AWS_REGION and AWS_LAMBDA_FUNCTION_NAME. The identity those keys resolve to is arn:<partition>:sts::<account>:assumed-role/<execution role name>/<function name> with UserId <RoleId>:<function name>.
Emulator: for a python3.12 function created with a plain lambda.amazonaws.com-trusting execution role, os.environ.get("AWS_ACCESS_KEY_ID", "")[:4] inside the handler is the empty string — the variable is absent (or empty), so no credential of any class is injected. The handler therefore has no execution-role identity to sign with; the rest of the contract (session token presence, the SDK picking up the environment credentials) cannot hold either.
Why: a function that calls any AWS API through the bundled SDK has no credentials to sign with, so real handler code either falls back to some other credential source or fails outright; and nothing the function does can be authorized as its execution role, which erases per-function authorization. Note this is NOT the nodejs22 case already filed as 20261109T224859Z-iam-lambda-sts-a-nodejs22-x-function-s.md (the suite reports that issue as owning only the two nodejs22 checks); here the python3.12 sandbox gets no key at all rather than a wrongly classed one.
Fix: the Lambda Invoke path's sandbox environment construction — the component that assumes the execution role per invocation and injects AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_SESSION_TOKEN (ASIA-classed, from the STS session for assumed-role/<role>/<function>) into the python runtime's environment.
Traces: iam__lambda__sts.function-runs-as-an-assumed-role-session-named-for-the-function.

## Evidence
check iam__lambda__sts.function-runs-as-an-assumed-role-session-named-for-the-function, line 67 — AssertionError: "AWS_ACCESS_KEY_ID starts '', not the temporary-credential prefix ASIA". The handler returns env.get("AWS_ACCESS_KEY_ID", "")[:4]; expected 'ASIA' (a 4-char STS temporary key class), actual '' (variable absent/empty). Runtime python3.12, Handler index.handler, execution role trusting lambda.amazonaws.com with no inline statements.

## Steps to Reproduce
Create an IAM role trusting lambda.amazonaws.com; create a python3.12 function (Handler index.handler) with that role; Invoke it RequestResponse with a handler that returns os.environ.get("AWS_ACCESS_KEY_ID", "")[:4], bool(os.environ.get("AWS_SECRET_ACCESS_KEY")) and bool(os.environ.get("AWS_SESSION_TOKEN")). The emulator returns an empty key prefix; AWS returns an ASIA-prefixed key id with secret and session token present and used by the bundled SDK.
