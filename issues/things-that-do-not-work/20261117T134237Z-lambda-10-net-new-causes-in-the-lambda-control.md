Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.allocation-beyond-memory-size-is-killed-out-of-memory-rejected lambda.out-of-memory-kill-discards-the-execution-environment lambda.published-version-keeps-its-own-memory-limit-rejected-past-it lambda.nodejs-webassembly-module-instantiates-and-computes lambda.nodejs-webassembly-bad-magic-number-rejected lambda.nodejs-esm-createrequire-resolves-node-builtins lambda.nodejs-esm-createrequire-missing-local-module-rejected lambda.add-layer-version-permission-organization-id-requires-wildcard-principal-rejected lambda.durable-execution-kms-enforcement lambda.durable-execution-kms-enforcement-rejected lambda.durable-execution-kms-errors lambda.image-config-working-directory-length-limit-rejected lambda.list-functions-unissued-marker-rejected lambda.publish-version-description-tracked-per-version-through-list-and-qualified-read lambda.update-function-code-in-flight-revisionid-precondition
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.allocation-beyond-memory-size-is-killed-out-of-memory-rejected|_assert_out_of_memory#8|AssertionError|assert "Error Type: Runtime.OutOfMemory" in report.split("\t"), (
Issue-Key: lambda.out-of-memory-kill-discards-the-execution-environment|_assert_out_of_memory#9|AssertionError|assert "Error Type: Runtime.OutOfMemory" in report.split("\t"), (
Issue-Key: lambda.published-version-keeps-its-own-memory-limit-rejected-past-it|_assert_out_of_memory#12|AssertionError|assert "Error Type: Runtime.OutOfMemory" in report.split("\t"), (
Issue-Key: lambda.nodejs-webassembly-module-instantiates-and-computes|_invoke_ok#2|AssertionError|assert err is None, f"the function failed: {err!r} {out!r}"
Issue-Key: lambda.nodejs-webassembly-bad-magic-number-rejected|run#5|AssertionError|assert payload["errorType"] == "CompileError", (
Issue-Key: lambda.nodejs-esm-createrequire-resolves-node-builtins|_ok#3|AssertionError|assert err is None, f"Invoke {name}: FunctionError={err!r}, payload {raw[:600]!r}"
Issue-Key: lambda.nodejs-esm-createrequire-missing-local-module-rejected|_ok#5|AssertionError|assert err is None, f"Invoke {name}: FunctionError={err!r}, payload {raw[:600]!r}"
Issue-Key: lambda.add-layer-version-permission-organization-id-requires-wildcard-principal-rejected|run#12|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: lambda.durable-execution-kms-enforcement|_client_error#14|AssertionError|raise AssertionError(f"expected a ClientError but the call succeeded and returned {result!r}")
Issue-Key: lambda.durable-execution-kms-enforcement-rejected|_client_error#21|AssertionError|raise AssertionError(f"expected a ClientError but the call succeeded and returned {result!r}")
Issue-Key: lambda.durable-execution-kms-errors|run#30|AssertionError|assert state_code in kms_codes + ("InvalidParameterValueException", "ResourceConflictException"), (
Issue-Key: lambda.image-config-working-directory-length-limit-rejected|expect_error#9|AssertionError|assert got_message == message, f"unexpected message {got_message!r}"
Issue-Key: lambda.list-functions-unissued-marker-rejected|_refusal#14|AssertionError|raise AssertionError(f"expected the call to be refused, but it returned {result!r}")
Issue-Key: lambda.publish-version-description-tracked-per-version-through-list-and-qualified-read|run#24|AssertionError|assert again["Version"] == "2", (
Issue-Key: lambda.update-function-code-in-flight-revisionid-precondition|run#10|AssertionError|assert settled not in (before, in_flight), (
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# 10 net-new causes in the lambda control plane and execution environment: no out-of-memory REPORT, missing Node.js WebAssembly and node:module, four refusals that never happen or use the wrong wording, durable-execution KMS enforcement, and GetDurableExecutionState NotImplemented

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Ten distinct root causes in the lambda service. Each is numbered with its own failing check ids; all ten must be closed.

