Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-identity.deep-cognito-identity-2-disabled-identity-merge-destination-lookup-rejected-old-token-still-vends
Issue-Shape-Id: com.amazonaws.cognitoidentity#AWSCognitoIdentityService
Issue-Key: cognito-identity|GetOpenIdTokenForDeveloperIdentity|NotAuthorizedException
Issue-Commit: 551556482864271bf2a94d2347709669640f8cae

# cognito-identity: REOPENS — GetOpenIdTokenForDeveloperIdentity naming a merge-disabled identity is still refused NotAuthorizedException "Logins don't match" instead of re-routing to the merge survivor

> _Note: this issue was identified against the code base at commit `551556482864271bf2a94d2347709669640f8cae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetOpenIdTokenForDeveloperIdentity

## Description
REOPENS 20261030T233921Z-cognito-identity-getopenidtokenfordeveloperidentity-refuses-two-link-flows-aws.md: its fix left the call refused.

That issue's Cause 1 required GetOpenIdTokenForDeveloperIdentity to RESOLVE a supplied developer login to its canonical identity instead of validating the request's explicit IdentityId against it, and to reserve the logins-don't-match refusal for a Logins map carrying no login valid for the pool/identity. The two checks it named now pass, but the same guard still fires on the post-merge form of exactly that flow: an explicit IdentityId that names the identity MergeDeveloperIdentities disabled, with a developer login that is valid for the pool and now canonical to the survivor.

This issue carries 1 root cause.

Cause 1 of 1: GetOpenIdTokenForDeveloperIdentity(IdentityPoolId=<pool>, IdentityId=<merge-disabled identity>, Logins={<DeveloperProviderName>: <developer user>}) after MergeDeveloperIdentities.

AWS: recorded behavior only for the re-routing rule — the model (com.amazonaws.cognitoidentity#AWSCognitoIdentityService, awsJson1_1) marks GetOpenIdTokenForDeveloperIdentity.IdentityId OPTIONAL (@length 1..55, pattern ^[\w-]+:[0-9a-f-]+$), Logins required (LoginsMap @length 0..10), output IdentityId + Token, errors DeveloperUserAlreadyRegisteredException(400), InvalidParameterException(400), NotAuthorizedException(@httpError 403), ResourceConflictException(409), ResourceNotFoundException(404), TooManyRequestsException(429), InternalErrorException(500); the operation's own documentation states the existing IdentityId is the way to associate a login with an existing identity, i.e. the login is canonical. After a merge of two developer users, the surviving identity owns BOTH developer user identifiers and the merge's losing (destination) identity is retained but disabled (DescribeIdentity still answers for it, with Logins exactly ['DISABLED', <pool region>]). A later GetOpenIdTokenForDeveloperIdentity that names the DISABLED identity together with either merged developer user answers HTTP 200 and returns the SURVIVOR's IdentityId plus a Token — the supplied IdentityId is silently overridden by the identity the developer login actually resolves to. The disabled identity is not an authorization failure: LookupDeveloperIdentity on it is refused NotAuthorizedException 'You do not have access to this identity' (fixed text), while its own pre-merge OpenID token keeps vending through GetCredentialsForIdentity and keeps being accepted by STS AssumeRoleWithWebIdentity.

Emulator: the call is refused with NotAuthorizedException and the message "Logins don't match. Please include at least one valid login for this identity or identity pool." (entirely fixed text — it has no variable parts drawn from the request or state; this is AWS's wording for a Logins map that proves nothing for the pool, which this request is not: the map carries the pool's configured DeveloperProviderName and a developer user identifier registered in that pool). The replayed check aborts there with botocore.errorfactory.NotAuthorizedException on GetOpenIdTokenForDeveloperIdentity, so no token and no IdentityId are returned. The emulator is deciding ownership by comparing the request's IdentityId to the login's binding and treating the merge-disabled identity as a non-matching login, instead of resolving the login to the surviving identity.

Why: this is the ordinary re-login path for a developer user whose identity was merged. Every client (including the SDK Cognito credential providers) caches an IdentityId and replays it with the developer login; once a merge has happened that cached id is the disabled one, and on AWS the next call transparently returns the survivor. On the emulator the federation flow is refused with an authorization error, so the client re-authenticates in a loop against a login that is in fact valid and never obtains credentials for the merged identity. Post-merge behavior (re-routing, the survivor's two developer users, the disabled identity's still-usable pre-merge token) becomes unreachable and untestable.

Fix: the explicit-IdentityId / logins-vs-identity ownership branch of the GetOpenIdTokenForDeveloperIdentity handler in the cognito-identity service — resolve each supplied developer login to its canonical identity (following a merge's disabled-identity -> survivor redirect recorded in identity state) and return 200 with that identity; raise the logins-don't-match NotAuthorizedException only when the Logins map carries no login valid for the pool or for the named identity.

Traces: cognito-identity.deep-cognito-identity-2-disabled-identity-merge-destination-lookup-rejected-old-token-still-vends.

Open issues reviewed and NOT covering this cause (none carries this check): 20261029T230931Z-cognito-identity-identity-merge-and-login-list-state.md (merge/unlink login-list bookkeeping and ListIdentities projections, wire corpus — not the post-merge re-route refusal), 20261113T131245Z-cognito-identity-developer-identity-ownership-refusals-are-mis.md (the opposite direction: a login bound to a DIFFERENT, non-merged identity must be refused), 20261108T145123Z-cognito-identity-getopenidtokenfordeveloperidentity-mints-a-developer-token-on.md (role-mapping pool should refuse the mint).

## Evidence
Behavioral replay, simparity-behavioral-aws-v1 (emulator up and serving: 306/340 checks passed; cognito-identity 260/281).

Check cognito-identity.deep-cognito-identity-2-disabled-identity-merge-destination-lookup-rejected-old-token-still-vends [error]:
  botocore.errorfactory.NotAuthorizedException: An error occurred (NotAuthorizedException) when calling the GetOpenIdTokenForDeveloperIdentity operation: Logins don't match. Please include at least one valid login for this identity or identity pool.

Sequence exercised by the check: CreateIdentityPool(AllowUnauthenticatedIdentities=false, DeveloperProviderName=<dp>) -> GetOpenIdTokenForDeveloperIdentity(Logins={<dp>: <user_dst>}) = identity DST (+Token) -> GetOpenIdTokenForDeveloperIdentity(Logins={<dp>: <user_src>}) = identity SRC (+Token) -> MergeDeveloperIdentities(SourceUserIdentifier=<user_src>, DestinationUserIdentifier=<user_dst>, DeveloperProviderName=<dp>, IdentityPoolId=<pool>) -> for each merged developer user: GetOpenIdTokenForDeveloperIdentity(IdentityId=<disabled DST>, Logins={<dp>: <user>}).

Expected (AWS, recorded): HTTP 200 with IdentityId == the surviving identity and a Token, for BOTH the destination user and the source user; DescribeIdentity(<disabled>) still answers with Logins ['DISABLED', <region>]; LookupDeveloperIdentity(<disabled>) refused 400 NotAuthorizedException 'You do not have access to this identity'; GetCredentialsForIdentity with the disabled identity's own pre-merge token still vends, and STS AssumeRoleWithWebIdentity still accepts that token with SubjectFromWebIdentityToken == the disabled identity.
Actual (emulator): the GetOpenIdTokenForDeveloperIdentity call naming the disabled identity is refused NotAuthorizedException with the fixed text "Logins don't match. Please include at least one valid login for this identity or identity pool." (no variable parts), so no IdentityId/Token is returned and the remaining post-merge assertions are never reached.

Smithy cited (services/service-com-amazonaws-cognitoidentity-awscognitoidentityservice): GetOpenIdTokenForDeveloperIdentity.IdentityId optional @length 1..55 pattern ^[\w-]+:[0-9a-f-]+$; Logins required LoginsMap @length 0..10; output IdentityId + Token (Token sensitive); NotAuthorizedException @httpError 403, ResourceConflictException @httpError 409; MergeDeveloperIdentities output = IdentityId (the survivor). Which error class covers a login/identity ownership mismatch, the recorded 400 wire status for these refusals, and the DISABLED/region marker are recorded behavior only.

Note: identity ids, pool ids, developer provider names, developer user identifiers, role names and session names in the sequence above are per-run environment values, not requirements; the requirements are the 200 status, that the returned IdentityId is the merge survivor, and the error class/message reserved for a genuinely invalid Logins map.

## Steps to Reproduce
1. Boot the emulator and replay cognito-identity.deep-cognito-identity-2-disabled-identity-merge-destination-lookup-rejected-old-token-still-vends.
2. By hand: CreateIdentityPool(IdentityPoolName=<name>, AllowUnauthenticatedIdentities=false, DeveloperProviderName=dp).
3. GetOpenIdTokenForDeveloperIdentity(IdentityPoolId=pool, Logins={dp: userDst}) -> identity DST; GetOpenIdTokenForDeveloperIdentity(IdentityPoolId=pool, Logins={dp: userSrc}) -> identity SRC (DST != SRC).
4. MergeDeveloperIdentities(SourceUserIdentifier=userSrc, DestinationUserIdentifier=userDst, DeveloperProviderName=dp, IdentityPoolId=pool). Confirm DescribeIdentity(DST).Logins == ['DISABLED', <pool region>] and DescribeIdentity(SRC).Logins carries dp once per merged user, and that the response IdentityId is the survivor SRC.
5. Call GetOpenIdTokenForDeveloperIdentity(IdentityPoolId=pool, IdentityId=DST, Logins={dp: userDst}) and again with Logins={dp: userSrc}. Each must answer HTTP 200 with IdentityId == SRC and a Token; the emulator answers NotAuthorizedException "Logins don't match. Please include at least one valid login for this identity or identity pool."
6. Compare HTTP status, body __type / x-amzn-errortype header and message against those expectations.
