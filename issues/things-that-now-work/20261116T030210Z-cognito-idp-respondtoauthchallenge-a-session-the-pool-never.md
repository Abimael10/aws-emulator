Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.op-contract-respond-to-auth-challenge
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.op-contract-respond-to-auth-challenge|run#19|AssertionError|assert err.get("Code") == "CodeMismatchException", f"expected CodeMismatchException, got {err!r}"
Issue-Commit: f6d8f01946911e76d57d5db06ea3a96a4efb6135

# cognito-idp RespondToAuthChallenge: a Session the pool never issued is refused 403 NotAuthorizedException "Invalid session for the user." instead of 400 CodeMismatchException "Invalid code/session provided"

> _Note: this issue was identified against the code base at commit `f6d8f01946911e76d57d5db06ea3a96a4efb6135`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RespondToAuthChallenge

## Description
One net-new root cause in cognito-idp RespondToAuthChallenge's handling of an unresolvable Session. No open issue carries this check: the other RespondToAuthChallenge failures in this suite are owned by 20261029T182116Z-cognito-idp-mfa-challenge-state-machine-mfa-setup.md (MFA challenge state machine) and 20261114T000014Z-cognito-idp-kms-lambda-sqs-cognito-idp-respondtoauthchallenge-refuses-an-email-otp-answered.md (email-OTP session expiry), and neither describes the error class chosen for an unparseable session string.

Cause 1 of 1 — an unresolvable Session is classified as an authorization failure instead of a code/session mismatch.
AWS: Smithy: RespondToAuthChallenge input Session -> SessionType (@length min 20, max 4096, sensitive, not required); modeled errors include CodeMismatchException (client, HTTP 400, doc: "This exception is thrown if the provided code doesn't match what the server was expecting") and NotAuthorizedException (client, HTTP 403, doc: "This exception is thrown when a user isn't authorized"). Recorded behavior: a RespondToAuthChallenge call for ChallengeName=NEW_PASSWORD_REQUIRED whose Session is a string Cognito never issued is refused with HTTP 400, error code CodeMismatchException and the message "Invalid code/session provided" — AWS parses the opaque session blob as (invalid) challenge-response material and reports a mismatch, not an auth failure. The whole message is fixed text; it interpolates nothing from the request or from pool state (no username, client id or challenge name appears in it).
Emulator: the same call is refused with HTTP 403, error code NotAuthorizedException and the message "Invalid session for the user." — wrong status class, wrong error code, and a sentence AWS does not write for this case. The happy path in the same check (InitiateAuth USER_PASSWORD_AUTH -> NEW_PASSWORD_REQUIRED -> RespondToAuthChallenge issuing tokens and moving the user to CONFIRMED) passes, so only the refusal branch diverges.
Why: a client that branches on the modeled error — retrying/prompting on CodeMismatchException (400, a bad code the user can re-enter) versus restarting authentication or surfacing a security failure on NotAuthorizedException (403) — takes the wrong path for every stale, truncated or tampered session token; SDK retry/error-mapping layers that classify 403 differently from 400 also diverge, and tests asserting AWS's error code or sentence fail against the emulator.
Fix: the session-resolution branch of the RespondToAuthChallenge handler (the challenge-response validation that rejects a session it cannot resolve) must raise the CodeMismatchException the operation models, with AWS's sentence, instead of NotAuthorizedException.
Traces: cognito-idp.op-contract-respond-to-auth-challenge.

## Evidence
Verbatim from the replay (cognito-idp.op-contract-respond-to-auth-challenge):

  File "check_cognito_idp_op_contract_respond_to_auth_challenge.py", line 64, in run
    assert err.get("Code") == "CodeMismatchException", f"expected CodeMismatchException, got {err!r}"
  AssertionError: expected CodeMismatchException, got {'Message': 'Invalid session for the user.', 'Code': 'NotAuthorizedException'}

Expected vs actual:
- expected: HTTP 400, Code = CodeMismatchException, Message = "Invalid code/session provided" (the check asserts the exact sentence on the next line). Entirely fixed text; no variable parts.
- actual: HTTP 403, Code = NotAuthorizedException, Message = "Invalid session for the user." (emulator-authored sentence, with a trailing period AWS does not write here).

Everything before the refusal matched AWS: AdminCreateUser left the user FORCE_CHANGE_PASSWORD, InitiateAuth(USER_PASSWORD_AUTH) answered ChallengeName=NEW_PASSWORD_REQUIRED with a Session, RespondToAuthChallenge(NEW_PASSWORD_REQUIRED) returned an AuthenticationResult with an AccessToken, and AdminGetUser then reported CONFIRMED.

## Steps to Reproduce
1. CreateUserPool, then CreateUserPoolClient with ExplicitAuthFlows ALLOW_USER_PASSWORD_AUTH, ALLOW_ADMIN_USER_PASSWORD_AUTH, ALLOW_REFRESH_TOKEN_AUTH, ALLOW_USER_SRP_AUTH.
2. AdminCreateUser with a TemporaryPassword and MessageAction=SUPPRESS; AdminGetUser reports UserStatus FORCE_CHANGE_PASSWORD.
3. InitiateAuth(ClientId, AuthFlow=USER_PASSWORD_AUTH, AuthParameters={USERNAME, PASSWORD=<temp password>}) -> ChallengeName=NEW_PASSWORD_REQUIRED plus a Session.
4. RespondToAuthChallenge(ClientId, ChallengeName=NEW_PASSWORD_REQUIRED, Session=<that session>, ChallengeResponses={USERNAME, NEW_PASSWORD}) -> succeeds (AuthenticationResult.AccessToken present; AdminGetUser now CONFIRMED).
5. Call RespondToAuthChallenge again with Session="not-a-real-session-token" (any string of 20..4096 chars the pool never issued) and ChallengeResponses={USERNAME, NEW_PASSWORD=<another valid password>}.
   Expect: HTTP 400, CodeMismatchException, Message "Invalid code/session provided".
   Observed: HTTP 403, NotAuthorizedException, Message "Invalid session for the user."
