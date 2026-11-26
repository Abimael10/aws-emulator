Issue-Source: trace-sweep-divergence
Issue-Trace: cip-autherr-flow-not-enabled-user-srp
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cip-autherr-flow-not-enabled-user-srp cip-chal-srp-password-verifier-envelope
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|InitiateAuth|4xx->4xx|$.message
Issue-Key: cognito-idp|InitiateAuth|2xx->2xx|$.ChallengeParameters.SECRET_BLOCK
Issue-Key: cognito-idp|InitiateAuth|2xx->2xx|$.ChallengeParameters.SRP_B
Issue-Key: cognito-idp|RespondToAuthChallenge|4xx->4xx|$.message
Issue-Key: cognito-idp|RespondToAuthChallenge|4xx->4xx|$.__type
Issue-Key: cognito-idp|RespondToAuthChallenge|4xx->4xx|$headers.x-amzn-errortype
Issue-Related: 20261102T111936Z-cognito-idp-response-payloads-diverge-read-paths-omit.md
Issue-Commit: 47635055653444c8189ab0ed1a8047834cbe5b1c

# cognito-idp: the USER_SRP_AUTH PASSWORD_VERIFIER continuation diverges — a Session is minted and then demanded, and SRP refusals carry the wrong class and wording (4 causes)

