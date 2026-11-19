Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.layers-bin-on-path-executes-layer-binary lambda.layers-combined-unzipped-size-over-250mb-rejected lambda.python-custom-exception-class-names-error-type-and-frames lambda.python-module-level-exception-rejected-with-its-own-error-type lambda.python-syntax-error-in-handler-module-rejected-at-invoke lambda.python-handler-in-nested-package-resolves-relative-imports-and-files lambda.python-invalid-client-context-rejected-before-the-handler-runs lambda.python-trailing-data-after-json-fails-runtime-unmarshal lambda.python-unparseable-payload-rejected-before-the-handler-runs
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.layers-bin-on-path-executes-layer-binary|run#12|AssertionError|assert ran["ran"] is True, (
Issue-Key: lambda.layers-combined-unzipped-size-over-250mb-rejected|run#11|AssertionError|assert control == {"size": _BLOB}, (
Issue-Key: lambda.python-custom-exception-class-names-error-type-and-frames|run#15|AssertionError|assert body["stackTrace"] == _FRAMES, f"stackTrace {body['stackTrace']!r}, expected {_FRAMES!r}"
Issue-Key: lambda.python-module-level-exception-rejected-with-its-own-error-type|run#5|IndexError|last = body["stackTrace"][-1]
Issue-Key: lambda.python-syntax-error-in-handler-module-rejected-at-invoke|run#4|AssertionError|assert body["stackTrace"] == [' File "/var/task/index.py" Line 2\n return {\'ok\': True\n'], (
Issue-Key: lambda.python-handler-in-nested-package-resolves-relative-imports-and-files|_ok#10|AssertionError|assert r["status"] == 200 and r["error"] is None, (
Issue-Key: lambda.python-invalid-client-context-rejected-before-the-handler-runs|run#7|AssertionError|assert msg == _MSG, f"{label}: message {msg!r}, expected {_MSG!r}"
Issue-Key: lambda.python-trailing-data-after-json-fails-runtime-unmarshal|run#5|AssertionError|assert body["errorMessage"] == want, (
Issue-Key: lambda.python-unparseable-payload-rejected-before-the-handler-runs|run#5|AssertionError|assert msg == want, f"payload {payload!r}: message {msg!r}, expected {want!r}"
Issue-Commit: 0d5fdb6d7a78988c971aec85849026eed9fa5958

# lambda: 8 net-new causes — layer content is not extracted usably (non-executable bin/, 8 MiB file truncation), the Python runtime's error payloads carry no/partial stackTrace, package handlers cannot be imported, and three refusal messages diverge

> _Note: this issue was identified against the code base at commit `0d5fdb6d7a78988c971aec85849026eed9fa5958`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Eight distinct root causes in the `lambda` behavioral suite, all reached through Invoke and the Python managed runtime / layer extraction. Each is numbered with its own failing check ids; all eight must be closed before submitting.

Cause 1 of 8 — a layer's bin/ entry lands at /opt/bin but cannot be executed.
AWS: PublishLayerVersion content (Smithy: PublishLayerVersion.Content -> LayerVersionContentInput, required) is unpacked into /opt in the execution environment; a layer's bin/ directory therefore appears as /opt/bin, /opt/bin is on the environment's PATH, and extracted files are given executable mode, so a handler can run the tool by its bare name via subprocess. The Smithy model says nothing about the /opt layout, so this is recorded/documented runtime behavior only.
Emulator: shutil.which() DOES resolve the tool to /opt/bin/<tool> and PATH does contain /opt/bin, but subprocess.run([<tool>]) raises OSError — the extracted file is present at the right path and is not executable.
Why: every layer that ships a helper binary or CLI (the standard way layers distribute native tooling) is unusable; the handler sees the tool on PATH and then cannot run it, which is worse than it being absent.
Fix: the layer-extraction step of the execution-environment setup — file mode/permission handling for extracted layer content.
Traces: lambda.layers-bin-on-path-executes-layer-binary.

Cause 2 of 8 — layer file content is truncated at 8 MiB on extraction.
AWS: layer content is extracted byte-for-byte, so a large file inside a layer appears under /opt at its full size; only the COMBINED unzipped size of a function's layers exceeding the 250 MB quota is refused (InvalidParameterValueException, 400, at CreateFunction/UpdateFunctionConfiguration). The single-layer control therefore reads the full declared file size.
Emulator: the single-layer control reports the file's size as 8388608 bytes (8 MiB) instead of the full 146800640-byte file it published — extraction stops at an 8 MiB ceiling. Hypothesis: a fixed 8 MiB read/extract buffer caps every extracted layer file.
Why: layer payloads are silently corrupted rather than refused — a handler reading a layer asset gets a truncated file with no error — and the 250 MB combined-size quota can never be reached, so the quota refusal is unreachable too.
Fix: layer content extraction/storage in the execution-environment setup (remove the per-file size ceiling), and the combined-unzipped-size quota check that depends on it.
Traces: lambda.layers-combined-unzipped-size-over-250mb-rejected.

