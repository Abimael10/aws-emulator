Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda.deep-apigateway-2-apigw-nonproxy-content-handling-converts-request-body-bytes apigateway__lambda.deep-apigateway-2-apigw-nonproxy-convert-to-binary-rejects-malformed-base64-with-500 apigateway__lambda.deep-apigateway-2-apigw-nonproxy-content-handling-converts-response-body-bytes apigateway__lambda.deep-apigateway-2-apigw-nonproxy-request-override-header-and-querystring-reach-backend
Issue-Key: apigateway__lambda.deep-apigateway-2-apigw-nonproxy-content-handling-converts-request-body-bytes|run#14|AssertionError|assert seen["sha256"] == _sha(raw) and seen["len"] == len(raw), (
Issue-Key: apigateway__lambda.deep-apigateway-2-apigw-nonproxy-convert-to-binary-rejects-malformed-base64-with-500|run#15|AssertionError|assert status == 200 and json.loads(data)["sha256"] == _sha(good), (
Issue-Key: apigateway__lambda.deep-apigateway-2-apigw-nonproxy-content-handling-converts-response-body-bytes|run#15|AssertionError|assert _sha(got) == _sha(raw), (
Issue-Key: apigateway__lambda.deep-apigateway-2-apigw-nonproxy-request-override-header-and-querystring-reach-backend|backend_saw#18|AssertionError|assert status == 200, f"POST /{path} returned {status!r}: {data[:300]!r}"
Issue-Commit: a39251e5c9db86afe29d1902591682be414d06f8

# apigateway__lambda: 3 net-new causes — integration contentHandling is ignored on the request and on the response (no base64 conversion), and a request template that sets $context.requestOverride answers 500

> _Note: this issue was identified against the code base at commit `a39251e5c9db86afe29d1902591682be414d06f8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Three net-new root causes in the non-proxy (`type=HTTP`) integration transformation pipeline of the execute-api data plane, grounded in four failing behavioral checks. A fixer must close ALL THREE before submitting.

Context: open issue 20261107T124627Z-apigateway-lambda-3-net-new-causes-non-proxy.md lists these four check ids, but its replay keys are the old `_poll_http` TIMEOUTs of its cause 1 ("non-proxy HTTP integrations never answer"). The stage now ANSWERS, so the replay no longer matches those keys and reported these four checks as unowned: what follows are the transformations that are still not applied once the hop completes. That issue does not carry these causes.

=== Cause 1 of 3: `Integration$contentHandling` is ignored on the integration REQUEST — the body reaches the backend unconverted ===

AWS: Smithy (services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json): `Integration$contentHandling` targets `ContentHandlingStrategy` with values `CONVERT_TO_BINARY | CONVERT_TO_TEXT` and documents "Specifies how to handle request payload content type conversions... If this property is not defined, the request payload will be passed through from the method request to integration request without modification". The concrete contract: with `CONVERT_TO_BINARY`, API Gateway base64-DECODES a text method-request body and the backend receives the decoded raw bytes; with `CONVERT_TO_TEXT` it base64-ENCODES a binary body and the backend receives the base64 text; each mode is a no-op when the payload is already of its target kind; an integration with contentHandling unset forwards the bytes byte-identical.

Emulator: the conversion is never performed — the backend hashes exactly the bytes the client sent. On a `CONVERT_TO_BINARY` resource fed base64 text for 320 raw bytes, the backend reported the sha256 and length of the 428-byte base64 TEXT instead of the 320 decoded bytes (the check states the base64 text's own digest and the emulator's digest are identical). The same is visible on a second check whose well-formed-base64 control (32 raw bytes, 44 base64 chars) was answered 200 with the digest of the 44 text bytes.

Why: every binary-payload workflow built on the classic non-proxy integration is corrupted rather than refused: the backend receives base64 text where it expects bytes (or vice-versa), so uploads are double-encoded, digests/content-lengths disagree, and a client gets a 200 carrying wrong data with no error to detect. Because the control case fails, the companion refusal contract (a `CONVERT_TO_BINARY` body that is not valid base64 must be answered 500) cannot be exercised at all and is still unverified.

Fix: the execute-api data-plane integration-request transform for `type=HTTP`/`AWS` (non-proxy) — apply `contentHandling` to the method-request payload (base64-decode for CONVERT_TO_BINARY, base64-encode for CONVERT_TO_TEXT, pass through when unset or already of the target kind) before the hop to the integration endpoint, and answer 500 when CONVERT_TO_BINARY is given a body that is not valid base64.

Traces: apigateway__lambda.deep-apigateway-2-apigw-nonproxy-content-handling-converts-request-body-bytes; apigateway__lambda.deep-apigateway-2-apigw-nonproxy-convert-to-binary-rejects-malformed-base64-with-500.

=== Cause 2 of 3: `IntegrationResponse$contentHandling` is ignored on the integration RESPONSE — the client gets the backend's body unconverted ===

AWS: Smithy, same model: `IntegrationResponse$contentHandling` targets `ContentHandlingStrategy` (`CONVERT_TO_BINARY | CONVERT_TO_TEXT`) and documents "Specifies how to handle response payload content type conversions... If this property is not defined, the response payload will be passed through from the integration response to the method response without modification". The contract: `CONVERT_TO_BINARY` base64-decodes a text backend body into raw bytes for the client, `CONVERT_TO_TEXT` base64-encodes a binary backend body as text, and an explicit setting does NOT depend on the client's `Accept` header (an UNSET integration response is the one that follows the Accept/`binaryMediaTypes` passthrough rule).

Emulator: no conversion happens. On a `CONVERT_TO_BINARY` integration response whose backend answers base64 text for 320 raw bytes, the client received the 428-byte base64 text (different digest, different length) instead of the decoded bytes.

Why: a client of a binary-serving REST API receives base64 text where the API is configured to hand it bytes (or the reverse), with a 200 status — images/archives/octet-streams are silently corrupted, and the Accept-driven passthrough rule for UNSET integration responses cannot be distinguished from the explicit setting.

Fix: the execute-api data-plane integration-response transform — apply the integration response's `contentHandling` to the backend payload before writing the method response (independently of `Accept`), and keep the unset case on the documented Accept/`binaryMediaTypes` passthrough rule.

Traces: apigateway__lambda.deep-apigateway-2-apigw-nonproxy-content-handling-converts-response-body-bytes.

=== Cause 3 of 3: a request mapping template that sets `$context.requestOverride.header.*` / `.querystring.*` makes the stage answer 500 instead of applying the override ===

AWS: a request `requestTemplates` VTL body may contain `#set($context.requestOverride.header.<name> = "<value>")` and `#set($context.requestOverride.querystring.<name> = "<value>")`; API Gateway evaluates the template, sends the rendered body to the backend, and the override REPLACES whatever the same integration's `requestParameters` mapped for that header/query parameter (Smithy `Integration$requestParameters` documents the `method.request.{location}.{name}` → integration-parameter mapping that the override beats). With nothing to override, the template still CREATES the header and query parameter. Recorded behavior only for the override semantics (the data plane is not in the Smithy model); the recording shows the override resource answering 200 with the backend seeing the override values.

