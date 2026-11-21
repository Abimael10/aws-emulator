Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda.get-function-code-signing-config-lifecycle iam__lambda.nodejs24-webcrypto-hmac-sha256-and-aesgcm-round-trip iam__lambda.nodejs24-webcrypto-aesgcm-decrypt-rejects-tampered-ciphertext iam__lambda.update-function-configuration-dead-letter-target-arn-pattern-rejected
Issue-Key: iam__lambda.get-function-code-signing-config-lifecycle|run#6|AssertionError|assert "FunctionName" not in before, (
Issue-Key: iam__lambda.nodejs24-webcrypto-hmac-sha256-and-aesgcm-round-trip|_invoke_ok#3|AssertionError|assert err is None, f"the function failed: {err!r} {out!r}"
Issue-Key: iam__lambda.nodejs24-webcrypto-aesgcm-decrypt-rejects-tampered-ciphertext|_invoke_ok#3|AssertionError|assert err is None, f"the function failed: {err!r} {out!r}"
Issue-Key: iam__lambda.update-function-configuration-dead-letter-target-arn-pattern-rejected|run#20|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code}: {message!r}"
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# 3 causes: GetFunctionCodeSigningConfig answers 200 with a FunctionName when no config is attached, the Node.js 24 WebCrypto SubtleCrypto surface is missing, and a malformed DeadLetterConfig.TargetArn is refused ResourceConflictException instead of a validation error

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFunctionCodeSigningConfig

## Description
Three distinct root causes.

Cause 1 of 3: GetFunctionCodeSigningConfig on a function with no code-signing config attached answers 200 carrying a `FunctionName`.
AWS: both output members of GetFunctionCodeSigningConfig are required together — `CodeSigningConfigArn` and `FunctionName` describe an attachment that exists. For a function with no code-signing config attached, AWS answers 200 with neither member present, so a caller distinguishes "no config attached" from "this config attached" by the absence of the pair. (Smithy: lambda GetFunctionCodeSigningConfig output members `CodeSigningConfigArn` (required, pattern `^arn:...:code-signing-config:csc-[a-z0-9]{17}$`) and `FunctionName` (required).)
Emulator: the response is 200 with `FunctionName` set to the function's name and no `CodeSigningConfigArn` — a half-populated attachment that is neither the empty answer nor a valid pair.
Why: a client cannot tell whether a function is covered by code signing. A truthy `FunctionName` reads as "a config is attached" while the ARN it would need is missing, so enforcement checks either crash on the missing ARN or wrongly conclude signing is in force.
Fix: the lambda GetFunctionCodeSigningConfig handler and its serializer — when the function has no code-signing config attached, emit neither member.
Traces: iam__lambda.get-function-code-signing-config-lifecycle

