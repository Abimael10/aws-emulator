Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.device-srp-auth-completes-with-a-locally-computed-device-verifier
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.device-srp-auth-completes-with-a-locally-computed-device-verifier|_reach_device_password_verifier#29|Timeout|login = c.initiate_auth(
Issue-Commit: 5f72b15ceea68fb83a63d5bc988a846400d3d8ef

# cognito-idp: a confirmed+remembered device is signed in outright, so DEVICE_SRP_AUTH / DEVICE_PASSWORD_VERIFIER is unreachable and the device-SRP proof of possession is never required

> _Note: this issue was identified against the code base at commit `5f72b15ceea68fb83a63d5bc988a846400d3d8ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InitiateAuth

## Description
One net-new behavioral root cause in cognito-idp, grounded in the failing behavioral check cognito-idp.device-srp-auth-completes-with-a-locally-computed-device-verifier (verdict: mismatch, "Check timed out after 60.0s").

Related but NOT covering this check: open issue 20261115T175613Z-cognito-idp-4-net-new-causes-a-spent.md describes the same device-tracking area in its Cause 3, and the replay report does NOT mark this check as OWNED by it (the two sibling checks cognito-idp.e2e-device-remember-skips-mfa-then-forget-reinstates and cognito-idp.e2e-remembered-device-skips-mfa-on-next-login ARE marked owned, this one is not — its current replay key is not carried by that issue). It is filed here so the key is owned and so the second half of the handshake this check alone exercises — the DEVICE_PASSWORD_VERIFIER claim check and its refusal — is explicitly in scope. Nothing here is a call a fixed issue claimed to fix and left refused, so this is not a REOPENS: the emulator answers 200 where AWS issues a challenge.

=== Cause 1 of 1: with device tracking on, an InitiateAuth carrying DEVICE_KEY for a confirmed, remembered device completes authentication instead of challenging DEVICE_SRP_AUTH, and the DEVICE_SRP_AUTH -> DEVICE_PASSWORD_VERIFIER exchange (including its wrong-device-password refusal) is therefore never served. ===
AWS: for a pool created with DeviceConfiguration={ChallengeRequiredOnNewDevice: true, DeviceOnlyRememberedOnUserPrompt: true}, a device whose SRP material was registered with ConfirmDevice (Smithy: ConfirmDevice input DeviceKey @pattern ^[\w-]+_[0-9a-f-]+$ @length 1..55, DeviceSecretVerifierConfig {PasswordVerifier, Salt} — "A password verifier for a user's device. Used in SRP authentication." — output UserConfirmationNecessary) and then marked remembered with UpdateDeviceStatus(DeviceRememberedStatus='remembered') is NOT signed in on the strength of the DeviceKey. InitiateAuth(AuthFlow=USER_PASSWORD_AUTH, AuthParameters={USERNAME, PASSWORD, DEVICE_KEY}) answers with ChallengeName=DEVICE_SRP_AUTH plus a Session (Smithy: ChallengeNameType enumerates DEVICE_SRP_AUTH and DEVICE_PASSWORD_VERIFIER; InitiateAuth/RespondToAuthChallenge output declare ChallengeName, Session, ChallengeParameters, AuthenticationResult). Answering it with RespondToAuthChallenge(ChallengeName=DEVICE_SRP_AUTH, ChallengeResponses={USERNAME, DEVICE_KEY, SRP_A}) answers ChallengeName=DEVICE_PASSWORD_VERIFIER whose ChallengeParameters carry SRP_B, SALT and SECRET_BLOCK. A PASSWORD_CLAIM_SIGNATURE computed from the device password the stored verifier was derived from yields AuthenticationResult with tokens, and the access token carries a device_key claim equal to that DeviceKey and verifies RS256 against the pool's published JWKS. The same claim signed from a DIFFERENT device password is refused with NotAuthorizedException (Smithy: RespondToAuthChallenge errors include NotAuthorizedException @httpError 403) and the message 'Incorrect username or password.' — that message is entirely fixed text, with no part taken from the request or from state — and no AuthenticationResult accompanies the refusal. Recorded behavior only for the SRP key schedule itself (the undocumented "Caldera" SRP-6a variant over the RFC 5054 3072-bit group, hashing deviceGroupKey+deviceKey rather than poolName+username) and for the remembered status being eventually consistent on the auth path, which is why the check polls for the DEVICE_SRP_AUTH routing rather than asserting it on the first read.
Emulator: the same InitiateAuth answers HTTP 200 with AuthenticationResult and no ChallengeName at all — possession of the DeviceKey alone completes the sign-in. DEVICE_SRP_AUTH is never issued, so the check's poll (InitiateAuth with DEVICE_KEY every 3 s, asserting 'the remembered device was not routed to DEVICE_SRP_AUTH') never advances and the check exhausts its 60 s budget. Consequently DEVICE_PASSWORD_VERIFIER is never reached either: the emulator never serves SRP_B/SALT/SECRET_BLOCK, never verifies a PASSWORD_CLAIM_SIGNATURE against the salt/verifier ConfirmDevice stored, never issues an access token carrying a device_key claim, and never produces the NotAuthorizedException refusal for a claim derived from the wrong device password. Everything before the device leg succeeds (pool/client creation, TOTP enrolment, the SOFTWARE_TOKEN_MFA challenge for the un-tracked device, ConfirmDevice returning UserConfirmationNecessary=true, UpdateDeviceStatus), so the divergence is specific to the device-auth path.
Why: the device-trust MFA skip is granted with no proof of possession. A DeviceKey is not a secret — it is handed to the client in NewDeviceMetadata and is readable from ListDevices/GetDevice — so on the emulator a password plus a DeviceKey bypasses the second factor entirely, where AWS still demands the device prove it holds the device password through the SRP exchange. A client implementing the documented remembered-device handshake (every AWS mobile SDK does) also cannot be tested at all: the challenge it is written against never arrives, and the wrong-device-password rejection it must surface to the user can never be observed.
Fix: the device-tracking branch of the InitiateAuth handler (and its AdminInitiateAuth sibling) must, when AuthParameters name a DEVICE_KEY belonging to a confirmed and remembered device of that user and the pool's DeviceConfiguration requires a challenge, issue the DEVICE_SRP_AUTH challenge with a Session instead of returning AuthenticationResult; the RespondToAuthChallenge handler (and its admin sibling) must implement DEVICE_SRP_AUTH -> DEVICE_PASSWORD_VERIFIER over the per-device salt/verifier stored by ConfirmDevice — serving SRP_B/SALT/SECRET_BLOCK in ChallengeParameters, verifying the PASSWORD_CLAIM_SIGNATURE, minting tokens whose access token carries the device_key claim, and raising NotAuthorizedException with AWS's fixed message for a claim that does not verify. The state these handlers read is the per-device record ConfirmDevice/UpdateDeviceStatus write (device key, device group key, salt, password verifier, remembered flag).
Traces: cognito-idp.device-srp-auth-completes-with-a-locally-computed-device-verifier.

## Evidence
Failing check (verbatim from the replay report, service heading `### cognito-idp`):
  [mismatch] cognito-idp.device-srp-auth-completes-with-a-locally-computed-device-verifier
  Error: Check timed out after 60.0s