Cause 3 of 8 — every stackTrace frame in a Python unhandled-error payload drops its source line.
AWS: the Python managed runtime's unhandled-error payload serialises stackTrace as traceback.format_tb output, so each entry is a two-line string: '  File "<file>", line <n>, in <fn>\n    <the source line of that frame>\n'. The file name, line number, function name and source text are all values taken from the raised traceback.
Emulator: each entry is only '  File "<file>", line <n>, in <fn>\n' — the indented source-line continuation is dropped from every frame.
Why: tooling and operators that parse or render the trace lose the code context AWS guarantees; a payload compared against the real runtime's never matches.
Fix: the Python runtime's unhandled-error payload serializer (stackTrace frame formatting). The identical defect is already filed under a different service heading as 20261119T122442Z-appconfig-lambda-the-lambda-unhandled-error-payload-appconfig.md; that issue does not carry this heading's check.
Traces: lambda.python-custom-exception-class-names-error-type-and-frames.

Cause 4 of 8 — initialization-phase Python errors carry an EMPTY stackTrace.
AWS: an exception raised at module import time is reported with the raised class as errorType and a stackTrace naming the import frames; a SyntaxError in the handler module is reported with a stackTrace whose single frame is the '  File "<file>" Line <n>\n        <the offending source text>\n' form the CPython syntax reporter produces. The frame's file, line number and source text come from the uploaded package.
Emulator: stackTrace is the empty list `[]` in both cases, so the consumer's indexing of the last frame fails outright.
Why: a function that fails to initialise reports no location at all, so the cause of an init failure is undiagnosable from the Invoke response.
Fix: the Python runtime's init-phase error capture and the same error payload serializer as Cause 3.
Traces: lambda.python-module-level-exception-rejected-with-its-own-error-type, lambda.python-syntax-error-in-handler-module-rejected-at-invoke.

Cause 5 of 8 — a handler inside a package cannot be imported.
AWS: a Handler string naming a module inside a package (the 'pkg/sub/mod.handler' form) is imported as a submodule of its package, so relative imports inside that module resolve against the package and __file__-relative data files load; the invoke is a clean 200 with no FunctionError.
Emulator: Invoke answers 200 with FunctionError 'Unhandled' and the body {"errorMessage": "Unable to import module '<handler module path>': attempted relative import with no known parent package", "errorType": "Runtime.ImportModuleError", "requestId": "", "stackTrace": []} — the module is loaded as a top-level module with no package context.
Why: any function whose handler is organised into a package (the normal layout for anything beyond a single file) fails on every invocation.
Fix: the Python runtime's handler-string resolution and module import (import as a package submodule, setting __package__).
Traces: lambda.python-handler-in-nested-package-resolves-relative-imports-and-files.

Cause 6 of 8 — the invalid-ClientContext refusal message is not AWS's.
AWS: Invoke with a ClientContext (Smithy: Invoke.ClientContext, httpHeader X-Amz-Client-Context) that is not valid base64, OR whose base64 decodes to something that is not a JSON object, is refused with InvalidRequestContentException (Smithy: errorKind client, httpError 400) and the FIXED text 'Client context must be a valid Base64-encoded JSON object.' — one message for both classes, with no variable part — and the handler is not run.
Emulator: the code is correct (InvalidRequestContentException/400) but the message is 'ClientContext is not valid base64-encoded data.'
Why: a client that matches on the documented message does not recognise the refusal, and the emulator's wording also mis-describes the decodes-to-non-JSON case as a base64 problem.
Fix: the Invoke handler's ClientContext validation message.
Traces: lambda.python-invalid-client-context-rejected-before-the-handler-runs.

Cause 7 of 8 — the trailing-data unmarshal message reports a point instead of a span.
AWS: a payload with bytes trailing a complete JSON document fails in the Python runtime with errorMessage 'Unable to unmarshal input: Extra data: line <l> column <c> - line <l2> column <c2> (char <n> - <n2>)'. Fixed text: 'Unable to unmarshal input: Extra data: ' plus the ' - ' separated START and END position of the extra data; the line/column/char numbers are values derived from the request payload.
Emulator: 'Unable to unmarshal input: Extra data: line <l> column <c> (char <n>)' — only the start position, with no end position and no ' - ' span.
Why: the message differs from AWS's wording for this error class, so a consumer parsing the span cannot read the end of the offending region.
Fix: the Python runtime's payload-unmarshal error message formatting.
Traces: lambda.python-trailing-data-after-json-fails-runtime-unmarshal.

