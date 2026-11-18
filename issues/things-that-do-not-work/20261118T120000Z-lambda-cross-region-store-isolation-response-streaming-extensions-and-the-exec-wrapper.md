Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.invoke-refuses-a-function-arn-from-another-region lambda.custom-runtime-streaming-response-mode-delivers-chunks-via-invoke-with-response-stream lambda.invoke-with-response-stream-managed-streaming lambda.deep-lambda-runtime-truth-stream-first-chunk-reaches-client-before-handler-finishes lambda.external-extension-registers-receives-invoke-and-shutdown-events lambda.extension-that-never-registers-fails-the-invoke lambda.external-extension-that-exits-before-registering-fails-the-invoke lambda.extension-register-after-init-is-refused lambda.external-extension-layer-runs-identically-across-runtime-families lambda.exec-wrapper-script-starts-the-runtime-with-its-environment-and-arguments lambda.exec-wrapper-naming-a-missing-file-rejected-at-invoke
Issue-Key: lambda.invoke-refuses-a-function-arn-from-another-region|run#15|AssertionError|assert error.get("Message") == expected, (
Issue-Key: lambda|InvokeWithResponseStream|InvalidRuntimeException
Issue-Key: lambda.invoke-with-response-stream-managed-streaming|run#13|AssertionError|assert spread >= 1.0, f"chunks arrived within {spread:.3f}s of each other - looks buffered, not streamed: {chunk_times}"
Issue-Key: lambda.deep-lambda-runtime-truth-stream-first-chunk-reaches-client-before-handler-finishes|run#10|AssertionError|assert rest == b"", f"stream ended mid-line: trailing bytes {rest[:200]!r}"
Issue-Key: lambda.external-extension-registers-receives-invoke-and-shutdown-events|run#20|AssertionError|assert last["ext_log_exists"], (
Issue-Key: lambda.extension-that-never-registers-fails-the-invoke|run#8|AssertionError|assert bad["status"] == 200 and bad["error"] == "Unhandled", (
Issue-Key: lambda.external-extension-that-exits-before-registering-fails-the-invoke|run#8|AssertionError|assert bad["status"] == 200 and bad["error"] == "Unhandled", (
Issue-Key: lambda.extension-register-after-init-is-refused|run#12|AssertionError|assert late, f"no late-register attempt recorded for this invocation: {records!r}"
Issue-Key: lambda.external-extension-layer-runs-identically-across-runtime-families|run#7|AssertionError|assert not cold["function_error"], (
Issue-Key: lambda.exec-wrapper-script-starts-the-runtime-with-its-environment-and-arguments|run#8|AssertionError|assert body["wrapped"] == "set-by-wrapper", (
Issue-Key: lambda.exec-wrapper-naming-a-missing-file-rejected-at-invoke|run#5|AssertionError|assert control["error"] is None and json.loads(control["raw"]) == {"wrapped": "set-by-wrapper"}, (
Issue-Commit: 677b6e35c5226b67b9bfea5e554d250e3c084a9c

# lambda: 5 of the parent report's 8 causes are still open — the function store is not partitioned by Region, a custom runtime's package is never executed, a response stream is buffered by the core router, and nothing runs an extension or an exec wrapper

> _Split out of `issues/things-that-now-work/20261107T091747Z-lambda-code-signing-not-enforced-no-in-flight.md`, whose causes 1, 2 and 3 are fixed. This report carries the replay keys that do NOT pass, re-filed here so they are not recorded as working. Everything below is what that fix established about them; a later fixer starts from here rather than from scratch._

## Operation
Invoke

## Description
Five of the parent report's eight causes are open. One is a control-plane gap with a known
(large) shape, three need one execution capability this emulator does not have, and one is
blocked on the read-only core.

What the parent fix DID land, so a later fixer does not redo it: code signing is enforced on
both deployment operations, an in-flight update is refused with the modelled 409 and the
deployment it replaced keeps serving invocations until the clock makes the update's completion
due (`FunctionRecord::serving_code`), and `Invoke`/`InvokeWithResponseStream` name the ARN the
request addressed with no `$LATEST` the caller never asked for.

