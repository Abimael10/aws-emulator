Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.admin-delete-software-token-rejects-unconfigured-and-unknown-user
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.admin-delete-software-token-rejects-unconfigured-and-unknown-user|_error#11|AssertionError|raise AssertionError(f"expected a rejection, but the call SUCCEEDED and returned {result!r}")
Issue-Commit: ce9da1949a31b48f6e624e7aad44b9692a7b8b4d

# AdminDeleteSoftwareToken returns 200 instead of rejecting a user with no TOTP factor (and an unknown username)

> _Note: this issue was identified against the code base at commit `ce9da1949a31b48f6e624e7aad44b9692a7b8b4d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminDeleteSoftwareToken

## Description
One net-new root cause.

Cause 1 of 1: AdminDeleteSoftwareToken performs no precondition validation — it answers 200 with an empty body for every well-formed request, whether or not the named user has ever associated a software token, and (by the same missing validation) whether or not the username exists in the pool. Failing check ids: cognito-idp.admin-delete-software-token-rejects-unconfigured-and-unknown-user.

AWS: Smithy (com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice) models AdminDeleteSoftwareToken with input {UserPoolId (required, pattern ^[\w-]+_[0-9a-zA-Z]+$), Username (required, sensitive)}, an empty output shape, and the client errors ResourceNotFoundException (httpError 404), UserNotFoundException (httpError 404), InvalidParameterException (400), NotAuthorizedException (403), OperationNotEnabledException (400), UserNotConfirmedException (400), TooManyRequestsException (429). Recorded behavior: when the user exists in the pool but has never completed AssociateSoftwareToken/VerifySoftwareToken, AWS refuses with HTTP 404, error code ResourceNotFoundException and the message "Software token MFA has not been configured for this user." — that whole string is fixed text for this error class, with no variable part. When the username does not exist in the pool at all, AWS refuses with HTTP 404, error code UserNotFoundException and the fixed message "User does not exist." The call succeeds (empty 200 body) only for a user who actually has a SOFTWARE_TOKEN factor, and it then removes that factor, so AdminGetUserAuthFactors no longer lists SOFTWARE_TOKEN.

Emulator: the replay shows the unconfigured-user call returning HTTP 200 with a 2-byte (`{}`) body and no Error member at all — the harness reports `expected a rejection, but the call SUCCEEDED and returned {'ResponseMetadata': {... 'HTTPStatusCode': 200 ... 'content-length': '2' ...}}`. Because the first rejection never happens, the handler is accepting the operation unconditionally; the unknown-username branch is governed by the same absent lookup and was not reached in this replay.

Why: a client cannot distinguish "this user had no TOTP factor" or "this user does not exist" from "the TOTP factor was removed". Code that uses AdminDeleteSoftwareToken to confirm de-enrollment, or that branches on ResourceNotFoundException/UserNotFoundException to decide whether to enroll a factor or to report a bad username, silently takes the success path. It also masks typo'd usernames: an administrator revoking a user's TOTP factor gets a 200 while the real user keeps their authenticator.

Fix: in the AdminDeleteSoftwareToken handler and its validation — resolve the username against the pool's user store first and raise UserNotFoundException (404, "User does not exist.") when absent; then check the resolved user's software-token/MFA state and raise ResourceNotFoundException (404, "Software token MFA has not been configured for this user.") when no verified software token exists; only on the remaining path clear the user's software-token factor in state so AdminGetUserAuthFactors stops reporting SOFTWARE_TOKEN.

Traces: cognito-idp.admin-delete-software-token-rejects-unconfigured-and-unknown-user (error-semantics group; the positive control in the same check asserts the factor is actually removed for an enrolled user).

## Evidence
Check cognito-idp.admin-delete-software-token-rejects-unconfigured-and-unknown-user, [mismatch].

Expected (AWS, recorded): AdminDeleteSoftwareToken(UserPoolId=<pool>, Username=<existing user with no TOTP>) -> HTTP 404, Error.Code="ResourceNotFoundException", Error.Message="Software token MFA has not been configured for this user."; AdminDeleteSoftwareToken(UserPoolId=<pool>, Username=<username never created>) -> HTTP 404, Error.Code="UserNotFoundException", Error.Message="User does not exist."

Actual (emulator): HTTP 200, empty JSON body, no Error member:
  AssertionError: expected a rejection, but the call SUCCEEDED and returned {'ResponseMetadata': {'RequestId': '<req-id>', 'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.1', 'content-length': '2', ...}, 'RetryAttempts': 0}}
(request id, date and generated pool/user names vary per run and are not part of the requirement.)

Smithy: AdminDeleteSoftwareToken errors include ResourceNotFoundException (httpError 404) and UserNotFoundException (httpError 404); output shape AdminDeleteSoftwareTokenResponse is empty, so a successful 200 body of `{}` is correct only on the success path.

## Steps to Reproduce
1. CreateUserPool(PoolName=<name>, DeletionProtection=INACTIVE).
2. SetUserPoolMfaConfig(UserPoolId=<pool>, SoftwareTokenMfaConfiguration={Enabled: true}, MfaConfiguration="OPTIONAL").
3. CreateUserPoolClient(UserPoolId=<pool>, ExplicitAuthFlows=[ALLOW_ADMIN_USER_PASSWORD_AUTH, ALLOW_REFRESH_TOKEN_AUTH], GenerateSecret=false).
4. AdminCreateUser(UserPoolId=<pool>, Username=<user>, MessageAction=SUPPRESS) then AdminSetUserPassword(..., Permanent=true). Do NOT associate a software token.
5. Call AdminDeleteSoftwareToken(UserPoolId=<pool>, Username=<user>) -> emulator answers 200; AWS answers 404 ResourceNotFoundException "Software token MFA has not been configured for this user."
6. Call AdminDeleteSoftwareToken(UserPoolId=<pool>, Username=<a username that was never created>) -> expect 404 UserNotFoundException "User does not exist."
7. Positive control: AdminInitiateAuth(ADMIN_USER_PASSWORD_AUTH) -> AssociateSoftwareToken -> VerifySoftwareToken with a valid RFC 6238 code, confirm AdminGetUserAuthFactors lists SOFTWARE_TOKEN, call AdminDeleteSoftwareToken, and confirm SOFTWARE_TOKEN is gone.
