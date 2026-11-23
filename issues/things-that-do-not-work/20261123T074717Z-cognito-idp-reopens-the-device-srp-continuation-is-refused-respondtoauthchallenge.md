Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.e2e-device-remembered-skips-mfa-challenge
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-identity-provider|RespondToAuthChallenge|NotAuthorizedException
Issue-Commit: cdaca303dd80edc32c77ea853d2e866117177c76

# REOPENS: the device-SRP continuation is refused — RespondToAuthChallenge(DEVICE_PASSWORD_VERIFIER) answers NotAuthorizedException 'session can only be used once' for the session its own DEVICE_SRP_AUTH challenge was issued on

> _Note: this issue was identified against the code base at commit `cdaca303dd80edc32c77ea853d2e866117177c76`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RespondToAuthChallenge

## Description
REOPENS 20261117T184623Z-cognito-idp-a-confirmed-remembered-device-is-signed.md: its fix left the call refused.

That fixed issue made InitiateAuth route a confirmed+remembered device to DEVICE_SRP_AUTH and declared the DEVICE_SRP_AUTH -> DEVICE_PASSWORD_VERIFIER exchange in scope ("the RespondToAuthChallenge handler (and its admin sibling) must implement DEVICE_SRP_AUTH -> DEVICE_PASSWORD_VERIFIER ... verifying the PASSWORD_CLAIM_SIGNATURE, minting tokens"). The first half now works — the behavioral check reaches ChallengeName='DEVICE_SRP_AUTH' and then ChallengeName='DEVICE_PASSWORD_VERIFIER' with SRP_B/SALT/SECRET_BLOCK — but the SECOND leg of the handshake is now REFUSED, so tokens are still unreachable and the device-SRP proof of possession still cannot complete.

=== Cause 1 of 1: the challenge Session minted by InitiateAuth is spent by the DEVICE_SRP_AUTH answer, so the DEVICE_PASSWORD_VERIFIER answer carried on that same Session is refused NotAuthorizedException instead of completing the sign-in. ===
AWS: a remembered-device sign-in is a THREE-call exchange over ONE session. InitiateAuth(AuthFlow=USER_PASSWORD_AUTH, AuthParameters={USERNAME, PASSWORD, DEVICE_KEY}) answers ChallengeName=DEVICE_SRP_AUTH plus a Session; RespondToAuthChallenge(ChallengeName=DEVICE_SRP_AUTH, Session=<that session>, ChallengeResponses={USERNAME, DEVICE_KEY, SRP_A}) answers ChallengeName=DEVICE_PASSWORD_VERIFIER with ChallengeParameters {SRP_B, SALT, SECRET_BLOCK}; RespondToAuthChallenge(ChallengeName=DEVICE_PASSWORD_VERIFIER, Session=<the SAME session>, ChallengeResponses={TIMESTAMP, USERNAME, DEVICE_KEY, PASSWORD_CLAIM_SECRET_BLOCK, PASSWORD_CLAIM_SIGNATURE}) answers HTTP 200 with AuthenticationResult and no ChallengeName. Smithy: RespondToAuthChallenge input Session (SessionType, @length 20..4096) is optional and output declares ChallengeName/Session/ChallengeParameters/AuthenticationResult; ChallengeNameType enumerates DEVICE_SRP_AUTH and DEVICE_PASSWORD_VERIFIER; NotAuthorizedException is @httpError 403. Recorded behavior only for the continuation rule, and the recording is explicit about it: AWS's DEVICE_PASSWORD_VERIFIER challenge response carries ONLY ChallengeName + ChallengeParameters and NO Session member at all, so the only session a client can present on the third call is the one the original InitiateAuth minted — i.e. answering the DEVICE_SRP_AUTH challenge ADVANCES the session's state, it does not retire it. The one-use rule AWS does apply is per completed sign-in: a session that has already yielded an AuthenticationResult cannot be replayed.
Emulator: the third call is refused HTTP 403, Error.Code 'NotAuthorizedException', Error.Message 'Invalid session for the user, session can only be used once.' — that sentence is entirely fixed text (nothing is interpolated from the request or state) and is the emulator's SESSION_ALREADY_USED_MESSAGE, declared at services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice/src/state.rs:604-605 with the comment "answering its challenge is what spends it". The DEVICE_SRP_AUTH answer is treated as the session's one and only use, so the device handshake can never be finished and no AuthenticationResult is ever issued on the device path.
Why: a client that implements the documented remembered-device handshake (every AWS mobile SDK does) cannot sign in at all against the emulator: it gets a DEVICE_PASSWORD_VERIFIER challenge it is then forbidden to answer, and the 403 it receives is indistinguishable from a genuinely replayed session. The device-SRP proof of possession that the reopened issue existed to make observable is still unobservable — the correct-claim token issuance and the wrong-device-password NotAuthorizedException refusal both sit behind this call — and a multi-leg challenge chain that retires its session after the first leg makes any future multi-step challenge (DEVICE_SRP_AUTH, MFA_SETUP, CUSTOM_AUTH chains) unanswerable.
Fix: the session bookkeeping shared by the RespondToAuthChallenge handler (src/ops/respond_to_auth_challenge.rs) and its admin sibling, over the challenge-session state in src/state/sessions.rs and the device leg in src/state/device_auth.rs: answering an INTERMEDIATE challenge must advance the stored session to the next outstanding challenge (DEVICE_SRP_AUTH -> DEVICE_PASSWORD_VERIFIER) and leave it answerable on the SAME Session value, and the already-used refusal (state.rs SESSION_ALREADY_USED_MESSAGE) must apply only once the session has produced an AuthenticationResult. The DEVICE_PASSWORD_VERIFIER challenge response must also carry no Session member, matching the recording.
Traces: cognito-idp.e2e-device-remembered-skips-mfa-challenge.

