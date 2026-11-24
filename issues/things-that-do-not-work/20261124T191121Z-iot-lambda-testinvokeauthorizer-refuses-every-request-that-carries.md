Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot__lambda.authorizer-signed-token-either-key-passthrough iot__lambda.authorizer-test-invoke-signature-mismatch-rejected
Issue-Key: iot__lambda|TestInvokeAuthorizer|InvalidRequestException
Issue-Key: iot__lambda.authorizer-test-invoke-signature-mismatch-rejected|run#22|AssertionError|assert err == expected, (
Issue-Commit: 05c7972ccec5de40063e90508b79406e82288e9e

# iot__lambda: TestInvokeAuthorizer refuses every request that carries no httpContext/mqttContext/tlsContext, so a token+tokenSignature test of a signing-enabled custom authorizer never runs

> _Note: this issue was identified against the code base at commit `05c7972ccec5de40063e90508b79406e82288e9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TestInvokeAuthorizer

## Description
One cause (Cause 1 of 1): TestInvokeAuthorizer treats the three optional protocol contexts as a required one-of and rejects the request before resolving the token signature or invoking the authorizer's Lambda.

AWS: Smithy (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json) models TestInvokeAuthorizer POST /authorizer/{authorizerName}/test with exactly ONE required member, `authorizerName` (@length 1..128, pattern ^[\w=,@-]+$). `token` (@length 1..6144), `tokenSignature` (@length 1..2560, pattern ^[A-Za-z0-9+/]+={0,2}$), `httpContext`, `mqttContext` and `tlsContext` are ALL optional, and the model declares no one-of/required-union constraint over the three contexts. Recorded behavior: AWS accepts `authorizerName` + `token` + `tokenSignature` with no protocol context, verifies the signature against the authorizer's registered tokenSigningPublicKeys, invokes the authorizer function and returns its verdict members unchanged (isAuthenticated, principalId, policyDocuments, refreshAfterInSeconds, disconnectAfterInSeconds); a signature that verifies against none of the registered keys is refused with HTTP 400 InvalidRequestException whose message is the fixed text `Token signature mismatch for authorizer ` followed by the request's `authorizerName` value (the only variable part of that message).

Emulator: the handler short-circuits on the three contexts before anything else. Any request with httpContext, mqttContext and tlsContext all absent is answered HTTP 400 InvalidRequestException with the message `Request must have Http/Mqtt/Tls context.` — regardless of token/tokenSignature, of whether signing is enabled, and of whether the signature verifies. The signature check and the Lambda invocation that follow in the handler are therefore unreachable on this (documented, modeled) call shape.

Why: a client testing a signing-enabled custom authorizer the documented way — pass the token and its base64 signature, read back the authorizer function's verdict — gets a hard 400 where AWS returns a verdict, so principalId/policyDocuments/TTL passthrough can never be observed. Worse, the blanket refusal MASKS the signature verdict: a forged signature that AWS refuses as `Token signature mismatch for authorizer <authorizerName>` is refused with the same code but the wrong message, so a client cannot tell "bad signature" from "malformed request", and a test suite asserting the mismatch message sees a false reason for a correct rejection.

Fix: the request validation in this operation's handler — the guard that returns InvalidRequestException when all three contexts are absent must be removed/narrowed so a context-less request proceeds to the signature verification and the authorizer invocation already implemented below it; the no-context case is not a modeled refusal. Signature-mismatch refusals must keep the `Token signature mismatch for authorizer <authorizerName>` wording.

Traces: iot__lambda.authorizer-signed-token-either-key-passthrough; iot__lambda.authorizer-test-invoke-signature-mismatch-rejected. Code citations: services/service-com-amazonaws-iot-awsiotservice/src/ops/test_invoke_authorizer.rs lines 208-223 (the all-contexts-absent guard, message from crate::state::MISSING_AUTHORIZATION_CONTEXT_MESSAGE), with the signature check at lines 231-261 and the invocation at lines 263-300 sitting behind it.

## Evidence
Expected (AWS):
- test_invoke_authorizer(authorizerName=<signing-enabled authorizer>, token=<t>, tokenSignature=<base64 RSA/SHA256 sig by a registered key>) -> HTTP 200 with the authorizer Lambda's verdict passed through: isAuthenticated=True, principalId unchanged, policyDocuments unchanged, refreshAfterInSeconds/disconnectAfterInSeconds unchanged.
- same call with a signature made by an UNREGISTERED key -> HTTP 400, code InvalidRequestException, message `Token signature mismatch for authorizer <authorizerName>` (fixed text + the request's authorizerName).

Actual (emulator), both calls:
- HTTP 400, code InvalidRequestException, message `Request must have Http/Mqtt/Tls context.`

Replay output:
- iot__lambda.authorizer-signed-token-either-key-passthrough — botocore.errorfactory.InvalidRequestException: An error occurred (InvalidRequestException) when calling the TestInvokeAuthorizer operation: Request must have Http/Mqtt/Tls context.
- iot__lambda.authorizer-test-invoke-signature-mismatch-rejected — AssertionError: expected ('InvalidRequestException', 'Token signature mismatch for authorizer <authorizerName>') for an unverifiable signature, got ('InvalidRequestException', 'Request must have Http/Mqtt/Tls context.')

## Steps to Reproduce
1. Create a Lambda function returning {isAuthenticated: true, principalId: <alnum>, policyDocuments: [<policy json>], refreshAfterInSeconds: 300, disconnectAfterInSeconds: 86400} and allow iot.amazonaws.com to invoke it.
2. iot:CreateAuthorizer with status=ACTIVE, signingDisabled=false, tokenKeyName="token", tokenSigningPublicKeys={Key1: <RSA public key PEM>} (optionally a second key Key2).
3. Sign the token bytes with the registered private key (PKCS1v15 / SHA256) and base64-encode it.
4. Call iot:TestInvokeAuthorizer(authorizerName=..., token=..., tokenSignature=...) with NO httpContext, mqttContext or tlsContext.
5. Observe 400 InvalidRequestException "Request must have Http/Mqtt/Tls context." instead of a 200 verdict from the Lambda.
6. Repeat step 4 with a signature made by a key that was never registered: observe the same message instead of "Token signature mismatch for authorizer <authorizerName>".
