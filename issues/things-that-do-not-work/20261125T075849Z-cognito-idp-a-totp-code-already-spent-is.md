Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.admin-respond-to-auth-challenge-refuses-a-replayed-totp-without-spending-the-session
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.admin-respond-to-auth-challenge-refuses-a-replayed-totp-without-spending-the-session|expect_error#21|AssertionError|raise AssertionError(f"expected {code} but the call succeeded: keys {sorted(result)!r}")
Issue-Commit: 5261a6608fb5c4ad7b21097cccea3743144efaab

# cognito-idp: a TOTP code already spent is accepted again by AdminRespondToAuthChallenge instead of being refused ExpiredCodeException "Your software token has already been used once."

> _Note: this issue was identified against the code base at commit `5261a6608fb5c4ad7b21097cccea3743144efaab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminRespondToAuthChallenge

## Description
One net-new root cause on the cognito-idp software-token (TOTP) MFA plane.

Cause 1 of 1: AdminRespondToAuthChallenge accepts a SOFTWARE_TOKEN_MFA_CODE that has already been consumed once, completing the challenge and issuing tokens, where AWS refuses the replayed code and keeps the session live.

AWS: Smithy (AdminRespondToAuthChallenge): input {UserPoolId (required, UserPoolIdType @length 1..55, pattern ^[\w-]+_[0-9a-zA-Z]+$), ClientId (required, sensitive), ChallengeName (required, ChallengeNameType enum), Session (SessionType @length 20..4096, sensitive), ChallengeResponses (sensitive map)}; modeled errors include ExpiredCodeException (client, HTTP 400), CodeMismatchException (client, 400), SoftwareTokenMFANotFoundException (client, 400) and NotAuthorizedException (client, 403). The sibling RespondToAuthChallenge models the identical error set, and VerifySoftwareToken (which spends an enrolment code) models CodeMismatchException/NotAuthorizedException. Recorded behavior: a TOTP code for the user's associated software token is single-use. Once a given 30-second window's code has been consumed — either by VerifySoftwareToken during enrolment, or by successfully completing a SOFTWARE_TOKEN_MFA challenge — presenting that same code again to AdminRespondToAuthChallenge (ChallengeName=SOFTWARE_TOKEN_MFA, ChallengeResponses={USERNAME, SOFTWARE_TOKEN_MFA_CODE}) is refused with HTTP 400, error code ExpiredCodeException and the message "Your software token has already been used once." — that whole sentence is fixed text, with nothing interpolated from the request or state. The refusal does NOT spend the challenge session: the same Session string, presented with the NEXT 30-second window's code, still completes and returns an AuthenticationResult with TokenType "Bearer" and a working AccessToken (GetUser on it resolves to that username). The single-use rule is per code value and persists across sign-ins: a code consumed to complete one challenge is still refused ExpiredCodeException on a later, freshly initiated SOFTWARE_TOKEN_MFA challenge for the same user.

Emulator: the replayed code is accepted. After AssociateSoftwareToken + VerifySoftwareToken(UserCode=<code C>) + SetUserMFAPreference(SoftwareTokenMfaSettings Enabled/PreferredMfa), AdminInitiateAuth(ADMIN_USER_PASSWORD_AUTH) correctly returns ChallengeName SOFTWARE_TOKEN_MFA with a Session, but AdminRespondToAuthChallenge with that SAME code C answers HTTP 200 with a body carrying AuthenticationResult and ChallengeParameters (replay reports: "expected ExpiredCodeException but the call succeeded: keys ['AuthenticationResult', 'ChallengeParameters', 'ResponseMetadata']"). So the emulator validates the TOTP code only against the current time window and keeps no record of which codes have been consumed; no ExpiredCodeException is ever served, and the later same-code replay against a second sign-in is accepted for the same reason.

Why: a client relying on AWS's behavior gets a weaker security posture and a different control flow. The single-use guarantee is what makes TOTP MFA resistant to code capture/replay — an attacker (or a buggy retry) that observes one six-digit code can authenticate again inside the same window and, on this emulator, across later sign-ins too. Applications that branch on ExpiredCodeException to prompt "that code was already used, wait for the next one" never see that branch and instead observe a successful authentication; conversely any test or policy asserting replay protection passes against the emulator and fails against AWS. Because the check stops at this unexpected success, the downstream AWS guarantees it asserts — that the refusal leaves the session live so the next window's code completes that same Session, and that the completed session's token resolves to the user — are left unverified on the emulator.

Fix: the software-token MFA verification path shared by the AdminRespondToAuthChallenge / RespondToAuthChallenge handlers and VerifySoftwareToken in the cognito-idp crate, plus the per-user software-token state it reads: record each TOTP code value (or its time step) as consumed when it is accepted by VerifySoftwareToken or by a completed SOFTWARE_TOKEN_MFA challenge, and on a subsequent presentation of an already-consumed code raise ExpiredCodeException (HTTP 400) with "Your software token has already been used once." before the challenge is completed; the refusal must leave the challenge session unspent so the next window's code still completes it, and the consumed-code record must outlive the individual sign-in so a code spent on one challenge is still refused on a later one.

Traces: cognito-idp.admin-respond-to-auth-challenge-refuses-a-replayed-totp-without-spending-the-session. Related but not covering: open issue 20261122T020123Z-cognito-idp-2-net-new-causes-adminrespondtoauthchallenge-misclassifies.md describes the AdminRespondToAuthChallenge session-binding misclassification (CodeMismatchException vs NotAuthorizedException) and does not mention software-token code replay; open issue 20261121T105536Z-cognito-idp-setusermfapreference-uses-the-pool-s-no.md concerns SetUserMFAPreference messaging only.

## Evidence
Cause 1 (cognito-idp.admin-respond-to-auth-challenge-refuses-a-replayed-totp-without-spending-the-session), verbatim from the replay:
  File "check_cognito_idp_admin_respond_to_auth_challenge_refuses_a_replayed_totp_without_spending_the_session.py", line 69, in run
    expect_error(lambda: respond(first["Session"], enrol_code), *_REPLAYED)
  File "...", line 113, in expect_error
    raise AssertionError(f"expected {code} but the call succeeded: keys {sorted(result)!r}")
  AssertionError: expected ExpiredCodeException but the call succeeded: keys ['AuthenticationResult', 'ChallengeParameters', 'ResponseMetadata']

The failing call is AdminRespondToAuthChallenge(UserPoolId=<pool with MfaConfiguration=OPTIONAL and SoftwareTokenMfaConfiguration.Enabled=true>, ClientId=<client with ALLOW_ADMIN_USER_PASSWORD_AUTH>, ChallengeName="SOFTWARE_TOKEN_MFA", Session=<the Session AdminInitiateAuth returned with ChallengeName SOFTWARE_TOKEN_MFA>, ChallengeResponses={"USERNAME": <username>, "SOFTWARE_TOKEN_MFA_CODE": <the SAME six-digit code already consumed by VerifySoftwareToken>}).
- expected: HTTP 400, Error.Code "ExpiredCodeException", Error.Message "Your software token has already been used once." — the entire message is fixed text; nothing in it is interpolated from the request or state.
- actual: HTTP 200 with a body containing AuthenticationResult and ChallengeParameters; no error is raised, so the replayed code authenticates the user.

The same divergence is asserted a second time in the check against a freshly initiated challenge (a code spent to COMPLETE one SOFTWARE_TOKEN_MFA challenge must still be refused ExpiredCodeException on the next sign-in), and the unverified downstream assertions are: after the refusal the original Session plus the next 30-second window's code returns AuthenticationResult.TokenType == "Bearer", and GetUser(AccessToken=<that token>) returns Username == <the username under test>. The specific six-digit code values, pool/client ids and usernames in the recording are per-run values that fill the template; the requirement is the single-use rule, the status, the code and the sentence.

## Steps to Reproduce
Re-replay the check in trace_ids. In outline:
(1) CreateUserPool.
(2) SetUserPoolMfaConfig(UserPoolId, MfaConfiguration="OPTIONAL", SoftwareTokenMfaConfiguration={Enabled: true}).
(3) CreateUserPoolClient(GenerateSecret=false, ExplicitAuthFlows=[ALLOW_ADMIN_USER_PASSWORD_AUTH, ALLOW_USER_PASSWORD_AUTH, ALLOW_REFRESH_TOKEN_AUTH]).
(4) AdminCreateUser(MessageAction=SUPPRESS) then AdminSetUserPassword(Permanent=true).
(5) AdminInitiateAuth(AuthFlow=ADMIN_USER_PASSWORD_AUTH, {USERNAME, PASSWORD}) -> AuthenticationResult.AccessToken.
(6) AssociateSoftwareToken(AccessToken) -> SecretCode; compute the current TOTP code C (HMAC-SHA1, 30 s period, 6 digits) from it.
(7) VerifySoftwareToken(AccessToken, UserCode=C) -> Status "SUCCESS"; SetUserMFAPreference(AccessToken, SoftwareTokenMfaSettings={Enabled: true, PreferredMfa: true}).
(8) AdminInitiateAuth again -> ChallengeName "SOFTWARE_TOKEN_MFA" plus Session S.
(9) AdminRespondToAuthChallenge(UserPoolId, ClientId, ChallengeName="SOFTWARE_TOKEN_MFA", Session=S, ChallengeResponses={USERNAME, SOFTWARE_TOKEN_MFA_CODE: C}) — expect HTTP 400 ExpiredCodeException "Your software token has already been used once."; the emulator returns 200 with AuthenticationResult.
(10) Poll until the TOTP code changes to C2 (next 30 s window), then AdminRespondToAuthChallenge with the SAME Session S and C2 — expect success with AuthenticationResult.TokenType == "Bearer" (i.e. the refusal in step 9 did not spend S), and GetUser(AccessToken) == the username.
(11) AdminInitiateAuth once more -> ChallengeName "SOFTWARE_TOKEN_MFA" plus Session S2; AdminRespondToAuthChallenge(Session=S2, SOFTWARE_TOKEN_MFA_CODE: C2) — expect the identical 400 ExpiredCodeException "Your software token has already been used once.", proving the consumed-code record survives across sign-ins.