Emulator: the twin resource that differs ONLY in not setting any override answers 200 and correctly forwards the client's mapped header and query value, while the resource whose template carries the two `#set($context.requestOverride...)` directives answers HTTP 500 with body `{"message":"Internal server error"}` — the generic gateway error, so template evaluation itself fails on the `$context.requestOverride` reference. Fixed text here is the sentence `Internal server error` in the `message` member; nothing in that body comes from the request, and it is the WRONG answer: AWS answers 200.

Why: the documented way to compute a backend header or query parameter in VTL turns the whole method into an unconditional 500. A client cannot use parameter overrides at all, and the failure is indistinguishable from a backend outage.

Fix: the execute-api data-plane VTL evaluator / integration-request builder — support `$context.requestOverride.header.*`, `.querystring.*` (and `.path.*`) as assignable template context, and let the resulting values replace the `requestParameters`-mapped values on the outgoing integration request instead of raising into the generic 500 path.

Traces: apigateway__lambda.deep-apigateway-2-apigw-nonproxy-request-override-header-and-querystring-reach-backend (the 'mapped' twin's control assertions passed; the POST to the override resource returned 500).

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1), service heading `### apigateway__lambda`, 4 failing checks reported as unowned:

Cause 1:
  [mismatch] deep-apigateway-2-apigw-nonproxy-content-handling-converts-request-body-bytes
  AssertionError: CONVERT_TO_BINARY must base64-decode the text body: the backend should hash the 320 decoded bytes (f50f839f6df3…), it hashed 428 bytes (ff5bbe410afc…); the base64 text itself would hash to ff5bbe410afc… (i.e. the emulator forwarded the base64 text verbatim)
  Setup: CreateRestApi(binaryMediaTypes=["application/octet-stream"]); four child resources (tobin=CONVERT_TO_BINARY, tobin-unset=unset, totext=CONVERT_TO_TEXT, totext-unset=unset), each PutMethod(POST, NONE) + PutIntegration(type="HTTP", integrationHttpMethod="POST", uri=<echo backend>/echo, requestParameters={integration.request.header.Content-Type: "'application/octet-stream'"}[, contentHandling=…]) + PutMethodResponse/PutIntegrationResponse(200); CreateDeployment(stageName="live"); POST base64 text with Content-Type: text/plain.
  Expected: the backend hashes the DECODED bytes. Actual: it hashes the base64 text (status 200).

  [mismatch] deep-apigateway-2-apigw-nonproxy-convert-to-binary-rejects-malformed-base64-with-500
  AssertionError: control: well-formed base64 must decode to the 32 raw bytes (d0f3046c6de3…) at the backend; got 200: {"sha256": "9893963e03c7…", "len": 44, …}
  (44 = the length of the base64 TEXT; the malformed-base64 → 500 assertion was never reached.)

