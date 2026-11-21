Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.id-token-verifies-rs256-against-the-issuer-jwks cognito-idp.access-token-verifies-rs256-under-an-issuer-jwks-key-distinct-from-the-id-tokens cognito-idp.refreshed-id-token-verifies-under-the-same-jwks-and-keeps-the-session-claims cognito-idp.id-token-from-another-pool-rejected-by-this-pools-jwks cognito-idp.openid-configuration-of-a-pool-without-a-domain-names-the-token-issuer-and-endpoints-under-it cognito-idp.openid-discovery-jwks-uri-serves-unique-unpadded-rs256-signing-keys cognito-idp.openid-configuration-for-an-unknown-pool-under-the-issuer-rejected cognito-idp.deep-cognito-idp-2-token-rotation-invalid-config-rejected
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.id-token-verifies-rs256-against-the-issuer-jwks|_get_json#23|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.access-token-verifies-rs256-under-an-issuer-jwks-key-distinct-from-the-id-tokens|_get_json#16|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.refreshed-id-token-verifies-under-the-same-jwks-and-keeps-the-session-claims|_get_json#26|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.id-token-from-another-pool-rejected-by-this-pools-jwks|_get_json#4|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.openid-configuration-of-a-pool-without-a-domain-names-the-token-issuer-and-endpoints-under-it|_get_json#17|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.openid-discovery-jwks-uri-serves-unique-unpadded-rs256-signing-keys|_get_json#14|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.openid-configuration-for-an-unknown-pool-under-the-issuer-rejected|_get#13|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.deep-cognito-idp-2-token-rotation-invalid-config-rejected|_error#6|AssertionError|raise AssertionError("call expected to be refused SUCCEEDED")
Issue-Commit: 9a1cab38181c81438a46747e76c041451acccd46

# cognito-idp: 2 net-new causes — issued tokens' iss claim names cognito-idp.<region>.amazonaws.com (a host the emulator does not serve), so OIDC discovery and JWKS are unreachable; and RefreshTokenRotation together with ALLOW_REFRESH_TOKEN_AUTH is accepted instead of refused

> _Note: this issue was identified against the code base at commit `9a1cab38181c81438a46747e76c041451acccd46`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminInitiateAuth

## Description
Two net-new root causes in cognito-idp.

=== Cause 1 of 2: the issuer the emulator stamps into every issued token is a real-AWS host the emulator does not serve, so neither the OIDC discovery document nor the JWKS document can be fetched from it ===

AWS: a user pool's token issuer is `https://cognito-idp.<region>.amazonaws.com/<userPoolId>`, that issuer is the `iss` claim of both the ID token and the access token, and the issuer itself SERVES the OIDC discovery document at `{iss}/.well-known/openid-configuration` and the signing keys at `{iss}/.well-known/jwks.json`. The discovery document names `issuer` = that same `iss`, `jwks_uri` = `{iss}/.well-known/jwks.json`, and `authorization_endpoint`/`token_endpoint`/`userinfo_endpoint`/`revocation_endpoint`/`end_session_endpoint` = `{iss}/authorize`, `{iss}/token`, `{iss}/userInfo`, `{iss}/revoke`, `{iss}/logout`, with `id_token_signing_alg_values_supported` = ["RS256"]. A request for a pool id that does not exist under the issuer is refused. Reference: amazon-cognito-user-pools-using-tokens-verifying-a-jwt; OpenID Connect Discovery 1.0. Smithy: (Admin)InitiateAuth returns AuthenticationResult{IdToken, AccessToken}; the issuer host and the discovery/JWKS endpoints are recorded behavior, not modelled members.

Emulator: the tokens it issues carry `iss` = `https://cognito-idp.<region>.amazonaws.com/<userPoolId>` — literally the real AWS hostname — while the emulator itself is reachable only on its own host and port. Every attempt to dereference the issuer it published is refused by the harness before a request is even made: `simparity.published.PublishedUrlRejected: endpoint_url was given https://cognito-idp.<region>.amazonaws.com/<poolId>/.well-known/jwks.json (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://<emulator host>:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener` — and the same for `/.well-known/openid-configuration`. This is the emulator announcing an address other than the one it is reached on: nothing the emulator serves is bound to the issuer it stamped into the token. Because `iss` is unreachable, the discovery document, the `jwks_uri` it would name, and every RS256 verification of an ID token, an access token or a refreshed ID token are all unverifiable, and the unknown-pool-under-the-issuer refusal cannot be observed either.

