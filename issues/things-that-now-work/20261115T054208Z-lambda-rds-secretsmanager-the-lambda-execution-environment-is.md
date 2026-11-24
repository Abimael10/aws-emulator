Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__rds__secretsmanager.lambda-execution-role-assumed-for-rds-proxy-auth-positive
Issue-Key: lambda__rds__secretsmanager.lambda-execution-role-assumed-for-rds-proxy-auth-positive|run#26|AssertionError|assert result["has_session_token"] is True, (
Issue-Commit: af1425a55293113a016d3616ca9afb5fb5b89a60

# lambda__rds__secretsmanager: the Lambda execution environment is handed static credentials with no session token, so an in-function RDS IAM auth token is not signed by the function's assumed-role session

> _Note: this issue was identified against the code base at commit `af1425a55293113a016d3616ca9afb5fb5b89a60`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: the credentials the emulator exposes inside a Lambda invocation are not temporary assumed-role credentials — they carry no session token — even though GetCallerIdentity from inside the same invocation reports an assumed-role ARN for the function's execution role. Failing check id: lambda__rds__secretsmanager.lambda-execution-role-assumed-for-rds-proxy-auth-positive.

AWS: when AWS Lambda runs a function it assumes the function's execution role (the role whose trust policy allows sts:AssumeRole for the lambda.amazonaws.com service principal) and injects that session's TEMPORARY credentials into the execution environment as AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY and AWS_SESSION_TOKEN. Smithy (STS model, shape com.amazonaws.sts#Credentials, as returned by AssumeRole): members AccessKeyId, SecretAccessKey, SessionToken and Expiration are ALL @required — a role session by definition has a session token, and SessionToken is documented as "The token that users must pass to the service API to use the temporary credentials". Consequently, inside the function, boto3's default credential chain resolves to that role session: session.get_credentials().get_frozen_credentials().token is non-empty, the access key id begins with the temporary-credential prefix rather than a long-lived user key, sts:GetCallerIdentity returns an arn of the form arn:aws:sts::<account>:assumed-role/<execution-role-name>/<function-name>, and anything the function signs locally (rds.generate_db_auth_token, which presigns a SigV4 URL without a network call) is signed by that same session — its X-Amz-Credential scope carries the SAME access key id that the environment exposed. For the model's own view of the data-plane call that drives this, Smithy: Lambda Invoke returns StatusCode 200 with no FunctionError for a successful invocation, which is what the emulator returned here; the divergence is purely in the identity the invocation runs as.

Emulator: the invocation succeeds (StatusCode 200, no FunctionError) and the identity ARN it reports does contain ":assumed-role/<role-name>/", but the credentials actually materialised in the execution environment have an EMPTY session token: bool(creds.token) is False for the frozen credentials of the in-function default session. The check fails at the assertion on that value with 'AssertionError: the RDS auth token was minted from credentials with no session token behind them - these are static/long-lived credentials, not the Lambda's assumed-role session'. So the ARN is synthesised to look like a role session while the signing material behind it is a static, long-lived key — the assumed-role session is never really established, only reported. (Note the assertions that follow it and were never reached: the presigned RDS auth token must start with "<proxy-host>:<port>/?Action=connect" and must embed "X-Amz-Credential=<the access key the environment exposed>%2F"; those remain unverified until the credentials are fixed.)

Why: a client relying on AWS's behavior gets a function that cannot act as its own execution role. Every SDK call made from inside the function is signed by whatever static identity the emulator handed it, so per-function least privilege is unenforceable: two functions with different execution roles are indistinguishable to any authorization check, and a negative test that expects AccessDenied for a role lacking a permission can pass or fail for the wrong identity. Anything that requires a session token is broken outright — presigned SigV4 artifacts minted in-function (RDS Proxy / RDS IAM database auth tokens via GenerateDBAuthToken, S3 and other presigned URLs) omit X-Amz-Security-Token and are rejected by the consuming service, services that evaluate the aws:PrincipalArn / aws:userid of a role session see the wrong principal, and code that reads AWS_SESSION_TOKEN (or forwards it to a sidecar or subprocess) finds it unset.

Fix: the Lambda invoke path's credential provisioning for the execution environment — the component that assumes the function's execution role before handing control to the runtime and populates the runtime's credential environment. It must mint a real role session for the configured Role (an STS AssumeRole whose output carries AccessKeyId, SecretAccessKey, SessionToken and Expiration, all required by the STS model) and expose all three credential values, including the session token, to the running function, rather than exposing a static account key while separately synthesising an assumed-role ARN for GetCallerIdentity. The ARN GetCallerIdentity reports and the key the environment exposes must come from the same session.

Traces: lambda__rds__secretsmanager.lambda-execution-role-assumed-for-rds-proxy-auth-positive (verdict: mismatch).

## Evidence
Check: lambda__rds__secretsmanager.lambda-execution-role-assumed-for-rds-proxy-auth-positive [mismatch]

Scenario: create an IAM role trusted by lambda.amazonaws.com (AWSLambdaBasicExecutionRole attached plus an inline rds-db:connect allow), create a python3.12 function with that role, wait for State == Active, then Invoke it. The handler does:
  creds = boto3.Session().get_credentials().get_frozen_credentials()
  identity = sts.get_caller_identity()
  token = rds.generate_db_auth_token(DBHostname=<proxy host>, Port=3306, DBUsername="app_user")
and returns arn, access_key_id, has_session_token = bool(creds.token), token.

Expected (AWS): StatusCode 200, no FunctionError; arn contains ":assumed-role/<role-name>/"; has_session_token is True (STS Credentials.SessionToken is @required for a role session); len(token) > 100; token starts with "<proxy-host>:3306/?Action=connect"; token contains "X-Amz-Credential=<access_key_id>%2F" for the SAME access_key_id the environment exposed.

Actual (emulator): StatusCode 200 and no FunctionError; arn does contain ":assumed-role/<role-name>/"; but has_session_token is False. Replay failure:
  File "check_lambda__rds__secretsmanager_lambda_execution_role_assumed_for_rds_proxy_auth_positive.py", line 132, in run
    assert result["has_session_token"] is True, (
  AssertionError: the RDS auth token was minted from credentials with no session token behind them - these are static/long-lived credentials, not the Lambda's assumed-role session

The token-prefix and X-Amz-Credential-linkage assertions after it were never evaluated, so they are unverified rather than passing. The requirement here is structural (a session token exists and the signing key matches the exposed key); the specific role name, function name, proxy hostname, access key id and token bytes are per-run values and are not part of it.

## Steps to Reproduce
1. iam.create_role with AssumeRolePolicyDocument allowing sts:AssumeRole for Principal {"Service": "lambda.amazonaws.com"}; attach arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole and put an inline policy allowing rds-db:connect on "*".
2. lambda.create_function(Runtime="python3.12", Role=<that role arn>, Handler="index.handler", Code={"ZipFile": <zip containing index.py>}, Timeout=15, MemorySize=128) where index.py is:
     import boto3
     def handler(event, context):
         session = boto3.Session()
         creds = session.get_credentials().get_frozen_credentials()
         identity = session.client("sts").get_caller_identity()
         token = session.client("rds").generate_db_auth_token(
             DBHostname=event["host"], Port=event.get("port", 3306), DBUsername=event["user"])
         return {"arn": identity["Arn"], "access_key_id": creds.access_key,
                 "has_session_token": bool(creds.token), "token": token}
3. Poll lambda.get_function_configuration until State == "Active".
4. lambda.invoke(FunctionName=<fn>, Payload=json.dumps({"host": "<any>.proxy-abcdef.<region>.rds.amazonaws.com", "port": 3306, "user": "app_user"})).
5. Parse the response payload. Observe has_session_token is False (and AWS_SESSION_TOKEN unset in the execution environment) while arn reports ":assumed-role/<role-name>/". On AWS has_session_token is True, and the token's X-Amz-Credential scope carries the same access_key_id the environment exposed.