## Evidence
Failing check (verbatim from the replay report, under service heading `### cognito-idp`):
  [error] cognito-idp.e2e-device-remembered-skips-mfa-challenge
  Error: File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.NotAuthorizedException: An error occurred (NotAuthorizedException) when calling the RespondToAuthChallenge operation: Invalid session for the user, session can only be used once.

Where in the check: the two earlier legs PASSED, which is what localises the failure to the third call —
  assert remembered.get("ChallengeName") != "SOFTWARE_TOKEN_MFA"   (passed: the remembered device is no longer MFA-challenged)
  assert remembered.get("ChallengeName") == "DEVICE_SRP_AUTH"      (passed)
  srp_resp = respond_to_auth_challenge(ChallengeName="DEVICE_SRP_AUTH", Session=remembered["Session"], ...)
  assert srp_resp.get("ChallengeName") == "DEVICE_PASSWORD_VERIFIER"  (passed; ChallengeParameters carried SRP_B/SALT/SECRET_BLOCK)
  final = respond_to_auth_challenge(ChallengeName="DEVICE_PASSWORD_VERIFIER", ChallengeResponses=claim, Session=remembered["Session"])   <-- refused here
The check's own recorded note on that last call: "Real AWS does not echo a fresh Session on the DEVICE_PASSWORD_VERIFIER ChallengeParameters (verified empirically: the response carries only ChallengeName + ChallengeParameters, no Session key at all) -- the device-SRP continuation reuses the Session minted by the original InitiateAuth call."

Expected vs actual for RespondToAuthChallenge(ChallengeName='DEVICE_PASSWORD_VERIFIER', Session=<the InitiateAuth session>, ChallengeResponses={TIMESTAMP, USERNAME, DEVICE_KEY, PASSWORD_CLAIM_SECRET_BLOCK, PASSWORD_CLAIM_SIGNATURE}):
  Expected: HTTP 200, no ChallengeName, AuthenticationResult with a non-empty AccessToken.
  Actual:   HTTP 403, Error.Code 'NotAuthorizedException', Error.Message 'Invalid session for the user, session can only be used once.' (all fixed text), no AuthenticationResult.

Source of the refusal in the checkout: services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice/src/state.rs:604-605 (`pub const SESSION_ALREADY_USED_MESSAGE: &str = "Invalid session for the user, session can only be used once.";`), documented there as "answering its challenge is what spends it" — which is the rule that is wrong for an intermediate challenge.

Nothing in this finding depends on a per-run value: the pool/client ids, DeviceKey, DeviceGroupKey, salt, verifier, SRP ephemerals, session token and timestamps are all generated per run and are not part of the requirement.

## Steps to Reproduce
1. CreateUserPool with DeviceConfiguration={'ChallengeRequiredOnNewDevice': True, 'DeviceOnlyRememberedOnUserPrompt': True}; SetUserPoolMfaConfig(MfaConfiguration='OPTIONAL', SoftwareTokenMfaConfiguration={'Enabled': True}); CreateUserPoolClient(GenerateSecret=False, ExplicitAuthFlows=['ALLOW_USER_PASSWORD_AUTH','ALLOW_REFRESH_TOKEN_AUTH']).
2. AdminCreateUser(MessageAction='SUPPRESS') + AdminSetUserPassword(Permanent=True); sign in with InitiateAuth(USER_PASSWORD_AUTH); AssociateSoftwareToken + VerifySoftwareToken with the derived TOTP code; SetUserMFAPreference(SoftwareTokenMfaSettings={'Enabled': True, 'PreferredMfa': True}).
3. InitiateAuth(USER_PASSWORD_AUTH) -> SOFTWARE_TOKEN_MFA; answer it with a fresh code and read NewDeviceMetadata.DeviceKey/DeviceGroupKey from AuthenticationResult.
4. Derive an SRP salt/verifier for a locally chosen device password over the RFC 5054 3072-bit group (x = H(salt | H("<deviceGroupKey><deviceKey>:<devicePassword>")), v = g^x mod N); ConfirmDevice(AccessToken, DeviceKey, DeviceSecretVerifierConfig={'PasswordVerifier': b64(v), 'Salt': b64(salt)}); UpdateDeviceStatus(DeviceRememberedStatus='remembered').
5. InitiateAuth(USER_PASSWORD_AUTH, AuthParameters={USERNAME, PASSWORD, DEVICE_KEY}) -> ChallengeName='DEVICE_SRP_AUTH' with a Session S. Keep S.
6. RespondToAuthChallenge(ChallengeName='DEVICE_SRP_AUTH', Session=S, ChallengeResponses={USERNAME, DEVICE_KEY, SRP_A=hex(A)}) -> ChallengeName='DEVICE_PASSWORD_VERIFIER' with ChallengeParameters {SRP_B, SALT, SECRET_BLOCK}. Note that AWS's response carries NO Session member.
7. Compute the Caldera HKDF session key from the same ephemeral pair and the device password and call RespondToAuthChallenge(ChallengeName='DEVICE_PASSWORD_VERIFIER', Session=S, ChallengeResponses={TIMESTAMP, USERNAME, DEVICE_KEY, PASSWORD_CLAIM_SECRET_BLOCK, PASSWORD_CLAIM_SIGNATURE}). Expect HTTP 200 with AuthenticationResult and no ChallengeName; observe HTTP 403 NotAuthorizedException 'Invalid session for the user, session can only be used once.'
8. Regression guard: once step 7 yields tokens, replaying the SAME S must then be refused with that one-use message — the refusal is due only after the session has produced an AuthenticationResult.