Why: token verification is the whole contract of a user pool. A client that follows AWS's documented procedure — read `iss` from the token, GET `{iss}/.well-known/openid-configuration`, fetch `jwks_uri`, verify RS256 — cannot complete a single step against the emulator; it either reaches the real AWS endpoint (and fails, because the pool does not exist there) or nothing at all. Every downstream consumer that validates a Cognito token by its issuer (API Gateway JWT authorizers, AppSync user-pool auth, ALB authenticate-cognito, any library using the discovery document) is broken by the same published address. Related OPEN issues describe the inverse symptom — 20261109T124858Z-cognito-idp-5-net-new-causes-tokens-are.md and 20261111T061507Z-cognito-idp-3-net-new-causes-the-id.md both state the `iss` claim is "the bare emulator endpoint" — so the address published now is a different value from the one those issues record; neither was reported as owning these checks. 20261108T120220Z-cognito-idp-lambda-cognito-idp-the-user-pool-jwks-document-is.md and 20261024T114943Z-cognito-idp-lambda-the-pool-s-jwks-document.md concern the CONTENT of the JWKS document, not the host it is published on.

Fix: the user-pool issuer/endpoint publication in the token issuance path (the component that computes the `iss` claim for ID and access tokens) together with the OIDC discovery and JWKS listeners — the issuer must be an address the emulator actually serves, and `{iss}/.well-known/openid-configuration` and `{iss}/.well-known/jwks.json` must be routed on it, with the discovery document's issuer/jwks_uri/authorize/token/userInfo/revoke/logout fields all derived from that same published issuer and an unknown pool id under it refused.

Traces (check ids): cognito-idp.id-token-verifies-rs256-against-the-issuer-jwks, cognito-idp.access-token-verifies-rs256-under-an-issuer-jwks-key-distinct-from-the-id-tokens, cognito-idp.refreshed-id-token-verifies-under-the-same-jwks-and-keeps-the-session-claims, cognito-idp.id-token-from-another-pool-rejected-by-this-pools-jwks, cognito-idp.openid-configuration-of-a-pool-without-a-domain-names-the-token-issuer-and-endpoints-under-it, cognito-idp.openid-discovery-jwks-uri-serves-unique-unpadded-rs256-signing-keys, cognito-idp.openid-configuration-for-an-unknown-pool-under-the-issuer-rejected.

=== Cause 2 of 2: CreateUserPoolClient and UpdateUserPoolClient accept RefreshTokenRotation enabled together with ALLOW_REFRESH_TOKEN_AUTH instead of refusing it ===

AWS: an app client may not combine refresh token rotation with the legacy refresh auth flow. A CreateUserPoolClient or UpdateUserPoolClient request carrying both `RefreshTokenRotation={Feature: ENABLED, ...}` and `ALLOW_REFRESH_TOKEN_AUTH` in `ExplicitAuthFlows` is refused 400 `InvalidParameterException` with the message `ALLOW_REFRESH_TOKEN_AUTH is not a permitted ExplicitAuthFlow when refresh token rotation is enabled.` — that sentence is entirely fixed text; its only variable-looking parts (`ALLOW_REFRESH_TOKEN_AUTH`) are the flow name from the request. Each half alone is legal: rotation without the legacy flow is accepted and echoed in UserPoolClient.RefreshTokenRotation, and the legacy flow without rotation is accepted and echoed in ExplicitAuthFlows. A refused UpdATE must leave the client untouched — same ExplicitAuthFlows, same RefreshTokenRotation. Smithy: CreateUserPoolClient/UpdateUserPoolClient both take `ExplicitAuthFlows` (ExplicitAuthFlowsListType) and `RefreshTokenRotation` (RefreshTokenRotationType) and both declare `InvalidParameterException` (httpError 400); the mutual exclusion itself is recorded behavior only — the ExplicitAuthFlows documentation lists ALLOW_REFRESH_TOKEN_AUTH without stating the rotation interaction.

Emulator: the create SUCCEEDS and mints the client. Replay output: `AssertionError: call expected to be refused SUCCEEDED` raised at the CreateUserPoolClient call carrying ExplicitAuthFlows=['ALLOW_USER_PASSWORD_AUTH','ALLOW_REFRESH_TOKEN_AUTH'] and RefreshTokenRotation={'Feature':'ENABLED','RetryGracePeriodSeconds':0}. The validation gate is absent, so neither the error code, the message, nor the "no client left behind" / "refused update changes nothing" effects can hold; the UpdateUserPoolClient leg of the same contract is unreachable behind the failed create.

Why: a client created this way is in a configuration AWS will never produce. Code that enables rotation while leaving the legacy flow in place gets silent acceptance from the emulator and a 400 InvalidParameterException in production — the defect surfaces only at deployment. A caller also cannot rely on the refusal being effect-free, because there is no refusal at all: the emulator leaves a real app client behind.

Fix: CreateUserPoolClient's and UpdateUserPoolClient's input validation (the ExplicitAuthFlows / RefreshTokenRotation consistency check) must refuse the combination with InvalidParameterException and AWS's exact sentence, before any client is created or any field of an existing client is mutated.

Traces (check ids): cognito-idp.deep-cognito-idp-2-token-rotation-invalid-config-rejected.

