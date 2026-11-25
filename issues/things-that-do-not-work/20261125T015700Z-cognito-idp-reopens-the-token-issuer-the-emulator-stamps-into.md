Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.id-token-verifies-rs256-against-the-issuer-jwks cognito-idp.access-token-verifies-rs256-under-an-issuer-jwks-key-distinct-from-the-id-tokens cognito-idp.refreshed-id-token-verifies-under-the-same-jwks-and-keeps-the-session-claims cognito-idp.id-token-from-another-pool-rejected-by-this-pools-jwks cognito-idp.openid-configuration-of-a-pool-without-a-domain-names-the-token-issuer-and-endpoints-under-it cognito-idp.openid-discovery-jwks-uri-serves-unique-unpadded-rs256-signing-keys cognito-idp.openid-configuration-for-an-unknown-pool-under-the-issuer-rejected cognito-idp.refresh-token-auth-for-a-disabled-user-rejected cognito-idp.refresh-token-auth-under-another-client-rejected
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.id-token-verifies-rs256-against-the-issuer-jwks|_get_json#23|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.access-token-verifies-rs256-under-an-issuer-jwks-key-distinct-from-the-id-tokens|_get_json#16|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.refreshed-id-token-verifies-under-the-same-jwks-and-keeps-the-session-claims|_get_json#26|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.id-token-from-another-pool-rejected-by-this-pools-jwks|_get_json#4|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.openid-configuration-of-a-pool-without-a-domain-names-the-token-issuer-and-endpoints-under-it|_get_json#17|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.openid-discovery-jwks-uri-serves-unique-unpadded-rs256-signing-keys|_get_json#14|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.openid-configuration-for-an-unknown-pool-under-the-issuer-rejected|_get#13|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.refresh-token-auth-for-a-disabled-user-rejected|_get_json#20|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Key: cognito-idp.refresh-token-auth-under-another-client-rejected|_get_json#20|PublishedUrlRejected|target, headers = ctx.endpoint_route(url)
Issue-Commit: 7a03c6c633814b42753ce8c8f03d4e7bdff82ea0

# REOPENS: the token issuer the emulator stamps into every Cognito token (cognito-idp.<region>.localhost:<port>) is still not a host the emulator serves, so OIDC discovery and JWKS remain unreachable

> _Note: this issue was identified against the code base at commit `7a03c6c633814b42753ce8c8f03d4e7bdff82ea0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InitiateAuth

## Description
REOPENS 20261114T130559Z-cognito-idp-2-net-new-causes-issued-tokens.md: its fix left the call refused.

Cause 1 of 1: the issuer the emulator publishes in the `iss` claim of every issued token is a host name the emulator does not serve, so neither the OIDC discovery document nor the JWKS document can be dereferenced from the issuer the token names.

AWS: a user pool's token issuer is `https://cognito-idp.<region>.<domain>/<userPoolId>`; that value is the `iss` claim of the ID token and the access token, and the issuer itself SERVES `{iss}/.well-known/openid-configuration` and `{iss}/.well-known/jwks.json`. The discovery document names issuer={iss}, jwks_uri={iss}/.well-known/jwks.json and the authorize/token/userInfo/revoke/logout endpoints under {iss}, with id_token_signing_alg_values_supported=["RS256"]; the JWKS serves the RS256 keys that verify the issued tokens, one distinct key set per pool, and a pool id that does not exist under the issuer is refused. Reference: "Verifying a JSON Web Token" (amazon-cognito-user-pools-using-tokens-verifying-a-jwt) and OpenID Connect Discovery 1.0. Smithy: InitiateAuth/AdminInitiateAuth return AuthenticationResult{IdToken, AccessToken, RefreshToken}; the issuer host and the two well-known documents are recorded behavior only — they are not modelled members. For an emulator the requirement is reachability: a value the emulator publishes as an address must be an address the emulator is reached on (the endpoint under test, or a host it announces as its own on a per-resource listener).

Emulator: the `iss` claim is now `https://cognito-idp.<region>.localhost:<port>/<userPoolId>` — the port is the emulator's own listener port, but the host label `cognito-idp.<region>.localhost` is not one of the emulator's own hosts, so every attempt to dereference the published issuer is refused before a request is sent: `simparity.published.PublishedUrlRejected: endpoint_url was given https://cognito-idp.<region>.localhost:<port>/<poolId>/.well-known/jwks.json (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener` — identically for `/.well-known/openid-configuration`. No HTTP status, error code or body exists, because nothing is sent. The predecessor issue recorded the same refusal with the host `cognito-idp.<region>.amazonaws.com`; the host form changed, the defect (an announced address the emulator does not serve) did not.

Why: token verification is the whole contract of a user pool. A client that follows AWS's documented procedure — read `iss`, GET `{iss}/.well-known/openid-configuration`, fetch `jwks_uri`, verify RS256 — cannot complete one step against the emulator. Nothing downstream that validates a Cognito token by its issuer (API Gateway JWT authorizers, AppSync user-pool auth, ALB authenticate-cognito, any OIDC library) can work, and the negative behaviours that depend on discovery — a token from pool A rejected under pool B's JWKS, a refused refresh for a disabled user or under another app client, the unknown-pool-under-the-issuer refusal — are unobservable. Related: the OPEN issue 20261124T203931Z-apigateway-cognito-idp-lambda-the-cognito-id-token--r2.md records this same published host under the apigateway__cognito-idp__lambda heading, and 20261122T054242Z-cognito-idp-3-net-new-causes-id-token.md / 20261111T061507Z-cognito-idp-3-net-new-causes-the-id.md record an earlier value ("the bare emulator endpoint"); none of them carries these cognito-idp check keys.

