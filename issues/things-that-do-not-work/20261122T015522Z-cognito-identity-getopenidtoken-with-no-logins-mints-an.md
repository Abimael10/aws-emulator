Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-identity.deep-cognito-identity-2-disabled-identity-merge-destination-lookup-rejected-old-token-still-vends
Issue-Shape-Id: com.amazonaws.cognitoidentity#AWSCognitoIdentityService
Issue-Key: cognito-identity.deep-cognito-identity-2-disabled-identity-merge-destination-lookup-rejected-old-token-still-vends|_err#42|AssertionError|raise AssertionError(f"expected this call to be refused, but it SUCCEEDED and returned {result!r}")
Issue-Commit: 9646fdd566ff3c090b230e575cd06b10ee53c1e6

# cognito-identity: GetOpenIdToken with no Logins mints an unauthenticated token for a developer-provider identity instead of refusing NotAuthorizedException "Access to Identity '<id>' is forbidden."

> _Note: this issue was identified against the code base at commit `9646fdd566ff3c090b230e575cd06b10ee53c1e6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetOpenIdToken

## Description
One net-new root cause on the cognito-identity token-vending path.

Cause 1 of 1 — GetOpenIdToken called with only an IdentityId (no Logins) for an identity that was created through a developer provider, in a pool whose AllowUnauthenticatedIdentities is false, is satisfied with a token instead of being refused.

AWS: the call is refused as an authorization failure. Smithy (services/service-com-amazonaws-cognitoidentity-awscognitoidentityservice/smithy-model/com-amazonaws-cognitoidentity-awscognitoidentityservice.json, awsJson1_1): GetOpenIdToken input = IdentityId (required, @length 1..55, @pattern ^[\w-]+:[0-9a-f-]+$) and Logins (LoginsMap, @length 0..10, optional); modeled errors = ExternalServiceException(400), InvalidParameterException(400), NotAuthorizedException(@httpError 403), ResourceConflictException(409), ResourceNotFoundException(404), TooManyRequestsException(429), InternalErrorException(500); the operation is @optionalAuth. The model does not state which error covers an identity the caller has not proven, so the class/message/status pairing is recorded behavior only: the refusal is error code NotAuthorizedException with the message "Access to Identity '<identityId>' is forbidden." — fixed text "Access to Identity '" + the IdentityId taken verbatim from the request + "' is forbidden." is the only variable part. The refusal is identical for the merge survivor and for the merge-disabled destination identity, i.e. plain GetOpenIdToken carries no disabled/enabled signal: an identity whose only logins are developer-provider logins can never be vended an OpenID token without proving a login, and a pool that disallows unauthenticated identities can never vend an unauthenticated one.

Emulator: HTTP 200 with a success envelope — {'IdentityId': '<the requested id>', 'Token': '<jwt>'} (observed content-type application/x-amz-json-1.1). The minted token's claims carry "amr": ["unauthenticated"] and "sub" equal to the requested identity id, for a pool created with AllowUnauthenticatedIdentities=False whose identities exist only as developer-provider links. No __type, no message and no x-amzn-errortype header are emitted at all.

Why: this is the over-permissive direction and it opens the federation trust boundary. A caller that holds nothing but an identity id — which is not a secret and is returned by GetId, DescribeIdentity, LookupDeveloperIdentity and MergeDeveloperIdentities — can mint an OpenID token for that identity and carry it to STS AssumeRoleWithWebIdentity, so possession of an id becomes possession of the identity's session. Clients also lose the two invariants AWS gives them: a developer-authenticated pool cannot be driven down the unauthenticated path, and a pool with AllowUnauthenticatedIdentities=false vends no guest tokens. Every negative test asserting this refusal passes against AWS and fails only here.

Fix: the authorization check in the GetOpenIdToken handler of the cognito-identity service crate — before a token is minted, resolve the named identity's existing logins and require the request's Logins to prove one of them; refuse NotAuthorizedException with AWS's message when the request proves none, and additionally refuse the unauthenticated (empty-Logins) path when the pool's AllowUnauthenticatedIdentities is false. The x-amzn-errortype header follows from the error class in the awsJson1_1 error serializer.

Related open cognito-identity issues reviewed that do NOT cover this call and are not recorded as owning this check: 20261029T173827Z-cognito-identity-10-net-new-token-credential-refusal.md (its causes concern the OIDC token's signature, GetId/GetCredentialsForIdentity login validation, unsigned-request mapping and refusal messages, not GetOpenIdToken's ownership check), 20261113T131245Z-cognito-identity-developer-identity-ownership-refusals-are-mis.md and 20261120T185720Z-cognito-identity-login-conflict-and-login-token-refusals.md (GetOpenIdTokenForDeveloperIdentity / LookupDeveloperIdentity / GetCredentialsForIdentity refusals), 20261029T230931Z-cognito-identity-identity-merge-and-login-list-state.md (merge/login-list state, which this check's earlier assertions already pass).

Traces: cognito-identity.deep-cognito-identity-2-disabled-identity-merge-destination-lookup-rejected-old-token-still-vends.

## Evidence
Behavioral suite replay (312/341 passed; this check [mismatch]). Check cognito-identity.deep-cognito-identity-2-disabled-identity-merge-destination-lookup-rejected-old-token-still-vends reaches the GetOpenIdToken assertions after its pool/role/merge setup and all earlier assertions pass.

Expected (AWS): ClientError with Error.Code == 'NotAuthorizedException' and Error.Message == "Access to Identity '<identityId>' is forbidden." for both the merge-disabled destination identity and the surviving identity.

Actual (emulator): no error at all. The harness helper raised:
  AssertionError: expected this call to be refused, but it SUCCEEDED and returned {'IdentityId': 'us-west-2:bb0373aa-24b6-4e6c-844c-7f2034b1afb8', 'Token': 'eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJpc3MiOiJodHRwczovL2NvZ25pdG8taWRlbnRpdHkuYW1hem9uYXdzLmNvbSIsImF1ZCI6InVzLXdlc3QtMjoyYmMxYjBmNS0xODM0LTQ1ZmUtYmI3Yi1kYTgyZTZkMGMwMGMiLCJzdWIiOiJ1cy13ZXN0LTI6YmIwMzczYWEtMjRiNi00ZTZjLTg0NGMtN2YyMDM0YjFhZmI4IiwiYW1yIjpbInVuYXV0aGVudGljYXRlZCJdLCJpYXQiOi4uLiwiZXhwIjouLi4sImp0aSI6Ii4uLiJ9.', ...} with HTTPStatusCode 200
(the identity id, pool id, jti and timestamps above are this run's values; the requirement is the 200-with-token response where a refusal is required, and the decoded claims showing amr ["unauthenticated"] for a pool created with AllowUnauthenticatedIdentities=False).

Smithy citation: GetOpenIdToken models NotAuthorizedException (client, @httpError 403) among its errors; Logins is optional (LoginsMap @length 0..10) and the operation is @optionalAuth, so the ownership decision is the handler's, not the auth layer's.

## Steps to Reproduce
1. Boot the emulator and replay the behavioral check cognito-identity.deep-cognito-identity-2-disabled-identity-merge-destination-lookup-rejected-old-token-still-vends.
2. By hand: CreateIdentityPool with AllowUnauthenticatedIdentities=false and a DeveloperProviderName; SetIdentityPoolRoles with an authenticated role.
3. Call GetOpenIdTokenForDeveloperIdentity twice (two distinct developer user identifiers) to obtain two identity ids, then MergeDeveloperIdentities(SourceUserIdentifier=<src user>, DestinationUserIdentifier=<dst user>, DeveloperProviderName=<provider>, IdentityPoolId=<pool>).
4. For EACH of the two identity ids (the merge-disabled one and the survivor) call GetOpenIdToken(IdentityId=<that id>) with NO Logins.
5. Expected: a refusal with error code NotAuthorizedException and message "Access to Identity '<that identity id>' is forbidden." (and the awsJson1_1 x-amzn-errortype header carrying that class). Observed: HTTP 200 with an IdentityId/Token body whose token claims carry amr ["unauthenticated"].
6. Compare the HTTP status, the body __type/message and the x-amzn-errortype header against the refusal above.
