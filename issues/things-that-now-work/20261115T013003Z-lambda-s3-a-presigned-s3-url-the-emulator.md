Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3.nodejs-handler-https-get-fetches-a-presigned-url
Issue-Key: lambda__s3.nodejs-handler-https-get-fetches-a-presigned-url|run#9|AssertionError|assert "error" not in out, f"https.get failed in the function: {out!r}"
Issue-Commit: dda73b1c89ff90e3a0e7799df332912882e239a1

# lambda__s3: a presigned S3 URL the emulator itself issued is unreachable from inside a function — node https.get fails ENOTFOUND because the in-sandbox router does not recognise path-style S3 addressing

> _Note: this issue was identified against the code base at commit `dda73b1c89ff90e3a0e7799df332912882e239a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Resolved: the cause reported here — the in-function egress router attributing an outbound
> URL to a service by host alone, so a path-style presigned URL on the emulator's own
> endpoint reached nothing — is fixed. Such a URL is now attributed to the service its own
> presigned credential names and is sent as the URL states itself, and a presigned URL this
> emulator issues (the SigV4 query form its presigner mints) is served to a handler end to
> end. One leg of the recorded scenario still fails, on a cause outside this report and
> already filed on its own:
> `issues/things-that-do-not-work/20261027T190000Z-s3-legacy-query-credential-presigned-get.md`
> — no authenticator of this emulator recognises the LEGACY query credential form
> (`AWSAccessKeyId`/`Signature`/`Expires`), which the recorded run's client minted, so such a
> URL is refused `InvalidAccessKeyId` for every client, inside a function or outside it; that
> report records the core change it needs._

## Operation
GetObject

## Description
One net-new root cause.

Cause 1 of 1: the Lambda execution environment's outbound HTTP router refuses a presigned S3 URL that the emulator minted, because it attributes an outbound URL to a service by host alone and the URL is path-style (bucket as the first path segment of the emulator's own endpoint).

AWS: Smithy (s3) models GetObject as GET /{Key+} on the bucket endpoint; the presigned form carries the credential, signature and expiry in the query string, and nothing in the model distinguishes virtual-hosted addressing from path-style addressing — both are valid. Recorded behavior only for the in-function side: a function with outbound access fetches a presigned GET URL with an ordinary HTTP client (node's https.get / http.get) and receives HTTP 200 and the object's bytes.

Emulator: the request never leaves the sandbox. The in-function client fails with code ENOTFOUND and the emulator's own message '<the presigned URL> addresses no service this emulator serves: sqs, sns, events, eventbridge, pipes, dynamodb, logs, cloudwatchlogs, cloudwatch, apigateway, apigatewaymanagementapi, secretsmanager, kms, ssm, opensearch, es, stepfunctions, sfn, sesv2, s3, xray, rds-data, appconfigdata, appconfig, config, configservice, codedeploy, codepipeline, cloudfront, rekognition, lambda, glue, bedrock-agent-runtime, opensearchserverless'. The fixed text is that sentence and the service list; the variable part is the URL, which the emulator's own presigner produced in path-style form on the emulator's host. s3 is in the list the message prints, so the URL is refused for a service the emulator does serve.

Why: fetching a presigned URL from inside a function is the documented way to hand object access to code that must not hold S3 credentials; here it always fails. The emulator announces an address that its own in-function network layer will not route, and the failure arrives as a DNS-style ENOTFOUND, which a client reads as "that host does not exist" rather than "this request was refused".

Fix: the in-function egress/service router that maps an outbound URL onto a served service — it must attribute path-style S3 URLs on the emulator's own endpoint to s3 — and/or the S3 presigner, so the URL the emulator announces is one the sandbox can route.

Traces: lambda__s3.nodejs-handler-https-get-fetches-a-presigned-url.

## Evidence
Failing check lambda__s3.nodejs-handler-https-get-fetches-a-presigned-url [mismatch].

Expected (AWS): the handler's https.get/http.get of the presigned URL returns HTTP 200 and the object's bytes; no 'error' key in the function's response.

Actual (emulator): 'https.get failed in the function: {\'error\': \'Error\', \'code\': \'ENOTFOUND\', \'message\': \'https://127.0.0.1:<port>/<bucket>/fetched.bin?AWSAccessKeyId=...&Signature=...&Expires=... addresses no service this emulator serves: sqs, sns, events, eventbridge, pipes, dynamodb, logs, cloudwatchlogs, cloudwatch, apigateway, apigatewaymanagementapi, secretsmanager, kms, ssm, opensearch, es, stepfunctions, sfn, sesv2, s3, xray, rds-data, appconfigdata, appconfig, config, configservice, codedeploy, codepipeline, cloudfront, rekognition, lambda, glue, bedrock-agent-runtime, opensearchserverless\'}'.

## Steps to Reproduce
1. Create a bucket and put an object in it.
2. Generate a presigned GET URL for that object (the emulator returns a path-style URL on its own endpoint).
3. Create a nodejs22.x function whose handler fetches event.url with node's https.get/http.get and returns the status and body.
4. Invoke the function with that URL.
5. Observe: the handler reports code ENOTFOUND and the emulator's 'addresses no service this emulator serves' message instead of HTTP 200 and the object bytes.