The budget is consumed inside the check's _reach_device_password_verifier helper, which polls InitiateAuth every 3 s for up to 60 s and then asserts:
  assert login is not None and login.get("ChallengeName") == "DEVICE_SRP_AUTH", (
      f"the remembered device was not routed to DEVICE_SRP_AUTH: {login!r}")

Expected vs actual:
- InitiateAuth(AuthFlow='USER_PASSWORD_AUTH', AuthParameters={USERNAME, PASSWORD, DEVICE_KEY}) for a confirmed+remembered device
  Expected: HTTP 200 with ChallengeName='DEVICE_SRP_AUTH' and a Session, and no AuthenticationResult.
  Actual:   HTTP 200 with AuthenticationResult and no ChallengeName — the sign-in completes on the DeviceKey alone, so the poll never observes DEVICE_SRP_AUTH.
- RespondToAuthChallenge(ChallengeName='DEVICE_SRP_AUTH', ChallengeResponses={USERNAME, DEVICE_KEY, SRP_A})
  Expected: HTTP 200 with ChallengeName='DEVICE_PASSWORD_VERIFIER' and ChallengeParameters containing SRP_B, SALT and SECRET_BLOCK.
  Actual:   never reached — the preceding challenge is never issued.
- RespondToAuthChallenge(ChallengeName='DEVICE_PASSWORD_VERIFIER', ChallengeResponses={TIMESTAMP, USERNAME, DEVICE_KEY, PASSWORD_CLAIM_SECRET_BLOCK, PASSWORD_CLAIM_SIGNATURE}) with the signature derived from the device password the stored verifier came from
  Expected: HTTP 200, no ChallengeName, AuthenticationResult with an access token that verifies RS256 against the pool JWKS and whose claims carry username=<the user> and device_key=<that DeviceKey>.
  Actual:   never reached.
