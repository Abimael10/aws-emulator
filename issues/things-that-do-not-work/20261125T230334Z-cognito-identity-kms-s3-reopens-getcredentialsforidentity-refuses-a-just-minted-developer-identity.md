Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-identity__kms__s3.xsvc-cognito-identity-1-encryption-context-scoped-key-grant-denied-vended-session-outside-its-prefix cognito-identity__kms__s3.xsvc-cognito-identity-1-sse-kms-get-object-denied-to-vended-session-until-key-policy-grants-role
Issue-Key: cognito-identity|GetCredentialsForIdentity|NotAuthorizedException
Issue-Commit: 50eec8fda4c6238e3cbd0bab35b991aa1443e257

# REOPENS: GetCredentialsForIdentity refuses a just-minted developer-identity OpenID token with NotAuthorizedException "Invalid login token. Not a Cognito token.", so no Cognito-vended session can be obtained

> _Note: this issue was identified against the code base at commit `50eec8fda4c6238e3cbd0bab35b991aa1443e257`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCredentialsForIdentity

## Description
REOPENS 20261116T235533Z-cognito-identity-getcredentialsforidentity-applies-the-customrolearn-eligibility-rule--r3.md: its fix left the call refused. That issue asked GetCredentialsForIdentity to validate each Logins entry's token BEFORE applying the CustomRoleArn eligibility rule and to raise NotAuthorizedException with the fixed message "Invalid login token. Not a Cognito token." for a value that is not a Cognito-issued OpenID token. The token check now fires for a token the emulator itself just minted, so the whole developer-authenticated vending path is refused. (The same refusal also bears on fixed issue 20261110T092801Z-cognito-identity-a-just-minted-developer-identity-openid.md, which covered a just-minted developer-identity token being refused by this operation — there as expired, here as not-a-Cognito-token.)

Cause 1 of 1: a Logins value produced by GetOpenIdTokenForDeveloperIdentity is rejected by GetCredentialsForIdentity as not a Cognito token.