Cause 1 of 5: a function created through one Region's endpoint is still reachable through
another's.
AWS: a Lambda function is a REGIONAL resource — it lives in the Region it was created in and is
reachable only through that Region's endpoint, so a `FunctionName` resolved at another Region's
endpoint names nothing there, whatever form it takes.
Emulator: the ARN form is refused correctly — `crate::state::foreign_region_function_message`
answers `Functions from '<named>' are not reachable in this region ('<endpoint>')`, and the
parent fix's replay confirms the check gets past that assertion — but the STORE is not
partitioned: `ComAmazonawsLambdaAwsgirapiserviceState::functions` is keyed by function name
alone, so the bare name of a function created in another Region resolves here and the
invocation is SERVED (200) where AWS answers 404.
Why: a client testing Region isolation sees a function that should not exist at this endpoint,
and two Regions cannot hold the same function name at all.
Fix: re-key that map by (Region, name). It is reached from about sixty call sites, several of
which (the execution slot, the asynchronous delivery, the event-source pollers) are handed a
function name with no Region, so the Region has to be threaded to them; `CreateFunction`'s
duplicate-name check and every listing and account-usage aggregation are part of the same
change.
Traces: lambda.invoke-refuses-a-function-arn-from-another-region.

Cause 2 of 5: InvokeWithResponseStream refuses the custom runtime it must support.
AWS: InvokeWithResponseStream is modeled (POST
/2021-11-15/functions/{FunctionName}/response-streaming-invocations) with
InvalidRuntimeException typed as a SERVER error (HTTP 502) — i.e. not the answer for a validly
configured function. provided.al2023 is a supported Lambda runtime identifier.
Emulator: InvokeWithResponseStream (and Invoke) answer InvalidRuntimeException with message
"The runtime provided.al2023 is not supported by this emulator, which runs the Python and node
runtimes." — an emulator-authored message naming its own limits, and the SDK retries it to max
retries first.
Why: a client exercising a custom-runtime streaming function gets a 502-class server error
instead of its response stream; the message tells it about the emulator rather than about AWS.
Fix: a custom-runtime (`provided.*`) execution path — run the package's own `bootstrap` and
serve the INVOCATION half of the Runtime API, which `crate::state::execution::runtime_api`
deliberately answers as unrouted today (it serves only the SnapStart restore path). That is the
same capability causes 4 and 5 need.
Traces: lambda.custom-runtime-streaming-response-mode-delivers-chunks-via-invoke-with-response-stream.

Cause 3 of 5: InvokeWithResponseStream buffers the whole response instead of streaming it.
AWS: the operation's output member EventStream is @streaming (httpPayload), so chunks reach the
caller as the handler writes them, spread over time.
Emulator: all chunks arrive within 0.000 s of each other (inter-arrival times ~1.4e-4 s), i.e.
the body is produced and then flushed at once; a related check sees the stream end mid-line
with the trailing error document of a run the platform ended.
Why: a client that streams to reduce time-to-first-byte, or that consumes a long-running
response incrementally, cannot observe or validate that behaviour.
Fix: BLOCKED on the read-only core. The router buffers every response body —
`MultiServiceRouter::dispatch` is `http::Request<bytes::Bytes> -> http::Response<bytes::Bytes>`
(vendor/smithy-server-runtime/src/multi_service_router.rs:138-141) — so no handler under
`services/` can hand the client bytes as it produces them, however the invoke path is written.
It needs a core response body an operation can write incrementally (a streaming body type out
of `dispatch`), after which the invoke path can run the handler on its own task and frame each
write as it happens.
Traces: lambda.invoke-with-response-stream-managed-streaming,
lambda.deep-lambda-runtime-truth-stream-first-chunk-reaches-client-before-handler-finishes.

Cause 4 of 5: Lambda extensions never run and the Extensions API lifecycle is not enforced.
AWS: recorded behavior only; AWS executes every executable under /opt/extensions during init,
requires each external extension to call Register before init completes, fails the invocation
when an extension exits before registering or never registers, and refuses a late Register.
Emulator: nothing under /opt/extensions/ is executed (the extension never wrote its /tmp file);
a never-registering extension and a crashing extension both leave the invoke answering 200 with
FunctionError absent and the handler's normal payload; no late-register attempt is recorded at
all; and in the cross-runtime leg the extension's presence turns the invoke into a run the
platform ended on the function's timeout.
Why: a client cannot test an extension (telemetry agent, secrets cache) at all, and a broken
extension that AWS would surface as a failed invocation passes silently.
Fix: executing an executable shipped in a LAYER inside the sandbox — the capability cause 2
needs as well — plus the Extensions API (Register / INVOKE / SHUTDOWN) on the Runtime API
address, and the init phase failing the invocation when a registered extension does not come up.
Traces: lambda.external-extension-registers-receives-invoke-and-shutdown-events,
lambda.extension-that-never-registers-fails-the-invoke,
lambda.external-extension-that-exits-before-registering-fails-the-invoke,
lambda.extension-register-after-init-is-refused,
lambda.external-extension-layer-runs-identically-across-runtime-families.

