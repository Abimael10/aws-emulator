Issue-Source: trace-sweep-divergence
Issue-Trace: lambda-esm-dynamodb-stream-at-timestamp-starting-position-rejected
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-esm-dynamodb-stream-at-timestamp-starting-position-rejected lambda-rmq-traj-mapping-request-shape-mistakes lambda-function-url-config-lifecycle lambda-lvau-traj-key-rename-breaks-handler
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda|CreateEventSourceMapping|4xx->4xx|$.message
Issue-Key: lambda|CreateFunctionUrlConfig|2xx->2xx|$.FunctionUrl
Issue-Key: lambda|Invoke|2xx->2xx|$.stackTrace[*]
Issue-Commit: 8702776e504117afd39650a0117de2e045f05a5d

# lambda response fidelity: wrong ESM starting-position refusal text, non-AWS function URL form, and stdlib stack frames in Invoke errors

> _Note: this issue was identified against the code base at commit `8702776e504117afd39650a0117de2e045f05a5d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Three net-new lambda wire divergences found by replaying the simparity-wire-aws-v1 corpus (174 fixtures, 1889 steps). All three are response-payload fidelity faults: the emulator reaches the right status and the right error class but serves text / value forms that are not the ones AWS serves. THIS BUNDLE CONTAINS 3 CAUSES; all three must be fixed and re-replayed before closing.

=== Cause 1 of 3: CreateEventSourceMapping's refusal of an unsupported StartingPosition uses the emulator's own sentence instead of AWS's ===
Fixture: lambda-esm-dynamodb-stream-at-timestamp-starting-position-rejected, step task#6/task#7 (cluster representative: task#7). Also affects lambda-rmq-traj-mapping-request-shape-mistakes#task#2.
AWS: Smithy (com-amazonaws-lambda-awsgirapiservice) CreateEventSourceMapping: POST /2015-03-31/event-source-mappings, success code 202, StartingPosition targets the EventSourcePosition enum, errors include InvalidParameterValueException (httpError 400). The model does not state the text, so: recorded behavior only — the recording answers HTTP 400 with the message "Unsupported starting position for arn type: <event-source-arn>". Fixed text: "Unsupported starting position for arn type: "; the single variable part is the EventSourceArn taken verbatim from the request (in this recording a DynamoDB table stream ARN, but the ARN itself is a request value, not part of the requirement).
Emulator: same status 400 and same error type, but the message body is "StartingPosition AT_TIMESTAMP is not supported for the event source '<event-source-arn>'" — different fixed text, the starting-position name interpolated where AWS names none, and the ARN quoted where AWS leaves it bare.
Why: clients and tests that match on the refusal text (SDK error-message assertions, retry/branch logic keyed on the message, log scrapers) do not recognise the refusal, and a user comparing emulator output against AWS sees a different diagnosis of the same rejection.
Fix: the CreateEventSourceMapping handler's starting-position validation — the place that builds the InvalidParameterValueException message must emit AWS's template "Unsupported starting position for arn type: {eventSourceArn}" for every arn type that does not support the requested position, not a per-position sentence.
Traces: lambda-esm-dynamodb-stream-at-timestamp-starting-position-rejected#task#7; lambda-rmq-traj-mapping-request-shape-mistakes#task#2.

=== Cause 2 of 3: CreateFunctionUrlConfig returns a path-shaped FunctionUrl instead of AWS's url-id subdomain form ===
Fixture: lambda-function-url-config-lifecycle, step task#8.
AWS: Smithy CreateFunctionUrlConfig: POST /2021-10-31/functions/{FunctionName}/url, success code 201, output member FunctionUrl is @required with @length min 40 max 100. AWS documents and the recording shows the function URL as a single endpoint whose url-id is the FIRST DNS LABEL of the host: https://<url-id>.lambda-url.<region>.on.aws/ — the url-id is a per-config generated value and the region comes from the endpoint; the requirement is the FORM (url-id as leading host label, lambda-url service label, trailing slash), not any particular id.
Emulator: answers 201 but serves FunctionUrl = "https://<host>:<port>/_aws/lambda-url/<32-hex-id>/" — the url-id is carried as a PATH segment under a fixed /_aws/lambda-url prefix and no lambda-url label appears in the host at all, so the recorded value's shape does not match. (The concrete host and port in the recording are environment values and are not the finding; the structural form is.)
Why: a client that parses the url-id out of the leading host label, or that builds/validates a function-URL endpoint from the documented form (CORS origin checks, SDK/CDK consumers, anything routing by subdomain), gets the wrong id or rejects the value outright; and the @length-bounded, host-labelled shape AWS guarantees is not what is stored or re-read.
Fix: the CreateFunctionUrlConfig handler/serializer where the function URL is minted (and the state that stores it, so Get/List read back the same value) must render the url-id as the leading host label of a lambda-url endpoint rather than as a path segment. Related but NOT covering this: open issue 20261117T041130Z-lambda-function-url-instants-and-the-getfunctioncodesigningconfig-read.md owns the CreationTime/LastModifiedTime instants on the same operations, not the FunctionUrl value.
Traces: lambda-function-url-config-lifecycle#task#8.

=== Cause 3 of 3: Invoke's unhandled-error payload reports standard-library stack frames from the filesystem, with a source line, instead of AWS's frozen-stdlib frame ===
Fixture: lambda-lvau-traj-key-rename-breaks-handler, step task#7.
AWS: Smithy Invoke: POST /2015-03-31/functions/{FunctionName}/invocations, status 200 with X-Amz-Function-Error set and the runtime's error document as the payload; the model says nothing about the document, so: recorded behavior only — the recording's stackTrace is a list of formatted traceback frame strings, and the entry for the frame inside the Python standard library has a different shape than the emulator's (the python3.12 managed runtime serves its stdlib frozen, so such a frame renders as a single line naming a frozen module and carries no source-line text). Fixed structure: one string per frame, '  File "<origin>", line <n>, in <name>'; the origin, line number and any source text are values of the runtime build, not requirements.
Emulator: for the same KeyError the second stackTrace entry is '  File "/var/lang/lib/python3.12/os.py", line 709, in __getitem__\n    raise KeyError(key) from None\n' — a filesystem path to the stdlib module plus an appended source line, which is not the shape AWS records for a stdlib frame. Hypothesis: the emulator's runtime image loads the standard library from disk rather than from frozen modules, so traceback formatting finds a source file and emits the extra line.
Why: a client that parses the error document's stackTrace (error-reporting integrations, tests asserting on the frame list, log/diagnostic tooling) sees extra frames content and an extra line per stdlib frame, and the leaked absolute stdlib path differs from everything AWS returns, so traceback-shaped assertions and fingerprints that work against AWS fail here.
Fix: the Invoke error-payload serialization in the python runtime support (how the runtime formats the traceback into the stackTrace list) — stdlib frames must render in the one-line, no-source form the managed runtime produces, instead of a filesystem path plus source text.
Traces: lambda-lvau-traj-key-rename-breaks-handler#task#7.

## Evidence
Replay summary: trace_set_id=simparity-wire-aws-v1, emulator_up=true, bootstrap_signal=false, steps_total=1889, steps_passed=1856, steps_mismatched=32, steps_errored=1, score_pct=98.3, divergence_count=18 (15 already owned by open issues or stub-filed; the 3 below are net-new).

Cause 1 — CreateEventSourceMapping, expected 400 / actual 400, key $.message:
  expected: 'Unsupported starting position for arn type: arn:aws:dynamodb:us-west-2:000000000000:table/sim2real-probe-lambda-esmts-tbl-c0b48aea/stream/2026-11-24T02:38:39.594'
  actual:   "StartingPosition AT_TIMESTAMP is not supported for the event source 'arn:aws:dynamodb:us-west-2:000000000000:table/sim2real-probe-lambda-esmts-tbl-c0b48aea/stream/2026-11-24T02:38:39.594'"
  divergence_key: service=lambda|CreateEventSourceMapping|status=400->400|keys=$.message; affected_count=2.

Cause 2 — CreateFunctionUrlConfig, expected 201 / actual 201, key $.FunctionUrl:
  expected: '<shaped-not-asserted>' (recorded shape: https://<url-id>.lambda-url.<region>.on.aws/)
  actual:   'https://127.0.0.1:36241/_aws/lambda-url/235b0172a3ad463aa55e9a2573273b8b/'
  divergence_key: service=lambda|CreateFunctionUrlConfig|status=201->201|keys=$.FunctionUrl; affected_count=1.
  Smithy: CreateFunctionUrlConfig output FunctionUrl @required, @length min 40 max 100.

Cause 3 — Invoke, expected 200 / actual 200, key $.stackTrace[1]:
  expected: '<shaped-not-asserted>' (recorded shape: a single frozen-stdlib frame line, no source text)
  actual:   '  File "/var/lang/lib/python3.12/os.py", line 709, in __getitem__\n    raise KeyError(key) from None\n'
  divergence_key: service=lambda|Invoke|status=200->200|keys=$.stackTrace[1]; affected_count=1.

## Steps to Reproduce
Replay the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout (replay_wire_corpus), then re-run the three fixtures named below.

Cause 1: fixture lambda-esm-dynamodb-stream-at-timestamp-starting-position-rejected, step task#7 — POST /2015-03-31/event-source-mappings for a DynamoDB table-stream EventSourceArn with StartingPosition=AT_TIMESTAMP; observe the 400 body's message. Same cluster: lambda-rmq-traj-mapping-request-shape-mistakes#task#2.

Cause 2: fixture lambda-function-url-config-lifecycle, step task#8 — POST /2021-10-31/functions/{FunctionName}/url with AuthType; observe FunctionUrl in the 201 body (and that Get/ListFunctionUrlConfigs read the same value back).

Cause 3: fixture lambda-lvau-traj-key-rename-breaks-handler, step task#7 — invoke a python3.12 function whose handler reads a now-missing environment variable (os.environ[...] raising KeyError); observe the 200 response's error document stackTrace, entry [1].