Cause 1 of 10: an invocation killed for exceeding its memory limit does not record the kill on the REPORT line.
AWS: recorded behavior only (the log text is not in the Smithy model). When a handler allocates past the function's `MemorySize`, the sandbox kills the execution environment and the invocation's REPORT line carries an extra tab-separated field whose fixed text is `Error Type: Runtime.OutOfMemory`, alongside `Duration`, `Billed Duration`, `Memory Size` and `Max Memory Used` (whose values vary per run). The invoke itself reports a function error.
Emulator: the REPORT line carries only Duration / Billed Duration / Memory Size / Max Memory Used — no `Error Type: Runtime.OutOfMemory` field at all, so an allocation far past the limit is not reported as an out-of-memory kill (and the environment is not shown to be discarded).
Why: out-of-memory is the most common Lambda production failure, and the REPORT line is where it is diagnosed. A client cannot tell an OOM kill from a normal completion, and a published version's own lower memory limit cannot be shown to be enforced.
Fix: the lambda invoke/execution-environment memory enforcement and the REPORT-line emitter in the log pipeline.
Checks: lambda.allocation-beyond-memory-size-is-killed-out-of-memory-rejected, lambda.out-of-memory-kill-discards-the-execution-environment, lambda.published-version-keeps-its-own-memory-limit-rejected-past-it

Cause 2 of 10: the Node.js managed runtime has no `WebAssembly` global.
AWS: recorded behavior only. The Node.js managed runtimes are stock Node, where the `WebAssembly` global exists: a valid module instantiates and its exported functions compute, and a buffer with a bad magic number is rejected with a `CompileError`.
Emulator: both invokes fail with errorType `ReferenceError` and message `WebAssembly is not defined` — so the global is absent and the documented `CompileError` for a malformed module can never be produced.
Why: any handler that loads a WASM module fails at the first reference, and a caller cannot distinguish "this runtime does not have WebAssembly" from "your module is bad".
Fix: the Node.js managed-runtime provisioning for the lambda service — the interpreter must expose the `WebAssembly` global.
Checks: lambda.nodejs-webassembly-module-instantiates-and-computes, lambda.nodejs-webassembly-bad-magic-number-rejected

Cause 3 of 10: the Node.js managed runtime cannot resolve the `node:module` builtin.
AWS: recorded behavior only. `node:module` is a core builtin in every supported Node.js runtime; an ESM handler can `import { createRequire } from 'node:module'`, and the require it builds resolves node builtins, while a missing LOCAL module through that require fails with the module-not-found error for THAT module.
Emulator: both invokes fail at import with errorType `Runtime.ImportModuleError` and message `Error: Cannot find module 'node:module'` — the builtin itself is unresolvable, so the handler never runs and the negative case reports the wrong missing module.
Why: `createRequire` is the standard bridge for ESM handlers that must use CommonJS dependencies. Every such handler fails to initialise, and the error names a core builtin rather than the user's own module, sending the operator after the wrong thing.
Fix: the Node.js managed-runtime module resolver in the lambda execution environment — the `node:`-prefixed builtin namespace must resolve.
Checks: lambda.nodejs-esm-createrequire-resolves-node-builtins, lambda.nodejs-esm-createrequire-missing-local-module-rejected

Cause 4 of 10: AddLayerVersionPermission's organization-id refusal message is not AWS's wording.
AWS: when `OrganizationId` is supplied, `Principal` must be `*`; a different principal is refused InvalidParameterValueException (400) with the fixed, parameter-free text `If organization id is specified then principal should be *`. (Smithy: lambda AddLayerVersionPermission input members `OrganizationId` (pattern `^o-[a-z0-9]{10,32}$`) and `Principal` (pattern `^\d{12}|\*|arn:...:root$`); error InvalidParameterValueException, httpError 400.)
Emulator: the message is reworded and interpolates the submitted principal into text AWS keeps fixed.
Why: the message is part of the contract. A client matching AWS's wording does not match, and a caller is shown a sentence AWS never emits.
Fix: the lambda AddLayerVersionPermission handler's validation message.
Checks: lambda.add-layer-version-permission-organization-id-requires-wildcard-principal-rejected