AWS: the developer-authenticated-identity flow is: GetOpenIdTokenForDeveloperIdentity returns {IdentityId, Token} and the caller passes that SAME Token straight back as the value of GetCredentialsForIdentity's Logins entry keyed "cognito-identity.amazonaws.com"; AWS answers HTTP 200 with Credentials (AccessKeyId, SecretKey, SessionToken, Expiration) for the pool's authenticated role. Smithy (com.amazonaws.cognitoidentity#AWSCognitoIdentityService, awsJson1_1): GetOpenIdTokenForDeveloperIdentity output member Token targets OIDCToken and is @sensitive; GetCredentialsForIdentity input is IdentityId (required, @length 1..55, @pattern ^[\w-]+:[0-9a-f-]+$), Logins (LoginsMap, @length 0..10) and CustomRoleArn (@length 20..2048), output is {IdentityId, Credentials}, and its modeled errors are ExternalServiceException(400), InternalErrorException(500), InvalidIdentityPoolConfigurationException(400), InvalidParameterException(400), NotAuthorizedException(client, @httpError 403, "Thrown when a user is not authorized to access the requested resource"), ResourceConflictException(409), ResourceNotFoundException(404), TooManyRequestsException(429). The model does not say which Logins values are acceptable; that a VALID, unexpired, pool-issued developer-identity token must be accepted, and that the sentence "Invalid login token. Not a Cognito token." is reserved for a Logins value that is not a Cognito-issued OpenID token, is recorded behavior. That message is entirely fixed text — no identity id, pool id, provider name or member name is interpolated into it.

Emulator: every GetCredentialsForIdentity call in this flow is refused NotAuthorizedException with message "Invalid login token. Not a Cognito token.", for a token minted seconds earlier by the emulator's own GetOpenIdTokenForDeveloperIdentity against the same pool with TokenDuration=900 and presented under the key "cognito-identity.amazonaws.com" for the identity id that mint returned. No Credentials envelope is ever produced, so nothing downstream of the vend runs.

Why: a client relying on AWS's behavior cannot obtain a federated session at all on the developer-authenticated path. NotAuthorizedException is the "re-authenticate, your login token is stale/forged/from another issuer" signal, so application code refreshes the login and retries — against the emulator that retry mints another token that is refused the same way, an unbounded loop. Every scenario that needs a Cognito-vended session (SSE-KMS reads under an encryption-context-conditioned key grant, prefix-scoped IAM denials, the audit of what that session did) is unreachable, and a positive control written to prove the session CAN read is never even attempted.

Fix: the Logins token validation stage of the GetCredentialsForIdentity handler — recognise a token this service's own GetOpenIdTokenForDeveloperIdentity issued (issuer/shape/pool binding and the "cognito-identity.amazonaws.com" login key) as a Cognito token and continue to role resolution, keeping the "Invalid login token. Not a Cognito token." refusal for values that genuinely are not pool-issued OpenID tokens, so the r3 fix's ordering is preserved without swallowing the valid case.

Traces: cognito-identity__kms__s3.xsvc-cognito-identity-1-encryption-context-scoped-key-grant-denied-vended-session-outside-its-prefix and cognito-identity__kms__s3.xsvc-cognito-identity-1-sse-kms-get-object-denied-to-vended-session-until-key-policy-grants-role, both [error] on the first GetCredentialsForIdentity call.

Open issues the replay report does NOT record as owning these checks, and that do not cover this: 20261120T185720Z-cognito-identity-login-conflict-and-login-token-refusals.md Cause 2 covers the OPPOSITE mis-classification (this message missing where it belongs, on a genuinely non-Cognito token under an ineligible CustomRoleArn); 20261124T070730Z-cognito-identity-3-net-new-causes-a-user.md, 20261125T014344Z-cognito-identity-getcredentialsforidentity-vends-a-credentials-accesskeyid-that.md and 20261028T221339Z-cognito-identity-3-net-new-getcredentialsforidentity-root-causes.md all describe the CONTENT of a vended session (key-id class, role mapping), which presupposes a vend that succeeds; 20261113T131245Z and 20261122T015522Z cover other developer-identity operations.

## Evidence
Behavioral suite replay against the emulator built from this checkout: 2331/2681 passed; both cognito-identity__kms__s3 checks errored on the same call.

Expected (AWS): GetCredentialsForIdentity(IdentityId=<id from GetOpenIdTokenForDeveloperIdentity>, Logins={"cognito-identity.amazonaws.com": <Token from that same mint>}) -> HTTP 200 with a Credentials member carrying AccessKeyId, SecretKey, SessionToken and Expiration for the identity pool's authenticated role.

Actual (emulator), identical for both checks:
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.NotAuthorizedException: An error occurred (NotAuthorizedException) when calling the GetCredentialsForIdentity operation: Invalid login token. Not a Cognito token.

The refused token was obtained in the immediately preceding call, ci.get_open_id_token_for_developer_identity(IdentityPoolId=<pool>, Logins={<developerProviderName>: <user>}, TokenDuration=900), which returned 200 with IdentityId and Token; the pool was created in the same check with AllowUnauthenticatedIdentities=False and that DeveloperProviderName, and SetIdentityPoolRoles had bound an authenticated role trusting cognito-identity.amazonaws.com with cognito-identity.amazonaws.com:aud = the pool id.

Message classification: "Invalid login token. Not a Cognito token." is wholly fixed text; it has no variable parts, so none of it is a per-run value. The divergence is that this fixed refusal is emitted at all for a pool-issued token (AWS answers 200 here), not any difference in its wording.

## Steps to Reproduce
1. Create an identity pool: CreateIdentityPool(IdentityPoolName=<name>, AllowUnauthenticatedIdentities=False, DeveloperProviderName=<devProvider>).
2. Create an IAM role whose trust policy allows sts:AssumeRoleWithWebIdentity for Federated principal cognito-identity.amazonaws.com with StringEquals cognito-identity.amazonaws.com:aud = the pool id and ForAnyValue:StringLike cognito-identity.amazonaws.com:amr = authenticated; bind it with SetIdentityPoolRoles(Roles={"authenticated": <roleArn>}).
3. Mint a developer identity: GetOpenIdTokenForDeveloperIdentity(IdentityPoolId=<pool>, Logins={<devProvider>: <anyUserIdentifier>}, TokenDuration=900). Keep the returned IdentityId and Token.
4. Call GetCredentialsForIdentity(IdentityId=<that IdentityId>, Logins={"cognito-identity.amazonaws.com": <that Token>}).
5. AWS answers 200 with a Credentials envelope for the authenticated role. The emulator answers NotAuthorizedException "Invalid login token. Not a Cognito token." No CustomRoleArn is sent, so the CustomRoleArn eligibility rule is not involved.
6. Re-replay to confirm: behavioral checks cognito-identity__kms__s3.xsvc-cognito-identity-1-encryption-context-scoped-key-grant-denied-vended-session-outside-its-prefix and cognito-identity__kms__s3.xsvc-cognito-identity-1-sse-kms-get-object-denied-to-vended-session-until-key-policy-grants-role.