> _Note: this issue was identified against the code base at commit `47635055653444c8189ab0ed1a8047834cbe5b1c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InitiateAuth

## Description
Four net-new causes in the emulator's SRP (USER_SRP_AUTH / PASSWORD_VERIFIER) sign-in path. Causes 2-4 share one root defect: the emulator models the PASSWORD_VERIFIER continuation as a Session-carrying challenge, so it mints a Session on InitiateAuth that AWS does not emit and then refuses every RespondToAuthChallenge that omits one, short-circuiting the real validation of the challenge responses. Cause 1 is the refusal wording on the same operation when the client has not enabled the flow.

Cause 1 of 4: InitiateAuth with AuthFlow=USER_SRP_AUTH against an app client that does not have that flow in ExplicitAuthFlows.
  AWS: Smithy: InitiateAuth.AuthFlow (AuthFlowType, @required) with errors InvalidParameterException (httpError 400, members message/reasonCode). The recorded response is HTTP 400 with message "USER_SRP_AUTH is not enabled for the client." The fixed text is "<flow> is not enabled for the client."; the only variable part is the flow name, taken from the request's AuthFlow member. Status and error code matched; only the message text diverged.
  Emulator: HTTP 400 with $.message = "USER_SRP_AUTH flow not enabled for this client" — paraphrased wording ("flow not enabled for this client" instead of "is not enabled for the client.", and no terminating period).
  Why: clients and SDK test suites that branch on, log, or surface the refusal text for a disabled auth flow see a string AWS never emits; an operator reading the emulator's message cannot match it to AWS documentation or to a recorded production trace.
  Fix: the InitiateAuth handler's auth-flow enablement check — the message template it builds for the InvalidParameterException it already raises with the correct status and code.
  Traces: cip-autherr-flow-not-enabled-user-srp#task#6.

Cause 2 of 4: InitiateAuth USER_SRP_AUTH on an enabled client returns a PASSWORD_VERIFIER challenge whose envelope is wrong.
  AWS: Smithy: InitiateAuth output members ChallengeName (ChallengeNameType, includes PASSWORD_VERIFIER), ChallengeParameters (ChallengeParametersType map), Session (SessionType, @length 20-4096) — Session is optional, and for the PASSWORD_VERIFIER challenge of USER_SRP_AUTH the recording shows NO Session member at all: the SRP continuation is carried by the challenge parameters (SECRET_BLOCK, SRP_B, SALT, USERNAME, USER_ID_FOR_SRP), not by a session token. The recording asserts SECRET_BLOCK and SRP_B by shape class, because their values are generated per handshake and are never themselves a requirement.
  Emulator: HTTP 200, but $.Session is present (extra; expected absent), and $.ChallengeParameters.SECRET_BLOCK and $.ChallengeParameters.SRP_B are both reported shape-mismatched against the shape class AWS emits for those parameters — the emulator's SECRET_BLOCK is a short base64 blob and its SRP_B a hex big-int that do not fall in AWS's shape class for these members.
  Why: an SDK or SRP client implementing the AWS handshake reads the continuation out of ChallengeParameters and sends back PASSWORD_CLAIM_SECRET_BLOCK/PASSWORD_CLAIM_SIGNATURE; it never has a Session to echo. It also derives the password claim from SECRET_BLOCK and SRP_B, so parameters outside AWS's shape class cannot be processed by a real SRP implementation. The handshake therefore cannot be completed against the emulator at all, which is exactly what causes 3 and 4 observe.
  Fix: the InitiateAuth handler's PASSWORD_VERIFIER challenge construction and the SRP challenge-parameter generation it serializes — omit Session for this challenge and emit SECRET_BLOCK/SRP_B in AWS's shape class.
  Traces: cip-chal-srp-password-verifier-envelope#task#6.

Cause 3 of 4: RespondToAuthChallenge(ChallengeName=PASSWORD_VERIFIER) with the challenge responses incomplete is refused for the wrong missing parameter.
  AWS: Smithy: RespondToAuthChallenge input ChallengeName (@required), Session (optional, @length 20-4096), ChallengeResponses (ChallengeResponsesType map); error InvalidParameterException (httpError 400). The recorded response is HTTP 400 InvalidParameterException with message "Missing required parameter PASSWORD_CLAIM_SIGNATURE" — fixed text "Missing required parameter " plus the name of the ChallengeResponses entry the PASSWORD_VERIFIER challenge requires and the request omitted. Session is NOT one of the parameters AWS requires for this challenge (see cause 2).
  Emulator: HTTP 400 with $.message = "Missing required parameter Session" — it validates Session, a member AWS leaves optional here, before (and instead of) the challenge responses it should be checking.
  Why: a client that correctly built an SRP response and simply mis-filled one challenge response is told to supply a parameter AWS never asks for, and is never told which claim is actually missing. Error-path tests that assert on the missing-parameter name fail, and the real validation of ChallengeResponses is never exercised.
  Fix: RespondToAuthChallenge's input validation for the PASSWORD_VERIFIER challenge — stop requiring Session and validate the challenge-response keys that challenge defines.
  Traces: cip-chal-srp-password-verifier-envelope#task#7.

Cause 4 of 4: RespondToAuthChallenge(PASSWORD_VERIFIER) with a bad password claim is misclassified instead of being refused as a failed sign-in.
  AWS: Smithy: RespondToAuthChallenge errors include NotAuthorizedException ("This exception is thrown when a user isn't authorized") alongside InvalidParameterException. The recording shows HTTP 400 with __type "NotAuthorizedException", message "Incorrect username or password." and the x-amzn-errortype header naming NotAuthorizedException. That message is entirely fixed text — deliberately, so a wrong password is not distinguishable from an unknown user.
  Emulator: HTTP 400 (status matches) but __type "InvalidParameterException", $.message "Missing required parameter Session" and x-amzn-errortype "InvalidParameterException:" — the same Session precondition from cause 3 fires, so the password claim is never evaluated and the failure is reported as a malformed request rather than a rejected credential.
  Why: a client cannot tell a wrong password from a malformed request: retry/lockout logic keyed on NotAuthorizedException never triggers, and credential-failure handling is never reached. It also leaks a different failure shape for an authentication failure than AWS's deliberately opaque one.
  Fix: RespondToAuthChallenge's PASSWORD_VERIFIER handler — evaluate the password claim and raise NotAuthorizedException with AWS's fixed "Incorrect username or password." text (and the matching x-amzn-errortype) once the Session precondition from cause 3 is removed.
  Traces: cip-chal-srp-password-verifier-envelope#task#8.

## Evidence
Replay of trace set simparity-wire-aws-v1 (200 fixtures, 1815 steps) against the emulator built from this checkout.

Cause 1 — fixture cip-autherr-flow-not-enabled-user-srp step task#6, InitiateAuth, expected 400 / actual 400:
  $.message: value_mismatch (expected 'USER_SRP_AUTH is not enabled for the client.', got 'USER_SRP_AUTH flow not enabled for this client')

Cause 2 — fixture cip-chal-srp-password-verifier-envelope step task#6, InitiateAuth, expected 200 / actual 200:
  $.ChallengeParameters.SECRET_BLOCK: value_mismatch (expected '<shaped-not-asserted>', got a short base64 blob)
  $.ChallengeParameters.SRP_B: value_mismatch (expected '<shaped-not-asserted>', got a hex big-int)
  $.Session: extra (expected None, got a session token)

Cause 3 — fixture cip-chal-srp-password-verifier-envelope step task#7, RespondToAuthChallenge, expected 400 / actual 400:
  $.message: value_mismatch (expected 'Missing required parameter PASSWORD_CLAIM_SIGNATURE', got 'Missing required parameter Session')

Cause 4 — fixture cip-chal-srp-password-verifier-envelope step task#8, RespondToAuthChallenge, expected 400 / actual 400:
  $.__type: value_mismatch (expected 'NotAuthorizedException', got 'InvalidParameterException')
  $.message: value_mismatch (expected 'Incorrect username or password.', got 'Missing required parameter Session')
  $headers.x-amzn-errortype: shape_mismatch (expected NotAuthorizedException, got 'InvalidParameterException:')

Model evidence: Smithy InitiateAuth / RespondToAuthChallenge — Session is an OPTIONAL SessionType member on both input and output; ChallengeNameType includes PASSWORD_VERIFIER; both operations model InvalidParameterException (400) and NotAuthorizedException as distinct errors.

No open issue owns these steps: the replay reported owned_by = [] for all four clusters. The nearest open issues are 20261123T074717Z-cognito-idp-reopens-the-device-srp-continuation-is-refused-respondtoauthchallenge.md (DEVICE_PASSWORD_VERIFIER continuation, a different challenge) and 20261024T114943Z-cognito-idp-lambda-the-pool-s-jwks-document.md (CUSTOM_AUTH unimplemented); neither covers the USER_SRP_AUTH PASSWORD_VERIFIER envelope or these refusal strings.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: create a user pool and an app client WITHOUT USER_SRP_AUTH in ExplicitAuthFlows, then call InitiateAuth(AuthFlow=USER_SRP_AUTH, ClientId=<client>, AuthParameters={USERNAME, SRP_A}). Observe $.message = 'USER_SRP_AUTH flow not enabled for this client' instead of AWS's 'USER_SRP_AUTH is not enabled for the client.' — replay fixture cip-autherr-flow-not-enabled-user-srp step task#6.
3. Cause 2: with USER_SRP_AUTH enabled and a confirmed user, call InitiateAuth(AuthFlow=USER_SRP_AUTH, AuthParameters={USERNAME, SRP_A}). Observe ChallengeName=PASSWORD_VERIFIER returned together with a Session member (AWS returns none) and SECRET_BLOCK/SRP_B outside AWS's shape class — replay fixture cip-chal-srp-password-verifier-envelope step task#6.
4. Cause 3: call RespondToAuthChallenge(ChallengeName=PASSWORD_VERIFIER, ClientId=<client>, ChallengeResponses={USERNAME, PASSWORD_CLAIM_SECRET_BLOCK, TIMESTAMP} and NO Session), omitting PASSWORD_CLAIM_SIGNATURE. Observe 'Missing required parameter Session' instead of 'Missing required parameter PASSWORD_CLAIM_SIGNATURE' — step task#7 of the same fixture.
5. Cause 4: repeat step 4 with a complete but incorrect PASSWORD_CLAIM_SIGNATURE. Observe InvalidParameterException / 'Missing required parameter Session' instead of NotAuthorizedException / 'Incorrect username or password.' — step task#8 of the same fixture.