Cause 5 of 10: a StopDurableExecution on an execution whose KMS key cannot be used succeeds instead of being refused with a KMS-family error.
AWS: the durable-execution operations are modelled with the KMS error family precisely because they must surface a key failure to the caller: StopDurableExecution declares `KMSAccessDeniedException`, `KMSDisabledException`, `KMSInvalidStateException` and `KMSNotFoundException` (all httpError 502) alongside InvalidParameterValueException (400). When the configured customer-managed key is unusable, the call is refused with the corresponding KMS error, not completed. (Smithy: lambda StopDurableExecution errors.)
Emulator: both checks observe a 200 carrying only `StopTimestamp` — the call succeeded and no KMS enforcement happened at all.
Why: durable-execution state is encrypted with the configured key; completing the operation anyway means state was handled without the key the client demanded. A caller cannot detect a revoked, disabled or deleted key, and silently loses the encryption guarantee it configured.
Fix: the durable-execution operation handlers in the lambda service — the KMS key-usability check on the execution's `DurableConfig` key, and the mapping of a key failure onto the modelled KMS* errors (502).
Checks: lambda.durable-execution-kms-enforcement, lambda.durable-execution-kms-enforcement-rejected

Cause 6 of 10: GetDurableExecutionState answers an unmodelled `NotImplemented` error.
AWS: GetDurableExecutionState's modelled errors are InvalidParameterValueException (400), the four KMS* errors (502), ServiceException (500) and TooManyRequestsException (429); `NotImplemented` is not among them and is not an error code the lambda service emits. On a KMS-unusable execution it answers one of the KMS* errors; on a bad checkpoint token or ARN, InvalidParameterValueException. (Smithy: lambda GetDurableExecutionState errors.)
Emulator: the call answers error code `NotImplemented`.
Why: an unmodelled error code is not in any SDK's error shapes, so a client cannot catch it by type and every structured handler falls through to a generic failure. The operation is effectively unusable, and its KMS/validation refusals are unobservable.
Fix: the lambda GetDurableExecutionState handler — it must answer the modelled error family (or the state it is asked for) rather than NotImplemented.
Checks: lambda.durable-execution-kms-errors

Cause 7 of 10: CreateFunction validates a container image's existence before it validates `ImageConfig.WorkingDirectory`'s length.
AWS: input validation precedes resource lookup, so a `WorkingDirectory` over its length limit is refused with the working-directory-too-long validation message regardless of whether the image exists; only once the input is valid does the image source get resolved. (Smithy: lambda CreateFunction input member `ImageConfig` with member WorkingDirectory; error InvalidParameterValueException, httpError 400 — recorded behavior only for the ordering.)
Emulator: the refusal is the source-image-does-not-exist message — the image-existence check fired first, so the length constraint is never reported.
Why: a client gets told its image is missing when the real problem is an over-long working directory, and the member actually at fault is never named. The at-limit/over-limit boundary cannot be tested at all.
Fix: the lambda CreateFunction handler's validation ordering — `ImageConfig` member constraints must be enforced before the image source is resolved.
Checks: lambda.image-config-working-directory-length-limit-rejected

Cause 8 of 10: ListFunctions accepts a Marker the service never issued.
AWS: `Marker` is the pagination token ListFunctions itself minted in a previous `NextMarker`; a token the service never issued is refused InvalidParameterValueException at 400, not treated as the start of the list. (Smithy: lambda ListFunctions input member `Marker`, paginated inputToken `Marker` / outputToken `NextMarker`; error InvalidParameterValueException, httpError 400.)
Emulator: the call answers 200 with the first page of functions and a fresh `NextMarker`, i.e. the unissued marker is silently ignored.
Why: a client that corrupts, truncates or reuses a token across accounts or regions gets a plausible first page instead of a refusal, so it silently re-reads from the beginning and can loop or double-process without ever learning the token was invalid.
Fix: the lambda ListFunctions handler's pagination-token validation — an unissued/undecodable Marker must be refused InvalidParameterValueException (400).
Checks: lambda.list-functions-unissued-marker-rejected

