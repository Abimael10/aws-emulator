Issue-Source: trace-sweep-divergence
Issue-Trace: cognito-idp-admin-auth-challenge-flow
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cognito-idp-admin-auth-challenge-flow
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|AdminInitiateAuth|2xx->2xx|$.Session
Issue-Commit: f420f96d635d512f65dbfe0e5d9b800a5c85b936

# cognito-idp AdminInitiateAuth challenge Session is a 64-char hex digest instead of an AWS-shaped session token

> _Note: this issue was identified against the code base at commit `f420f96d635d512f65dbfe0e5d9b800a5c85b936`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminInitiateAuth

## Description
One net-new cause (service cognito-idp). The replay reports this cluster as unowned; the adjacent open issues do not carry its replay key.

Cause 1 of 1: AdminInitiateAuth's challenge response carries a Session value that is not an AWS-shaped session token, and is not the session value the recorded challenge flow carries forward.

AWS: when AdminInitiateAuth cannot complete authentication outright it answers HTTP 200 with ChallengeName, ChallengeParameters and an opaque Session that the caller must hand back verbatim to AdminRespondToAuthChallenge. Smithy: AdminInitiateAuth output member Session -> SessionType, a @sensitive string with @length min 20 max 4096 and no @pattern (the same shape appears on AdminInitiateAuth input Session and on the RespondToAuthChallenge pair). The model does not fix the encoding, so the token's form is recorded behavior only: the recordings show a long opaque base64url-style challenge token (hundreds of characters), and the corpus does not assert its literal text — it binds it as the flow's challenge-session variable (challenge_session) and re-presents it at the next step. The concrete token text is a per-run server-generated value and is NOT the requirement; the requirements are (a) the token belongs to AWS's opaque session-token shape class rather than a bare hex digest of internal state, and (b) the value a challenge response publishes is exactly the session the rest of the flow carries, so the binding taken from this step resolves.

Emulator: answers 200 with $.Session set to a 64-character lowercase-hexadecimal string (this run: '0e520de5292f460b9c0ce3e110298bd73389d1a2b14d425587f8387803c3c0ef'). It satisfies SessionType's length bound but not the recorded token shape, and it does not match the session the fixture's challenge_session binding holds for this step, so the assertion on $.Session fails (expected the bound challenge_session value, got the hex string). Hypothesis: the challenge session is minted as a raw hex digest over internal challenge state rather than as an encoded session blob, and is re-minted per response rather than published as the one session the challenge is keyed by.

Why: a client, proxy or test harness that validates or round-trips the challenge session against AWS's token shape rejects the value, and anything that sized a store column or buffer from observed AWS sessions is calibrated against a token an order of magnitude longer. Worse, a flow that captures the session from the AdminInitiateAuth response and replays it into AdminRespondToAuthChallenge cannot correlate the two: the value it holds is not the value the emulator considers current for that challenge, so a correct admin-auth challenge sequence cannot be driven end to end.

Fix: the AdminInitiateAuth handler's challenge-session minting/serializer (the session-token encoder, not the challenge state machine) — publish one stable, opaque AWS-shaped session for the outstanding challenge and serialize that same value. Same family as cause 8 of the open issue 'cognito-idp: response payloads diverge — read paths omit or misreport pool, client, domain and MFA state' (20261102T111936Z-cognito-idp-response-payloads-diverge-read-paths-omit.md), which carries only the replay key cognito-idp|InitiateAuth|2xx->2xx|$.Session and does not cover the admin path; the open issue 'cognito-idp: 6 net-new causes — hosted-UI/managed-login read paths...' (20261111T204833Z-cognito-idp-6-net-new-causes-hosted-ui.md) owns step task#5 of this same fixture (the AdminRespondToAuthChallenge error-class defect) and not this step.

Traces: cognito-idp-admin-auth-challenge-flow#task#4 (AdminInitiateAuth).

## Evidence
Replay of trace set simparity-wire-aws-v1 (200 fixtures, 1815 steps, 1721 passed, score 94.8%). One unowned divergence cluster, service=cognito-idp:

fixture cognito-idp-admin-auth-challenge-flow, step task#4, operation AdminInitiateAuth, verdict mismatch, status 200 -> 200, affected_count 1 (cognito-idp-admin-auth-challenge-flow#task#4):
  $.Session: value_mismatch (expected '{{challenge_session}}' — the flow's bound challenge-session token, got '0e520de5292f460b9c0ce3e110298bd73389d1a2b14d425587f8387803c3c0ef', a 64-char lowercase-hex string)

The divergence is the token's shape class and its identity with the session the flow carries, not its literal text: the recorded corpus never asserts the raw token (every other challenge step asserts it as a shape class or as this binding), and the hex digest reproduced above is this run's generated value, not a requirement.

Model evidence (Smithy, com.amazonaws.cognitoidentityprovider): AdminInitiateAuth output members ChallengeName (ChallengeNameType), Session (SessionType), ChallengeParameters, AuthenticationResult, AvailableChallenges; SessionType = string, @sensitive, @length min 20 max 4096, no @pattern. The adjacent InitiateAuth $.Session shape cluster in the same corpus (cip-chal-mfa-setup-required#task#7) shows the identical 64-char-hex symptom and is already owned by 20261102T111936Z-cognito-idp-response-payloads-diverge-read-paths-omit.md under the key cognito-idp|InitiateAuth|2xx->2xx|$.Session.

## Steps to Reproduce
1. git_clone the project and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus) and look at the cognito-idp clusters.
3. Cause 1: replay fixture cognito-idp-admin-auth-challenge-flow step task#4 — create the pool/user/app client as the earlier steps do, then call AdminInitiateAuth with an AuthFlow that lands on a challenge, and inspect the 200 body's $.Session. Observe a 64-character lowercase-hex string instead of the long opaque base64url-style token the recording carries, and note that the value does not match the session the fixture's challenge_session binding holds for this step (so the Session captured here is not the one the next AdminRespondToAuthChallenge call is keyed by).
4. Compare $.Session against the recorded response's token shape class (opaque, hundreds of characters, within SessionType @length 20..4096) — the literal text is per-run and is not the requirement.
5. Re-replay cognito-idp-admin-auth-challenge-flow#task#4 to confirm the fix before closing.