Cause 5 of 5: AWS_LAMBDA_EXEC_WRAPPER is ignored.
AWS: recorded behavior only; when AWS_LAMBDA_EXEC_WRAPPER names an executable, Lambda starts the
runtime through that wrapper (so the wrapper's exported variables and arguments are visible to
the handler) and fails the invoke at runtime when it names a missing file.
Emulator: the wrapper is not run — the handler's environment does not contain the wrapper's
export (observed None), and a wrapper naming a missing file still yields a successful invoke
(error=None, payload {"wrapped": null}) instead of a runtime failure.
Why: the documented hook for injecting profilers/agents silently does nothing, and a
misconfigured wrapper looks healthy.
Fix: the same layer-executable capability as causes 2 and 4, in the runtime bootstrap of the
invoke execution path: the wrapper runs, its exported environment reaches the handler's, and a
wrapper naming a file the package does not hold fails the invocation.
Traces: lambda.exec-wrapper-script-starts-the-runtime-with-its-environment-and-arguments,
lambda.exec-wrapper-naming-a-missing-file-rejected-at-invoke.

## Evidence
Expected vs actual, from the replay taken after the parent fix:
- invoke-refuses-a-function-arn-from-another-region: AssertionError: real AWS rejects this call, but it SUCCEEDED and returned a 200 invocation answer (x-amz-executed-version '$LATEST') — the Region-mismatch message assertion now passes and the check fails on the later leg
- custom-runtime-streaming-response-mode-delivers-chunks-via-invoke-with-response-stream: botocore.errorfactory.InvalidRuntimeException on InvokeWithResponseStream (after max retries): "The runtime provided.al2023 is not supported by this emulator, which runs the Python and node runtimes."
- invoke-with-response-stream-managed-streaming: AssertionError: chunks arrived within 0.000s of each other - looks buffered, not streamed
- deep-lambda-runtime-truth-stream-first-chunk-reaches-client-before-handler-finishes: AssertionError: stream ended mid-line: trailing bytes of the platform's own timeout document
- external-extension-registers-receives-invoke-and-shutdown-events: AssertionError: the extension never wrote '/tmp/<file>'; nothing under /opt/extensions/ was executed
- extension-that-never-registers-fails-the-invoke: AssertionError: never-registering extension: status=200 FunctionError=None raw=b'{"ok": true}'
- external-extension-that-exits-before-registering-fails-the-invoke: AssertionError: crashing extension: status=200 FunctionError=None raw=b'{"ok": true}'
- extension-register-after-init-is-refused: AssertionError: no late-register attempt recorded for this invocation: []
- external-extension-layer-runs-identically-across-runtime-families: AssertionError: python3.13/arm64: the extension must not break the invocation: FunctionError='Unhandled' with the platform's timeout document
- exec-wrapper-script-starts-the-runtime-with-its-environment-and-arguments: AssertionError: the wrapper's export is not in the handler's environment: None
- exec-wrapper-naming-a-missing-file-rejected-at-invoke: AssertionError: positive control (executable wrapper): error=None raw=b'{"wrapped": null}'

## Steps to Reproduce
1. Region isolation: CreateFunction through one Region's endpoint, then resolve it by its BARE
   NAME through another Region's endpoint -> expect ResourceNotFoundException 404, observe the
   function.
2. Custom runtime: CreateFunction with Runtime='provided.al2023' plus a bootstrap, call
   InvokeWithResponseStream.
3. Streaming: call InvokeWithResponseStream on a supported runtime whose handler writes chunks
   with delays, timing the chunk arrivals.
4. Extensions: attach a layer containing extensions/<name> (executable) and invoke; also use an
   extension that never registers, one that exits before registering, and one that registers
   late.
5. Exec wrapper: set AWS_LAMBDA_EXEC_WRAPPER to a layer-shipped wrapper that exports a
   variable, and to a missing file; invoke in both cases.
6. Re-replay the check ids in trace_ids.
