Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.import-rest-api-malformed-spec-rejected
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.import-rest-api-malformed-spec-rejected|run#15|AssertionError|assert message == "Invalid OAS input", f"unexpected rejection message {message!r}"
Issue-Commit: 65202ee683bcb66d82ebd52a38b6336bbaa34d8b

# ImportRestApi: an unparseable document is refused with 'Unable to determine OAS/Swagger version' instead of AWS's 'Invalid OAS input'

> _Note: this issue was identified against the code base at commit `65202ee683bcb66d82ebd52a38b6336bbaa34d8b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ImportRestApi

## Description
One net-new root cause in the apigateway control plane's ImportRestApi body parsing. Numbered below with its failing check id.

Cause 1 of 1: ImportRestApi refuses a body that is not a parseable OpenAPI/Swagger document with the WRONG message sentence.

AWS: ImportRestApi (Smithy: POST /restapis?mode=import, input member `body` is the required @httpPayload Blob; modeled errors BadRequestException(400), ConflictException(409), LimitExceededException(429), NotFoundException(404), TooManyRequestsException(429), UnauthorizedException(401)) refuses a body it cannot parse at all with HTTP 400 `BadRequestException` and the message exactly `Invalid OAS input`, and creates no RestApi. That sentence is entirely FIXED text — it carries no value taken from the request or from state. It is the refusal for the "the document did not parse" class: a byte string that is not valid JSON (or YAML) has no `swagger`/`openapi` key to inspect in the first place, so AWS never reaches the version-detection step for it. AWS's separate sentence `Unable to determine OAS/Swagger version` belongs to a DIFFERENT class — a document that parses but declares neither a `swagger` nor an `openapi` version.

Emulator: for a body that is truncated, unterminated JSON (the check sends `{"openapi": "3.0.1", "info": {"title": "<unique-title>" NOT VALID JSON !!!`, i.e. the stream ends mid-object), the emulator does refuse with HTTP 400 `BadRequestException` — the status, the error code and the no-partial-API guarantee are all correct — but the `Error.Message` is `Unable to determine OAS/Swagger version`. It answers the version-detection sentence for an input that never parsed, so the two refusal classes are collapsed into one.

Why: a client (or an importer/CI wrapper) that matches on the refusal text to tell a user "your file is not valid JSON/YAML" apart from "your file is missing its `openapi:`/`swagger:` declaration" is told the wrong thing, and any assertion on the exact message AWS emits fails. Because the emulator's chosen sentence is a real AWS sentence for a neighbouring condition, the defect is invisible to a code/status check and only shows up on the text.

Fix: the ImportRestApi handler's document-parsing/validation step in the apigateway (BackplaneControlService) crate — the same rendering path that the already-filed `failOnWarnings` refusal uses (`put_rest_api::warnings_refusal`, see the resolved issue issues/things-that-now-work/20261105T160000Z-apigateway-base-path-mapping-guard-and-import-refusal-sentences.md, which settles the `failOnWarnings` sentence and NOT this one). It must distinguish "body did not parse" (`Invalid OAS input`) from "parsed but no version key" (`Unable to determine OAS/Swagger version`) before it picks a message. No other part of the refusal needs to change.

Traces: apigateway.import-rest-api-malformed-spec-rejected (the check's positive control — a well-formed Swagger 2.0 document with a MOCK integration — imports, is tagged and is readable back by GetRestApi, so only the malformed-body refusal message diverges).

## Evidence
Failing check apigateway.import-rest-api-malformed-spec-rejected, [mismatch]:

  File "check_apigateway_import_rest_api_malformed_spec_rejected.py", line 71, in run
    assert message == "Invalid OAS input", f"unexpected rejection message {message!r}"
  AssertionError: unexpected rejection message 'Unable to determine OAS/Swagger version'

Expected (AWS): HTTP 400, Error.Code `BadRequestException`, Error.Message `Invalid OAS input` (fixed text, no variable parts).
Actual (emulator): HTTP 400, Error.Code `BadRequestException`, Error.Message `Unable to determine OAS/Swagger version`.

The preceding assertion in the same check (`code == "BadRequestException"`) passed, and the trailing assertions passed as well: the control RestApi is still listed and no RestApi named with the broken document's title was created — so the status, the code and the atomicity of the refusal are all already correct.

## Steps to Reproduce
1. Positive control: ImportRestApi with a well-formed Swagger 2.0 document (info.title set, one GET /hello with x-amazon-apigateway-integration type MOCK). It returns 201 and GetRestApi echoes `name` == info.title.
2. Call ImportRestApi with a body that is not parseable JSON, e.g. the truncated/unterminated bytes `{"openapi": "3.0.1", "info": {"title": "my-title" NOT VALID JSON !!!`.
3. Expect HTTP 400 with Error.Code `BadRequestException` and Error.Message exactly `Invalid OAS input`. The emulator answers `Unable to determine OAS/Swagger version`.
4. GetRestApis: no RestApi carrying the malformed document's title exists (this part already holds).