Fix: the issuer publication in the token-issuance path of the cognito-idp crate (the component that computes the `iss` claim for ID and access tokens) together with the OIDC discovery and JWKS routing — the published issuer must be an address the emulator serves (either the endpoint under test or a per-resource listener host registered as the emulator's own), `{iss}/.well-known/openid-configuration` and `{iss}/.well-known/jwks.json` must be routed on it, the discovery document's issuer/jwks_uri/authorize/token/userInfo/revoke/logout must all derive from that same published issuer, and an unknown pool id under it must be refused.

Traces (check ids): cognito-idp.id-token-verifies-rs256-against-the-issuer-jwks, cognito-idp.access-token-verifies-rs256-under-an-issuer-jwks-key-distinct-from-the-id-tokens, cognito-idp.refreshed-id-token-verifies-under-the-same-jwks-and-keeps-the-session-claims, cognito-idp.id-token-from-another-pool-rejected-by-this-pools-jwks, cognito-idp.openid-configuration-of-a-pool-without-a-domain-names-the-token-issuer-and-endpoints-under-it, cognito-idp.openid-discovery-jwks-uri-serves-unique-unpadded-rs256-signing-keys, cognito-idp.openid-configuration-for-an-unknown-pool-under-the-issuer-rejected, cognito-idp.refresh-token-auth-for-a-disabled-user-rejected, cognito-idp.refresh-token-auth-under-another-client-rejected.

## Evidence
All nine checks fail at the same frame (context.py endpoint_route -> _published_target), e.g. cognito-idp.id-token-verifies-rs256-against-the-issuer-jwks:
```
File "context.py", line 616, in endpoint_route
  return self._published_target(url, "endpoint_url"), {}
File "context.py", line 632, in _published_target
  raise PublishedUrlRejected(f"{message}: {problem[1]}", problem[0], value_class(value))
simparity.published.PublishedUrlRejected: endpoint_url was given https://cognito-idp.us-west-2.localhost:43509/us-west-2_7a6b1619f/.well-known/jwks.json (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:43509/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener
```
The same refusal, differing only in the per-run pool id and loopback port, is reported for the other eight checks; three of them (openid-configuration-for-an-unknown-pool-under-the-issuer-rejected, openid-configuration-of-a-pool-without-a-domain-..., openid-discovery-jwks-uri-...) name `/.well-known/openid-configuration` instead of `/.well-known/jwks.json`.
Expected: the `iss` of the issued ID/access token is an address the emulator serves, of the documented form https://cognito-idp.<region>.<domain>/<userPoolId>; GET {iss}/.well-known/openid-configuration -> 200 application/json with issuer={iss}, jwks_uri={iss}/.well-known/jwks.json, authorization_endpoint={iss}/authorize, token_endpoint={iss}/token, userinfo_endpoint={iss}/userInfo, revocation_endpoint={iss}/revoke, end_session_endpoint={iss}/logout, id_token_signing_alg_values_supported=["RS256"]; GET {iss}/.well-known/jwks.json -> 200 with the pool's RS256 keys (unpadded base64url n/e) that verify the issued tokens, disjoint from another pool's keys; the discovery path for a pool id that does not exist under the issuer -> refused.
Actual: `iss` host is cognito-idp.<region>.localhost (per-run port = the emulator's own port), a host the emulator does not serve, so the fetch is refused before it is issued and no status/code/body exists. (Region, pool id and port are per-run values; the published host form is the requirement violated.)
This is the refusal 20261114T130559Z-cognito-idp-2-net-new-causes-issued-tokens.md (status: fixed) claimed to fix — it carries seven of these nine check keys; the same checks are refused the same way again, and two more (refresh-token-auth-for-a-disabled-user-rejected, refresh-token-auth-under-another-client-rejected) now fail on it too.

## Steps to Reproduce
1. CreateUserPool (no domain) + CreateUserPoolClient(ExplicitAuthFlows=['ALLOW_USER_PASSWORD_AUTH','ALLOW_REFRESH_TOKEN_AUTH'], GenerateSecret=False).
2. AdminCreateUser(MessageAction='SUPPRESS') + AdminSetUserPassword(Permanent=True); InitiateAuth(AuthFlow='USER_PASSWORD_AUTH').
3. Base64url-decode the IdToken payload and read `iss`. Observed: https://cognito-idp.<region>.localhost:<emulator port>/<userPoolId>.
4. GET {iss}/.well-known/openid-configuration and GET {iss}/.well-known/jwks.json.
   Expected: the emulator serves both on an address it is reached on. Actual: the published host is not one of the emulator's own hosts, so the fetch is refused before it is issued (PublishedUrlRejected).
5. Repeat with a second pool (to compare kids/moduli), after InitiateAuth(REFRESH_TOKEN_AUTH) for the refreshed ID token, and with a pool id that does not exist under the issuer for the unknown-pool refusal.
