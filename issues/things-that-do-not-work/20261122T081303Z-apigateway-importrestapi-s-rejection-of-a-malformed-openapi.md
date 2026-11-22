Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.import-rest-api-malformed-spec-rejected
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.import-rest-api-malformed-spec-rejected|run#15|AssertionError|assert message == "Invalid OAS input", f"unexpected rejection message {message!r}"
Issue-Commit: 704534238375234ddf051963f5bbfba4730874a8

# apigateway: ImportRestApi's rejection of a malformed OpenAPI/Swagger body carries the emulator's own message "Invalid OpenAPI input." instead of AWS's "Invalid OAS input"

> _Note: this issue was identified against the code base at commit `704534238375234ddf051963f5bbfba4730874a8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ImportRestApi

## Description
One cause.

Cause 1 of 1: the BadRequestException message for an unparseable import document is mis-worded.

AWS: ImportRestApi (Smithy: POST /restapis?mode=import, body @httpPayload Blob required, modeled errors BadRequestException(400), ConflictException(409), LimitExceededException(429), NotFoundException(404), TooManyRequestsException(429), UnauthorizedException(401)). When the request body is not a parseable OpenAPI/Swagger document at all, API Gateway refuses with HTTP 400, Error.Code `BadRequestException` and the fixed Error.Message `Invalid OAS input` — recorded behavior: the message for this error class is entirely fixed text, it interpolates nothing from the request (no offset, no title, no parse-error detail) and it carries no trailing period. The model constrains only the shape; the message text is the recorded requirement.

Emulator: the status and the error code are right (BadRequestException), but the message body is the emulator's own phrasing, `Invalid OpenAPI input.` — a different noun ("OpenAPI" where AWS says "OAS") plus a trailing period that AWS does not emit. The rest of the check passes: no partial RestApi is created and the positive-control import still lists.

Why: a client that branches on, logs or asserts the refusal reason for a bad import never matches. Tooling that distinguishes "malformed document" (`Invalid OAS input`) from the other BadRequestException reasons ImportRestApi returns — unsupported extension, unknown basepath mode, failOnWarnings — has to key off the message, because the code is the same for all of them; with this wording it falls through to its generic branch and reports an unclassified import failure.

Fix: the ImportRestApi handler's body-parse/validation path in the apigateway service crate — the string it builds for the unparseable-document BadRequestException. No other behavior of the operation is involved.

Traces: apigateway.import-rest-api-malformed-spec-rejected (mismatch: `AssertionError: unexpected rejection message 'Invalid OpenAPI input.'`). Note: no open issue covers this; the nearest ones, 20261113T185449Z-apigateway-2-net-new-causes-importdocumentationparts-failonwarnings-rejection.md and 20261121T033135Z-apigateway-3-net-new-wire-causes-getdeployments-empty.md, are about ImportDocumentationParts, not ImportRestApi's parse refusal.

## Evidence
Check: apigateway.import-rest-api-malformed-spec-rejected

Request: ImportRestApi with body = `{"openapi": "3.0.1", "info": {"title": "<unique-title>" NOT VALID JSON !!!` (truncated JSON).

Expected (AWS): HTTP 400, Error.Code = BadRequestException, Error.Message = "Invalid OAS input" (fixed text, no interpolation, no trailing period).

Actual (emulator): HTTP 400, Error.Code = BadRequestException, Error.Message = "Invalid OpenAPI input."

Replay output:
  File "check_apigateway_import_rest_api_malformed_spec_rejected.py", line 71, in run
    assert message == "Invalid OAS input", f"unexpected rejection message {message!r}"
  AssertionError: unexpected rejection message 'Invalid OpenAPI input.'

Smithy (services/service-com-amazonaws-apigateway-backplanecontrolservice): ImportRestApi -> errors include BadRequestException (client, httpError 400); input member `body` is the required @httpPayload blob.

## Steps to Reproduce
1. Boot the emulator and point an apigateway client at it.
2. (Positive control) ImportRestApi with a well-formed Swagger 2.0 document containing one MOCK-integrated GET /hello; it succeeds and GetRestApi returns it.
3. Call ImportRestApi with a deliberately truncated/unparseable body, e.g. b'{"openapi": "3.0.1", "info": {"title": "x" NOT VALID JSON !!!'.
4. Inspect the ClientError: Code is BadRequestException (correct) but Message is 'Invalid OpenAPI input.' where AWS returns 'Invalid OAS input'.
5. GetRestApis confirms no partial API was created (that part already matches).