Cause 2:
  [mismatch] deep-apigateway-2-apigw-nonproxy-content-handling-converts-response-body-bytes
  AssertionError: CONVERT_TO_BINARY must decode the backend's base64 text into the 320 raw bytes (165d8d03aab6…); the client got 428 bytes (d06d417a9f4a…): b'AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwd'
  Setup: same API shape, GET methods, PutIntegrationResponse(200, contentHandling=CONVERT_TO_BINARY | CONVERT_TO_TEXT | unset) against backend paths /emit-text and /emit-binary; GET with Accept: text/plain resp. application/octet-stream.
  Expected: the client receives the decoded raw bytes regardless of Accept. Actual: it receives the backend's base64 text (status 200).

Cause 3:
  [mismatch] deep-apigateway-2-apigw-nonproxy-request-override-header-and-querystring-reach-backend
  AssertionError: POST /override returned 500: b'{"message":"Internal server error"}'
  Setup: two resources with IDENTICAL method/integration configuration (PutMethod(POST, NONE, requestParameters={method.request.header.x-ovr: false, method.request.querystring.qovr: false}); PutIntegration(type="HTTP", POST, uri=<echo>/echo, requestParameters={integration.request.header.x-ovr: "method.request.header.x-ovr", integration.request.querystring.qovr: "method.request.querystring.qovr"})) differing only in the request template: /mapped uses '{"templated":true}', /override prefixes it with `#set($context.requestOverride.header.x-ovr = "hdr-…")` and `#set($context.requestOverride.querystring.qovr = "qs-…")`.
  Expected: both answer 200; /mapped forwards the client's values (this control PASSED), /override forwards the template's values. Actual: /override answers 500 {"message":"Internal server error"}.

(All digests, lengths and generated values above are one run's; the requirements are the conversion rules, the 200-vs-500 status and the body text.)

## Steps to Reproduce
1. git_clone(), then run_behavioral_suite().
2. In the report find `### apigateway__lambda` and replay the four `deep-apigateway-2-apigw-nonproxy-*` checks listed in trace_ids.
3. Cause 1 by hand: CreateRestApi(binaryMediaTypes=["application/octet-stream"]); child resource; PutMethod(POST, authorizationType=NONE); PutIntegration(type="HTTP", integrationHttpMethod="POST", uri=<an echo HTTP backend>, contentHandling="CONVERT_TO_BINARY"); PutMethodResponse(200)/PutIntegrationResponse(200, selectionPattern=""); CreateDeployment(stageName="live"); POST base64 text (Content-Type: text/plain) and have the backend report sha256/length of what it received. AWS: the decoded bytes. Emulator: the base64 text. Repeat with contentHandling="CONVERT_TO_TEXT" and a raw binary body (AWS: base64 text at the backend), and with contentHandling unset (byte-identical both ways). Then POST a body that is NOT valid base64 to the CONVERT_TO_BINARY resource: AWS answers 500.
4. Cause 2 by hand: same API, a GET method whose backend answers base64 TEXT, PutIntegrationResponse(200, contentHandling="CONVERT_TO_BINARY"); GET the stage path with Accept: text/plain and with Accept: application/octet-stream. AWS hands the client the decoded raw bytes in both cases; the emulator hands back the base64 text. Mirror with CONVERT_TO_TEXT over a binary backend answer, and with contentHandling unset (Accept/binaryMediaTypes passthrough).
5. Cause 3 by hand: two resources configured identically (method requestParameters declaring header x-ovr and querystring qovr, integration requestParameters mapping both to the backend) whose request templates differ only in that one prefixes the body with `#set($context.requestOverride.header.x-ovr = "H")` and `#set($context.requestOverride.querystring.qovr = "Q")`. POST both. AWS answers 200 on both and the override resource's backend sees x-ovr=H and qovr=Q (even when the client sends neither); the emulator answers 200 on the plain resource and 500 {"message":"Internal server error"} on the override resource.