## Evidence
Cause 1 — published issuer is an address the emulator does not serve:
Expected (AWS): (Admin)InitiateAuth -> IdToken/AccessToken whose `iss` is the pool's issuer, and GET {iss}/.well-known/openid-configuration -> 200 application/json with issuer={iss}, jwks_uri={iss}/.well-known/jwks.json, authorization_endpoint={iss}/authorize, token_endpoint={iss}/token, userinfo_endpoint={iss}/userInfo, revocation_endpoint={iss}/revoke, end_session_endpoint={iss}/logout, id_token_signing_alg_values_supported=["RS256"]; GET {iss}/.well-known/jwks.json -> 200 with the RS256 signing keys that verify the issued tokens.
Actual (emulator): `iss` = https://cognito-idp.<region>.amazonaws.com/<userPoolId>, a host the emulator does not serve. Dereferencing it is refused:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://cognito-idp.us-west-2.amazonaws.com/<poolId>/.well-known/jwks.json (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
  ... identically for https://cognito-idp.us-west-2.amazonaws.com/<poolId>/.well-known/openid-configuration
Seven checks fail on exactly this: id-token-verifies-rs256-against-the-issuer-jwks, access-token-verifies-rs256-under-an-issuer-jwks-key-distinct-from-the-id-tokens, refreshed-id-token-verifies-under-the-same-jwks-and-keeps-the-session-claims, id-token-from-another-pool-rejected-by-this-pools-jwks, openid-configuration-of-a-pool-without-a-domain-names-the-token-issuer-and-endpoints-under-it, openid-discovery-jwks-uri-serves-unique-unpadded-rs256-signing-keys, openid-configuration-for-an-unknown-pool-under-the-issuer-rejected. (Region, pool id, host and port vary per run; the real-AWS host in `iss` is the requirement violated.)

Cause 2 — rotation + legacy refresh flow accepted:
Expected (AWS): CreateUserPoolClient(UserPoolId, ClientName, ExplicitAuthFlows=['ALLOW_USER_PASSWORD_AUTH','ALLOW_REFRESH_TOKEN_AUTH'], RefreshTokenRotation={'Feature':'ENABLED','RetryGracePeriodSeconds':0}) -> HTTP 400, Error.Code 'InvalidParameterException', Error.Message 'ALLOW_REFRESH_TOKEN_AUTH is not a permitted ExplicitAuthFlow when refresh token rotation is enabled.' and no client created (ListUserPoolClients does not show the name). The same request shape on UpdateUserPoolClient -> same 400/code/message, and DescribeUserPoolClient still reports the pre-call ExplicitAuthFlows and RefreshTokenRotation.
Actual (emulator): the call returns 200 and mints the client. Replay detail: AssertionError: call expected to be refused SUCCEEDED (raised from the CreateUserPoolClient leg).

## Steps to Reproduce
Cause 1:
1. CreateUserPool (no domain) + CreateUserPoolClient(ExplicitAuthFlows=['ALLOW_ADMIN_USER_PASSWORD_AUTH','ALLOW_REFRESH_TOKEN_AUTH'], GenerateSecret=False).
2. AdminCreateUser + AdminSetUserPassword(Permanent=True); AdminInitiateAuth(AuthFlow='ADMIN_USER_PASSWORD_AUTH').
3. Base64url-decode the IdToken payload and read `iss`. Observed: https://cognito-idp.<region>.amazonaws.com/<userPoolId>.
4. GET {iss}/.well-known/openid-configuration and GET {iss}/.well-known/jwks.json.
   Expected: the emulator serves both on an address it is reached on. Actual: the published host is cognito-idp.<region>.amazonaws.com, which the emulator does not serve, so the fetch is refused before it is issued (PublishedUrlRejected).
5. Repeat after InitiateAuth(REFRESH_TOKEN_AUTH) for the refreshed ID token, and with a second pool's token, to see the same unreachable issuer.

Cause 2:
1. CreateUserPool.
2. CreateUserPoolClient(UserPoolId=<pool>, ClientName=<name>, ExplicitAuthFlows=['ALLOW_USER_PASSWORD_AUTH','ALLOW_REFRESH_TOKEN_AUTH'], RefreshTokenRotation={'Feature':'ENABLED','RetryGracePeriodSeconds':0}).
   Expected: 400 InvalidParameterException 'ALLOW_REFRESH_TOKEN_AUTH is not a permitted ExplicitAuthFlow when refresh token rotation is enabled.'; Actual: 200 and a client is created.
3. Controls: CreateUserPoolClient with RefreshTokenRotation but ExplicitAuthFlows=['ALLOW_USER_PASSWORD_AUTH'] -> accepted, RefreshTokenRotation echoed; CreateUserPoolClient with ALLOW_REFRESH_TOKEN_AUTH and no RefreshTokenRotation -> accepted.
4. UpdateUserPoolClient on the rotation-only client, adding 'ALLOW_REFRESH_TOKEN_AUTH' while keeping RefreshTokenRotation enabled.
   Expected: same 400/code/message, and DescribeUserPoolClient unchanged (no ALLOW_REFRESH_TOKEN_AUTH, RefreshTokenRotation intact).
