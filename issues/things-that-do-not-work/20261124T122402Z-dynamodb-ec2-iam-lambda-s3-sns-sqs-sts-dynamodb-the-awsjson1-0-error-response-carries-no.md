Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__ec2__iam__lambda__s3__sns__sqs__sts.one-denied-principal-error-shape-per-protocol-family
Issue-Key: dynamodb__ec2__iam__lambda__s3__sns__sqs__sts.one-denied-principal-error-shape-per-protocol-family|run#23|AssertionError|assert ctype == "application/x-amz-json-1.0", f"dynamodb Content-Type was {ctype!r}"
Issue-Commit: 2ab5ac8cad8a83a47384c8ee23b8dea34a7c8513

# dynamodb: the awsJson1_0 error response carries no Content-Type header, so an AccessDeniedException body is served untyped

> _Note: this issue was identified against the code base at commit `2ab5ac8cad8a83a47384c8ee23b8dea34a7c8513`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTables

## Description
Cause 1 of 1: DynamoDB error responses omit the awsJson1_0 Content-Type header.

AWS: DynamoDB speaks awsJson1_0 (Smithy: service DynamoDB_20120810, protocols [awsJson1_0], sdkId DynamoDB, endpointPrefix dynamodb). Every awsJson1_0 response — success AND error — announces `Content-Type: application/x-amz-json-1.0`, with the error type carried in the body's `__type` member (there is no `x-amzn-query-error` shim header on plain awsJson1_0, unlike SQS). For a role session with no identity-based permission, ListTables is refused HTTP 400 with error code `AccessDeniedException` and the implicit-deny message whose fixed text is `User: <caller ARN> is not authorized to perform: <service>:<Action> on resource: <resource ARN> because no identity-based policy allows the <service>:<Action> action` — the caller ARN (here the assumed-role ARN `arn:aws:sts::<account>:assumed-role/<role>/<session>`), the action (`dynamodb:ListTables`) and the resource (`arn:aws:dynamodb:<region>:<account>:table/*`) are the variable parts taken from the request and the account/region in play; no value from any single run is part of the requirement.

Emulator: the refusal itself is right — the check's code, message and HTTP 400 assertions all pass — but the response's Content-Type header is absent as far as the client can see: the capture reads the response header named `Content-Type` and gets `None` (`AssertionError: dynamodb Content-Type was None`), where AWS serves `application/x-amz-json-1.0`. Either the header is not emitted on the error path at all, or it is emitted under a name a client reading `Content-Type` does not find. The failure stops the check at its first Content-Type assertion, so the same omission on the other protocol families exercised by this check (sqs/ec2/iam/sns/sts/lambda/s3) is untested and may be present too.

Why: a client that dispatches on the response media type cannot tell an awsJson1_0 error body from an opaque payload. Generic HTTP clients, proxies, gateways and SDK middleware that branch on Content-Type (and anything that checks the protocol's media type before parsing `__type`) will fail to parse or will mis-handle the refusal, so an authorization denial surfaces as an unparsable/garbled response instead of AccessDeniedException.

Fix: the DynamoDB awsJson1_0 error serializer / the HTTP edge that builds error responses for the service — it must set the same `Content-Type: application/x-amz-json-1.0` on error responses that it sets on successful ones, spelled as the protocol's media type and visible under the header name `Content-Type`.

Traces: failing check dynamodb__ec2__iam__lambda__s3__sns__sqs__sts.one-denied-principal-error-shape-per-protocol-family (assertion at the dynamodb leg: `assert ctype == "application/x-amz-json-1.0"`).

## Evidence
Check: dynamodb__ec2__iam__lambda__s3__sns__sqs__sts.one-denied-principal-error-shape-per-protocol-family [mismatch]

Step: a freshly created role with zero attached/inline policies is assumed; the resulting session calls dynamodb ListTables(Limit=1).

Expected (AWS): HTTP 400, error code AccessDeniedException, message `User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: dynamodb:ListTables on resource: arn:aws:dynamodb:<region>:<account>:table/* because no identity-based policy allows the dynamodb:ListTables action`, response header `Content-Type: application/x-amz-json-1.0`, body containing a `"__type"` member and NO `x-amzn-query-error` header.

Actual (emulator): code, message and HTTP 400 match; the response's Content-Type header is not observable —
  File "check_dynamodb__ec2__iam__lambda__s3__sns__sqs__sts_one_denied_principal_error_shape_per_protocol_family.py", line 145, in run
    assert ctype == "application/x-amz-json-1.0", f"dynamodb Content-Type was {ctype!r}"
  AssertionError: dynamodb Content-Type was None

Fixed text of the requirement: the media type string `application/x-amz-json-1.0` and the header name `Content-Type`. Variable parts (account, region, role name, session name) come from the request and the emulator's state, not from this run.

## Steps to Reproduce
1. iam.create_role with a trust policy allowing the account root to sts:AssumeRole; attach NO managed or inline policy.
2. sts.assume_role on that role (RoleSessionName="probe") to obtain session credentials.
3. With those credentials, call dynamodb ListTables(Limit=1) and capture the raw response via a botocore `before-parse` hook.
4. Observe the HTTP status (400) and error code (AccessDeniedException) are correct, but the captured response headers contain no `Content-Type: application/x-amz-json-1.0`.
Replay: dynamodb__ec2__iam__lambda__s3__sns__sqs__sts.one-denied-principal-error-shape-per-protocol-family