Cause 9 of 10: PublishVersion mints a new version when nothing has changed since the last publish.
AWS: Lambda does not publish a new version when the function's code and configuration are unchanged since the most recent version — the call returns that existing version, so repeating PublishVersion with no intervening update answers the same `Version` value rather than incrementing it. (Smithy: lambda PublishVersion output member `Version` (pattern `^(\$LATEST|[0-9]+)$`); the no-change rule is documented behavior — recorded behavior only.)
Emulator: a second PublishVersion with nothing changed returns the next ordinal, so every call mints a version.
Why: callers publish defensively on every deploy. Minting a version per call inflates the version space, breaks version-count and `ListVersionsByFunction` expectations, consumes code storage, and makes an alias's "current version" move when no code changed.
Fix: the lambda PublishVersion handler — compare the function's code hash and configuration against the latest published version and return that version when they are identical.
Checks: lambda.publish-version-description-tracked-per-version-through-list-and-qualified-read

Cause 10 of 10: UpdateFunctionCode does not mint a new RevisionId when the update settles.
AWS: `RevisionId` changes on every mutation. UpdateFunctionCode's immediate response carries the in-flight revision, and when the update settles the function's configuration carries a NEW revision id again — so the settled revision differs from both the pre-update one and the one in the update response, and a `RevisionId` precondition taken from the in-flight response is stale by the time the update completes. (Smithy: lambda UpdateFunctionConfiguration/PublishVersion input member `RevisionId` with error PreconditionFailedException, httpError 412; output member `RevisionId` — recorded behavior only for the re-mint on settle.)
Emulator: the settled revision equals the one returned by the update response, so no new revision is minted when the update settles.
Why: RevisionId is Lambda's optimistic-concurrency token. If it does not advance on settle, two concurrent updaters can both pass a precondition that should have failed one of them, and a client cannot detect that the function moved under it.
Fix: the lambda UpdateFunctionCode handler and the function state's revision bookkeeping — mint a fresh RevisionId when the pending update is applied, not only when it is accepted.
Checks: lambda.update-function-code-in-flight-revisionid-precondition

## Evidence
Cause 1 — actual, verbatim (three checks, same root cause):
  AssertionError: 400 MB at 128 MB: the REPORT line does not record the out-of-memory kill: 'REPORT RequestId: <id>\tDuration: 1628.59 ms\tBilled Duration: 1629 ms\tMemory Size: 128 MB\tMax Memory Used: 128 MB'
  AssertionError: version 1, 400 MB: the REPORT line does not record the out-of-memory kill: 'REPORT RequestId: <id>\tDuration: 1642.80 ms\tBilled Duration: 1643 ms\tMemory Size: 128 MB\tMax Memory Used: 128 MB'
Expected a tab-separated field `Error Type: Runtime.OutOfMemory` on that line (durations/ids are per-run).
Cause 2 — actual, verbatim:
  AssertionError: the function failed: 'Unhandled' {'errorMessage': 'WebAssembly is not defined', 'errorType': 'ReferenceError', ...}
  AssertionError: expected errorType 'CompileError', got 'ReferenceError': {'errorMessage': 'WebAssembly is not defined', 'errorType': 'ReferenceError', ...}
Cause 3 — actual, verbatim (both checks):
  AssertionError: Invoke <fn>: FunctionError='Unhandled', payload b'{"errorMessage":"Error: Cannot find module \'node:module\'","errorType":"Runtime.ImportModuleError",...}'
Cause 4 — actual, verbatim:
  AssertionError: expected message 'If organization id is specified then principal should be *', got "The principal must be '*' when an organization id is specified, not '000000000000'."