Cause 8 of 8 — an invalid-UTF-8 payload is reported as a replacement character rather than as an encoding error.
AWS: Invoke with a request body containing an invalid UTF-8 byte is refused before the handler runs with errorMessage 'Could not parse request body into json: Could not parse payload into json: Invalid UTF-8 start byte 0x<byte>\n at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: 1, column: <col>]'. Fixed text: everything except the hex byte, which is the offending byte from the request, and <col>, the 1-based byte column of that byte in the request.
Emulator: 'Could not parse request body into json: Could not parse payload into json: Unexpected character (<U+FFFD> (code 65533)): invalid unicode code point\n at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: 1, column: <col-1>]' — the body has already been lossily decoded to U+FFFD before validation, so the offending byte is lost, the error class is reported as an invalid code point instead of an invalid start byte, and the column is off by one.
Why: a client cannot distinguish a transport/encoding fault from a structural JSON fault, and the byte that caused it is not named.
Fix: Invoke's request-body validation — validate UTF-8 at the byte level before decoding, and produce the invalid-start-byte message.
Traces: lambda.python-unparseable-payload-rejected-before-the-handler-runs.

## Evidence
Cause 1: AssertionError: the handler must have been able to exec 'sim2real-layer-tool' from the attached layer; it reported {'ran': False, 'error_type': 'OSError', 'path': '/usr/local/bin:/usr/bin/:/bin:/opt/bin', 'resolved': '/opt/bin/sim2real-layer-tool'} — resolved on PATH, yet exec fails.

Cause 2: AssertionError: the one-layer control must extract the full 146800640-byte file; got {'size': 8388608}.

Cause 3: AssertionError: stackTrace ['  File "index.py", line 12, in handler\n', '  File "index.py", line 7, in _validate\n'], expected ['  File "index.py", line 12, in handler\n    qty = _validate(event)\n', '  File "index.py", line 7, in _validate\n    raise OrderRejected(f"order {order[\'id\']} rejected: qty {order[\'qty\']}")\n'].

Cause 4: module-level exception — IndexError: list index out of range while reading body["stackTrace"][-1] (stackTrace was []). Syntax error — AssertionError: attempt 1: stackTrace [], expected ['  File "index.py" Line 2\n        return {\'ok\': True\n'].

Cause 5: AssertionError: expected a clean 200, got status=200 FunctionError='Unhandled' body=b'{"errorMessage": "Unable to import module \'pkg/sub/mod\': attempted relative import with no known parent package", "errorType": "Runtime.ImportModuleError", "requestId": "", "stackTrace": []}'.

Cause 6: AssertionError: not base64: message 'ClientContext is not valid base64-encoded data.', expected 'Client context must be a valid Base64-encoded JSON object.' (error code InvalidRequestContentException matched).

Cause 7: AssertionError: payload b'{"a": 1} x': errorMessage 'Unable to unmarshal input: Extra data: line 1 column 10 (char 9)', expected 'Unable to unmarshal input: Extra data: line 1 column 10 - line 1 column 11 (char 9 - 10)'.

Cause 8: AssertionError: payload b'{"u": "\xff"}': message "Could not parse request body into json: Could not parse payload into json: Unexpected character ('\ufffd' (code 65533)): invalid unicode code point\n at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: 1, column: 8]", expected 'Could not parse request body into json: Could not parse payload into json: Invalid UTF-8 start byte 0xff\n at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: 1, column: 9]'.

## Steps to Reproduce
Cause 1: PublishLayerVersion a layer whose zip contains bin/<tool> (executable); CreateFunction with that layer; from the handler call shutil.which('<tool>') (resolves to /opt/bin/<tool>) and then subprocess.run(['<tool>']) — the exec raises OSError.

Cause 2: PublishLayerVersion a layer containing a single ~140 MiB file; attach it to a function and have the handler os.path.getsize() that file at /opt — it reports 8388608 instead of the published size.

Cause 3: Invoke a python3.x function whose handler raises a custom exception from a nested call; read stackTrace in the error payload — each frame lacks its indented source line.

Cause 4: (a) Invoke a function whose handler module raises at import time (module-level `raise LookupError(...)`); (b) Invoke a function whose handler module has a SyntaxError. In both, stackTrace is [].

Cause 5: Package a handler as pkg/sub/mod.py using a relative import (`from . import helper`), set Handler to 'pkg/sub/mod.handler', and Invoke — Runtime.ImportModuleError 'attempted relative import with no known parent package'.

Cause 6: Invoke with ClientContext='not base64!!' and again with base64('not json'); read Error.Code and Error.Message.

Cause 7: Invoke a python3.x function with Payload=b'{"a": 1} x'; read errorMessage.

Cause 8: Invoke a python3.x function with Payload=b'{"u": "\xff"}'; read Error.Message.