- the same claim signed from a DIFFERENT device password
  Expected: HTTP 403, Error.Code 'NotAuthorizedException', Error.Message exactly 'Incorrect username or password.' (entirely fixed text — nothing interpolated from the request or state), and no AuthenticationResult in the response.
  Actual:   never reached; the emulator never evaluates a device password claim, so this refusal cannot occur.

Nothing in this finding depends on a per-run value: the DeviceKey, DeviceGroupKey, salt, verifier, SRP ephemerals, pool and client ids and timestamps are all generated per run and are not part of the requirement.

## Steps to Reproduce
1. CreateUserPool with DeviceConfiguration={'ChallengeRequiredOnNewDevice': True, 'DeviceOnlyRememberedOnUserPrompt': True}; SetUserPoolMfaConfig(MfaConfiguration='OPTIONAL', SoftwareTokenMfaConfiguration={'Enabled': True}); CreateUserPoolClient(GenerateSecret=False, ExplicitAuthFlows=['ALLOW_USER_PASSWORD_AUTH','ALLOW_REFRESH_TOKEN_AUTH']).
2. AdminCreateUser(MessageAction='SUPPRESS') + AdminSetUserPassword(Permanent=True). Sign in with InitiateAuth(USER_PASSWORD_AUTH), then AssociateSoftwareToken / VerifySoftwareToken with the derived TOTP code and SetUserMFAPreference(SoftwareTokenMfaSettings={'Enabled': True, 'PreferredMfa': True}).
3. InitiateAuth(USER_PASSWORD_AUTH) again -> ChallengeName='SOFTWARE_TOKEN_MFA'; answer it with a fresh (unspent) code and read NewDeviceMetadata.DeviceKey / DeviceGroupKey from AuthenticationResult.
4. Derive an SRP salt and verifier for a locally chosen device password over the RFC 5054 3072-bit group, with x = H(salt | H("<deviceGroupKey><deviceKey>:<devicePassword>")) and v = g^x mod N; ConfirmDevice(AccessToken, DeviceKey, DeviceSecretVerifierConfig={'PasswordVerifier': b64(v), 'Salt': b64(salt)}) -> UserConfirmationNecessary must be True; UpdateDeviceStatus(AccessToken, DeviceKey, DeviceRememberedStatus='remembered').
5. InitiateAuth(USER_PASSWORD_AUTH, AuthParameters={USERNAME, PASSWORD, DEVICE_KEY}). Expect ChallengeName='DEVICE_SRP_AUTH' with a Session; observe HTTP 200 with AuthenticationResult and no ChallengeName (polling for up to 60 s never changes this).
6. RespondToAuthChallenge(ChallengeName='DEVICE_SRP_AUTH', Session, ChallengeResponses={USERNAME, DEVICE_KEY, SRP_A=hex(A)}). Expect ChallengeName='DEVICE_PASSWORD_VERIFIER' with ChallengeParameters {SRP_B, SALT, SECRET_BLOCK}.
7. Compute the Caldera HKDF session key from the SAME ephemeral pair used in step 6 and the device password, and answer RespondToAuthChallenge(ChallengeName='DEVICE_PASSWORD_VERIFIER', ChallengeResponses={TIMESTAMP, USERNAME, DEVICE_KEY, PASSWORD_CLAIM_SECRET_BLOCK, PASSWORD_CLAIM_SIGNATURE}). Expect no ChallengeName and tokens; the access token must verify against GET <endpoint>/<poolId>/.well-known/jwks.json and its claims must carry device_key=<DeviceKey>.
8. Repeat steps 5-7 with a signature derived from a DIFFERENT device password. Expect HTTP 403 NotAuthorizedException with the message exactly 'Incorrect username or password.' and no AuthenticationResult in the error response.
9. Control: InitiateAuth WITHOUT DEVICE_KEY must still be challenged 'SOFTWARE_TOKEN_MFA' (this already holds on the emulator).
