Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__ec2__iam__lambda__s3__sns__sqs__sts.one-denied-principal-error-shape-per-protocol-family
Issue-Key: dynamodb__ec2__iam__lambda__s3__sns__sqs__sts.one-denied-principal-error-shape-per-protocol-family|run#23|AssertionError|assert ctype == "application/x-amz-json-1.0", f"dynamodb Content-Type was {ctype!r}"
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# dynamodb__ec2__iam__lambda__s3__sns__sqs__sts: DynamoDB's AccessDeniedException reply carries no Content-Type header at all, so the awsJson1_0 error response is unlabelled

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> **Resolution: cause 1 of 1 DECLINED — the reported defect does not exist, and the one byte
> that still differs is blocked outside every path a fix may change.** The refusal DOES
> announce the protocol media type: the reply to this very call carries
> `content-type: application/x-amz-json-1.0` beside its `__type`, its `Message` and no
> `x-amzn-query-error`, confirmed on the wire three ways — the assembled router with request
> authentication on, a raw HTTP/1.1 socket against a served endpoint, and the built emulator
> binary on its own HTTPS data port. The only difference left from the recorded AWS reply is
> the LETTER CASE of the field name (`Content-Type` against `content-type`), which HTTP field
> names make insensitive and which the vendored HTTP/1.1 writer folds on EVERY response of
> EVERY service, successes included: it is decided in `vendor/server/src/lib.rs`
> (`serve_production_on_address`, which hands hyper neither `http1_preserve_header_case` nor
> an `OriginalHeaderCaseMap`), a read-only tree, and `aws_smithy_runtime_api::http::Headers`
> is an `http::HeaderMap`, whose `HeaderName` is lower-cased on construction, so no service
> crate can spell a field name otherwise. The rule the report is about is pinned over a real
> connection by
> `services/service-com-amazonaws-dynamodb-dynamodb-20120810/tests/all/refusal_reply_media_type.rs`.

## Operation
ListTables

## Description
Cause 1 of 1: the authorization-refusal path of the DynamoDB wire layer emits an error response with no Content-Type header.

AWS:      Smithy (service DynamoDB_20120810, 2012-08-10): the service's only protocol is awsJson1_0, sigv4Name `dynamodb`. Every awsJson1_0 reply — success AND error — is labelled `Content-Type: application/x-amz-json-1.0`, and the error payload carries its type in a body `__type` field (DynamoDB is plain awsJson1_0, with no awsQueryCompatible shim, so it must not carry an `x-amzn-query-error` header either). ListTables models no AccessDeniedException of its own, so the implicit-deny refusal is the service-common client error: HTTP 400 with code `AccessDeniedException` and the message `User: <caller-arn> is not authorized to perform: dynamodb:ListTables on resource: arn:aws:dynamodb:<region>:<account>:table/* because no identity-based policy allows the dynamodb:ListTables action` — the fixed text is `User: `, ` is not authorized to perform: `, ` on resource: `, ` because no identity-based policy allows the ` and ` action`, with the caller ARN, action and resource ARN taken from the request's signing identity, the operation and the account/region. The emulator already matched the code, the message and the 400 status on this call; only the header is missing.

Emulator: the 400 AccessDeniedException reply to ListTables arrives with NO Content-Type header — botocore's captured response headers give `Content-Type: None`. Successful DynamoDB replies in the same run do carry `content-type: application/x-amz-json-1.0`, so the omission is specific to the refusal path, not to the serializer in general. Hypothesis: the authorization decision short-circuits and writes the error body without going through the awsJson1_0 response serializer that sets the protocol content type.

Why:      an unlabelled error body is not a well-formed awsJson1_0 response. A client, proxy or SDK that dispatches on Content-Type (strict HTTP clients, logging/observability middleware, content negotiation in gateways, non-botocore SDKs and hand-rolled signers) cannot tell what the error body is and may treat the refusal as an unparseable/transport failure instead of an AccessDeniedException — turning a deterministic authorization decision into a retry or a crash. It also hides the protocol-family distinction the surrounding scenario asserts (DynamoDB plain awsJson1_0 vs SQS's awsQueryCompatible shim).

Fix:      dynamodb's HTTP error-response serializer / the authorization-rejection path that builds the AccessDeniedException reply — it must set `Content-Type: application/x-amz-json-1.0` on refusals exactly as it does on success, keep the `__type` field in the body, and still emit no `x-amzn-query-error` header.

Traces:   dynamodb__ec2__iam__lambda__s3__sns__sqs__sts.one-denied-principal-error-shape-per-protocol-family.

## Evidence
Setup: an IAM role with zero attached and zero inline policies is assumed via STS; GetCallerIdentity as that session succeeds (positive control), then dynamodb ListTables(Limit=1) is called as the permissionless principal.

Already matching (asserted before the failure):
  Error.Code    == "AccessDeniedException"
  Error.Message == "User: arn:aws:sts::<account>:assumed-role/<role>/probe is not authorized to perform: dynamodb:ListTables on resource: arn:aws:dynamodb:<region>:<account>:table/* because no identity-based policy allows the dynamodb:ListTables action"
  HTTP status   == 400

Expected: response header Content-Type == "application/x-amz-json-1.0"
Actual:
  File "check_dynamodb__ec2__iam__lambda__s3__sns__sqs__sts_one_denied_principal_error_shape_per_protocol_family.py", line 128, in run
    assert ctype == "application/x-amz-json-1.0", f"dynamodb Content-Type was {ctype!r}"
  AssertionError: dynamodb Content-Type was None

The diverging requirement is the absent Content-Type response header on the refusal. The role name, account, region and session name in the message are values taken from the request and state, not part of the message template.

## Steps to Reproduce
1. iam CreateRole with a trust policy allowing arn:aws:iam::<account>:root to sts:AssumeRole. Attach NO managed and NO inline policy.
2. sts AssumeRole into it (RoleSessionName "probe") and build a client from the returned credentials.
3. Positive control: sts GetCallerIdentity with those credentials succeeds and returns arn:aws:sts::<account>:assumed-role/<role>/probe.
4. Call dynamodb ListTables(Limit=1) with those credentials and capture the raw HTTP response headers (botocore `before-parse` hook).
5. Observe HTTP 400 with code AccessDeniedException and the correct message, but NO Content-Type header on the response (expected application/x-amz-json-1.0). Compare with any successful DynamoDB call in the same session, which does carry it.