(the account id is the submitted Principal, a per-run/request value; the expected text is wholly fixed).
Cause 5 — actual, verbatim (both checks):
  AssertionError: expected a ClientError but the call succeeded and returned {'ResponseMetadata': {..., 'HTTPStatusCode': 200, ...}, 'StopTimestamp': datetime.datetime(...)}
Expected one of KMSAccessDeniedException / KMSDisabledException / KMSInvalidStateException / KMSNotFoundException at HTTP 502.
Cause 6 — actual, verbatim:
  AssertionError: GetDurableExecutionState: expected a KMS-family/validation/conflict error, got NotImplemented
Cause 7 — actual, verbatim:
  AssertionError: unexpected message 'Source image 000000000000.dkr.ecr.us-west-2.amazonaws.com/sim2real-nonexistent@sha256:0000...0000 does not exist. Provide a valid source image.'
Expected the working-directory-too-long validation message for `ImageConfig.WorkingDirectory` of 999/1000 characters.
Cause 8 — actual, verbatim:
  AssertionError: expected the call to be refused, but it returned {'ResponseMetadata': {..., 'HTTPStatusCode': 200, ...}, 'NextMarker': 'sim2real-lfm-<id>-0', 'Functions': [ ... ]}
Expected InvalidParameterValueException at 400.
Cause 9 — actual, verbatim:
  AssertionError: publishing with nothing changed since version 2 should return the existing version '2', got '3'
Cause 10 — actual, verbatim:
  AssertionError: expected the settled RevisionId to differ from both the pre-update and the in-flight response one, before='55633f00-...' response='618bcf7e-...' settled='618bcf7e-...'
(the uuids are per-run; the divergence is settled == response).

## Steps to Reproduce
Cause 1: CreateFunction with MemorySize 128; Invoke a handler that allocates ~400 MB; read the tail log. Observed a REPORT line with no `Error Type: Runtime.OutOfMemory` field; expected that field present and a function error.
Cause 2: Invoke a nodejs handler that calls `WebAssembly.instantiate` on a valid module, and on a buffer with a bad magic number. Observed ReferenceError 'WebAssembly is not defined' for both; expected the module to compute, and CompileError for the bad magic number.
Cause 3: Invoke an ESM nodejs handler that does `import { createRequire } from 'node:module'`. Observed Runtime.ImportModuleError "Cannot find module 'node:module'"; expected the builtin to resolve (and a missing LOCAL module through the created require to report that module).
Cause 4: PublishLayerVersion, then AddLayerVersionPermission with `OrganizationId: o-xxxxxxxxxx` and `Principal: <12-digit account>`. Observed message "The principal must be '*' when an organization id is specified, not '<account>'."; expected `If organization id is specified then principal should be *`.
Cause 5: Start a durable execution on a function whose DurableConfig names a customer-managed key that is then made unusable; call StopDurableExecution. Observed 200 with StopTimestamp; expected one of KMSAccessDeniedException/KMSDisabledException/KMSInvalidStateException/KMSNotFoundException (502).
Cause 6: Call GetDurableExecutionState for the same execution. Observed error code `NotImplemented`; expected a KMS*/InvalidParameterValueException/ResourceConflictException error.
Cause 7: CreateFunction with PackageType Image, an ImageUri that does not exist, and `ImageConfig.WorkingDirectory` of 999 then 1000 characters. Observed the 'Source image ... does not exist' refusal; expected the working-directory length validation refusal.
Cause 8: ListFunctions with `MaxItems=1, Marker=<token the service never issued>`. Observed 200 with a page of functions; expected InvalidParameterValueException (400).
Cause 9: CreateFunction, PublishVersion (gets 1), update code, PublishVersion (gets 2), then PublishVersion again with nothing changed. Observed Version '3'; expected '2'.
Cause 10: Read a function's RevisionId, UpdateFunctionCode (note the response RevisionId), wait for the update to settle and read the RevisionId again. Observed settled == response revision; expected it to differ from both the pre-update and the in-flight response revision.