Cause 2 of 3: the Node.js 24 managed runtime's WebCrypto (`crypto.subtle`) surface is absent or stubbed, so standard algorithms do not exist.
AWS: recorded behavior only (the runtime's language surface is not in the Smithy model). The nodejs24.x managed runtime is a stock Node.js 24, where the global `crypto.subtle` SubtleCrypto implements `importKey`, `sign`, `verify`, `encrypt` and `decrypt` for `HMAC`/`SHA-256` and `AES-GCM`; an HMAC-SHA256 digest matches the same key/message computed outside the function, an AES-GCM round trip returns the original plaintext, and decrypting a ciphertext whose bytes or tag were tampered with rejects with an `OperationError`.
Emulator: the handler dies before any of that. One invoke returns FunctionError `Unhandled` with errorType `TypeError` and message `not a function` raised at the first `crypto.subtle` call; the other returns errorType `NotSupportedError` with message `Unrecognized algorithm name` from `webCipherName (node:internal/crypto)`. So the SubtleCrypto methods are either missing or recognise no standard algorithm name.
Why: WebCrypto is the supported way to do crypto in a modern Node.js Lambda. Any handler that signs, verifies or encrypts with it fails at the first call, and the documented tamper-rejection behavior a security-sensitive handler relies on cannot be exercised at all.
Fix: the Node.js managed-runtime provisioning for the lambda service — the nodejs24.x runtime image/interpreter must expose a complete `crypto.subtle` with the standard algorithm registry, not a stub.
Traces: iam__lambda.nodejs24-webcrypto-hmac-sha256-and-aesgcm-round-trip, iam__lambda.nodejs24-webcrypto-aesgcm-decrypt-rejects-tampered-ciphertext

Cause 3 of 3: UpdateFunctionConfiguration with a `DeadLetterConfig.TargetArn` that violates the member's pattern is refused with ResourceConflictException (an update already in progress) instead of the validation error.
AWS: `DeadLetterConfig.TargetArn` is pattern-constrained, and a value that does not match is a client-side validation failure — AWS answers 400 with a validation error naming the member and the constraint, not 409 ResourceConflictException. ResourceConflictException is reserved for a genuine concurrent-update conflict and must not be produced for a request that could never be valid. (Smithy: lambda UpdateFunctionConfiguration input member `DeadLetterConfig`; errors include InvalidParameterValueException (400) and ResourceConflictException (409).)
Emulator: the call answers `ResourceConflictException` with the fixed text `The operation cannot be performed at this time. An update is in progress for resource: ` followed by the function ARN taken from the request/state. The malformed ARN is never validated.
Why: a client sees a retryable conflict where AWS reports a permanent validation error. Retry logic keyed on ResourceConflictException loops forever on a request that can never succeed, and the member and constraint that are actually wrong are never named.
Fix: the lambda UpdateFunctionConfiguration handler's input validation — member patterns (here DeadLetterConfig.TargetArn) must be enforced and reported as a validation error before the in-progress-update conflict check runs.
Traces: iam__lambda.update-function-configuration-dead-letter-target-arn-pattern-rejected

## Evidence
Cause 1 — actual, verbatim:
  AssertionError: expected no FunctionName before any config is attached, got {'ResponseMetadata': {..., 'HTTPStatusCode': 200, ...}, 'FunctionName': 'sim2real-behav-fn-<id>'}
The 200 response carries `FunctionName` (a per-run name) and no `CodeSigningConfigArn`; expected neither member.
Cause 2 — actual, verbatim:
  AssertionError: the function failed: 'Unhandled' {'errorMessage': 'Unrecognized algorithm name', 'errorType': 'NotSupportedError', 'trace': ['NotSupportedError: Unrecognized algorithm name', '    at webCipherName (node:internal/crypto:407:21)', ...]}
  AssertionError: the function failed: 'Unhandled' {'errorMessage': 'not a function', 'errorType': 'TypeError', 'trace': ['TypeError: not a function', '    at <anonymous> (/var/task/index.js:2:38)', ...]}
Expected: a successful invoke whose HMAC-SHA256 digest matches an externally computed one and whose AES-GCM round trip returns the plaintext, with a tampered ciphertext rejecting as OperationError.
Cause 3 — actual, verbatim:
  AssertionError: expected ValidationException, got ResourceConflictException: 'The operation cannot be performed at this time. An update is in progress for resource: arn:aws:lambda:us-west-2:000000000000:function:sim2real-fn-<id>'
The ARN in the message is a per-run value; the divergence is the error code (409 ResourceConflictException instead of a 400 validation error) and the message class.

## Steps to Reproduce
Cause 1: CreateFunction with no code-signing config, then lambda GetFunctionCodeSigningConfig on it. Observed 200 with `FunctionName` set and no `CodeSigningConfigArn`; expected 200 with neither member present.
Cause 2: CreateFunction with Runtime nodejs24.x and a handler that calls `crypto.subtle.importKey`/`sign` for HMAC-SHA256 and `encrypt`/`decrypt` for AES-GCM; Invoke it. Observed FunctionError 'Unhandled' with TypeError 'not a function' / NotSupportedError 'Unrecognized algorithm name'; expected the HMAC digest to match an externally computed one, the AES-GCM round trip to return the plaintext, and a tampered ciphertext to reject with OperationError.
Cause 3: lambda UpdateFunctionConfiguration on a settled function with `DeadLetterConfig: {TargetArn: '<value violating the member pattern>'}`. Observed ResourceConflictException 'The operation cannot be performed at this time. An update is in progress for resource: <function arn>'; expected a 400 validation error naming DeadLetterConfig.TargetArn and the pattern it failed.
