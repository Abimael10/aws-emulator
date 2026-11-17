Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sso-oidc.create-token-with-iam-token-exchange-requires-subject-token-before-client-lookup
Issue-Shape-Id: com.amazonaws.ssooidc#AWSSSOOIDCService
Issue-Key: sso-oidc.create-token-with-iam-token-exchange-requires-subject-token-before-client-lookup|expect_error#3|AssertionError|assert got_message == message, f"expected message {message!r}, got {got_message!r}"
Issue-Commit: 4e99635405c26200cee463f04551477ed3b3aa28

# sso-oidc CreateTokenWithIAM(token-exchange) with no subjectToken answers InvalidRequestException error_description "Subject token is required" instead of AWS's "Subject Token is required"

> _Note: this issue was identified against the code base at commit `4e99635405c26200cee463f04551477ed3b3aa28`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTokenWithIAM

## Description
Cause 1 of 1: the fixed error_description text for the token-exchange grant's missing-subjectToken refusal is mis-cased in the emulator.

AWS: CreateTokenWithIAM with grantType=urn:ietf:params:oauth:grant-type:token-exchange and no subjectToken is refused with InvalidRequestException (Smithy: CreateTokenWithIAM errors include InvalidRequestException, errorKind client, httpError 400 — "Indicates that something is wrong with the input to the request. For example, a required parameter might be missing or out of range."). The model does not state the error_description text, so for that part this is recorded behavior only: the recording shows the refusal carries error code InvalidRequestException with message exactly "Subject Token is required". That message is entirely FIXED text for this refusal class — it has no variable parts drawn from the request or from state (the check deliberately uses a never-registered client ARN and still gets it, so the message is not parameterized by client, grant or token). The capital "T" in "Token" is part of that fixed text. The check also establishes the surrounding ordering contract: the missing field is caught BEFORE any client lookup, and supplying subjectToken + subjectTokenType for the same unregistered client ARN reaches grant lookup and is refused InvalidGrantException "Invalid token provided".

Emulator: returns the right error code and status but the message "Subject token is required" — lowercase "token". Replay output: AssertionError: expected message 'Subject Token is required', got 'Subject token is required'. The text comes from the service-wide fixed-description constant for this class, not from any per-request formatting, so every token-exchange request missing subjectToken gets the mis-cased sentence.

Why: error_description is the only machine-and-human discriminator between the several InvalidRequestException classes this operation can raise (missing subjectToken, missing refreshToken, missing code, missing assertion, unsupported subjectTokenType/requestedTokenType all share code InvalidRequestException / error=invalid_request / HTTP 400). A client, SDK retry wrapper, test suite or log-based alarm that matches the AWS sentence to tell "you forgot subjectToken" from "your request is otherwise invalid" fails to match against the emulator, so the branch that would surface an actionable message to the caller — or suppress a retry — is never taken, and the divergence is invisible to anyone comparing codes alone.

Fix: belongs in this operation's refusal text, i.e. the shared fixed-description constant the token-exchange branch of CreateTokenWithIAM's grant resolution hands to its invalid_request() helper. One constant carries this sentence for the whole crate, so correcting it there fixes every path that raises this class; the sibling constants for the other required-member refusals of the same operation should be re-checked against their recorded AWS wording at the same time, since they are written in the same sentence-case style and only this one is covered by a failing check.

Traces: sso-oidc.create-token-with-iam-token-exchange-requires-subject-token-before-client-lookup (mismatch). Not covered by either open sso-oidc issue: 20261109T124858Z-cognito-idp-identitystore-sso-admin-sso-oidc-createtokenwithiam.md is the jwt-bearer assertion rejection, and 20261110T062655Z-sso-oidc-createtoken-answers-an-expired-device-code.md is CreateToken's expired-device-code class.

## Evidence
Check: sso-oidc.create-token-with-iam-token-exchange-requires-subject-token-before-client-lookup [mismatch]

Request (clientId is a deliberately never-registered application ARN):
  CreateTokenWithIAM(clientId="arn:aws:sso::<account>:application/ssoins-.../apl-...",
                     grantType="urn:ietf:params:oauth:grant-type:token-exchange")
  (no subjectToken, no subjectTokenType)

Expected (AWS, recorded): HTTP 400, Error.Code = "InvalidRequestException",
  Error.Message = "Subject Token is required"

Actual (emulator): HTTP 400, Error.Code = "InvalidRequestException",
  Error.Message = "Subject token is required"

Replay assertion:
  File "check_sso_oidc_create_token_with_iam_token_exchange_requires_subject_token_before_client_lookup.py", line 31, in expect_error
    assert got_message == message, f"expected message {message!r}, got {got_message!r}"
  AssertionError: expected message 'Subject Token is required', got 'Subject token is required'

Fixed vs variable parts of the message: the whole sentence "Subject Token is required" is fixed text for this refusal class; nothing in it is substituted from the request or from emulator state (confirmed by the check getting it for an unregistered client ARN). Only the casing of "Token" diverges.

Source of the emulator's text:
  services/service-com-amazonaws-ssooidc-awsssooidcservice/src/state.rs:125
    pub const DESCRIPTION_SUBJECT_TOKEN_REQUIRED: &str = "Subject token is required";
  consumed at services/service-com-amazonaws-ssooidc-awsssooidcservice/src/ops/create_token_with_iam.rs:342
    return Err(invalid_request(st::DESCRIPTION_SUBJECT_TOKEN_REQUIRED));
  (the GRANT_TYPE_TOKEN_EXCHANGE arm, reached before the client registry is read — so the
   emulator's ordering already matches AWS; only the text diverges.)

Smithy (services/service-com-amazonaws-ssooidc-awsssooidcservice/smithy-model/com-amazonaws-ssooidc-awsssooidcservice.json):
  CreateTokenWithIAM errors include InvalidRequestException (errorKind: client, httpError: 400);
  input members subjectToken (sensitive) and subjectTokenType are both optional in the model,
  so the requirement is grant-conditional and enforced by the handler, not by modeled validation.
  The model does not specify error_description text — that part is recorded behavior only.

## Steps to Reproduce
1. Boot the emulator and point an sso-oidc client at it.
2. Call CreateTokenWithIAM with only clientId and grantType:
     clientId     = "arn:aws:sso::<account>:application/ssoins-0000000000000000/apl-0000000000000000"
                    (any ARN; it need not be a registered application — that is the point)
     grantType    = "urn:ietf:params:oauth:grant-type:token-exchange"
   Omit subjectToken and subjectTokenType entirely.
3. Observe the refusal. Expected AWS: HTTP 400, code InvalidRequestException, message
   "Subject Token is required". Observed: same status and code, message
   "Subject token is required".
4. Positive control that the refusal is about the missing field and not the unknown client
   (this part already matches AWS and should keep matching after the fix): repeat the call with
   the SAME unregistered clientId plus subjectToken="<any fabricated string>" and
   subjectTokenType="urn:ietf:params:oauth:token-type:access_token"; it must be refused
   InvalidGrantException / "Invalid token provided", i.e. it reaches grant lookup.
5. Re-replay sso-oidc.create-token-with-iam-token-exchange-requires-subject-token-before-client-lookup to confirm.
