Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cognito-idp__lambda.apigw-cognito-authorizer-claims-passthrough-to-lambda apigateway__cognito-idp__lambda.apigw-cognito-authorizer-claims-reach-integration
Issue-Key: apigateway__cognito-idp__lambda.apigw-cognito-authorizer-claims-passthrough-to-lambda|run#24|AssertionError|assert not iss_wrong, f"claims.iss {claims['iss']!r} is not the pool's issuer: {iss_wrong}"
Issue-Key: apigateway__cognito-idp__lambda.apigw-cognito-authorizer-claims-reach-integration|run#19|AssertionError|assert not iss_wrong, (
Issue-Commit: 47219b124a45386662a63382125b929260efea78

# apigateway__cognito-idp__lambda: the Cognito ID token's iss claim names cognito-idp.<region>.amazonaws.com — a host the emulator does not serve — so the authorizer claims check can never resolve the pool's issuer

> _Note: this issue was identified against the code base at commit `47219b124a45386662a63382125b929260efea78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminInitiateAuth

## Description
Cause 1 of 1: the issuer (iss) claim of an ID token minted by a user pool is published on a real-AWS host that this emulator does not answer on, so no client (and no API Gateway COGNITO_USER_POOLS authorizer check that re-derives the issuer/JWKS from the token) can reach it. Failing checks: apigateway__cognito-idp__lambda.apigw-cognito-authorizer-claims-passthrough-to-lambda and apigateway__cognito-idp__lambda.apigw-cognito-authorizer-claims-reach-integration.

AWS: recorded behavior only — the JWT body is not Smithy-modelled. A Cognito ID token's iss is the pool's issuer URL: scheme https, the labels 'cognito-idp' and the pool's region, the service domain, and a path of exactly one segment equal to the user-pool id (<region>_<suffix>). The fixed part of the template is the 'cognito-idp.<region>' prefix and the single-segment pool-id path; the domain, host suffix and port are environment values, and the pool id is a value from state. For an emulated deployment that means the issuer must be an address the emulator itself answers on (the endpoint a caller reaches cognito-idp on, or a per-resource listener host the emulator serves), because the issuer URL is what a verifier dereferences for /.well-known/openid-configuration and /.well-known/jwks.json.

Emulator: the minted ID token carries iss = 'https://cognito-idp.us-west-2.amazonaws.com/<poolId>' (observed values: .../us-west-2_ae2ab068a and .../us-west-2_bbf8003b1). The harness rejects it with: "the emulator published https://cognito-idp.us-west-2.amazonaws.com/<poolId>: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". The host is a real AWS endpoint that this emulator never serves, so the issuer is unreachable and the token is unverifiable; the host:port in that message is a per-run value, the defect is that the announced host is not one the emulator serves.

Why: every OIDC/JWT verifier performs the issuer check and derives the JWKS URI from iss. A client, a Lambda handler reading event.requestContext.authorizer.claims, or an API Gateway authorizer that validates the token against the pool's issuer must be able to fetch that issuer's discovery/JWKS documents. Announcing a real-AWS host either fails (no egress / not this emulator) or, worse, points verification at production AWS, so no token this emulator issues can be validated and multi-provider code cannot distinguish pools served here.

Fix: the cognito-idp issuer computation shared by the token-minting path (InitiateAuth/AdminInitiateAuth/RespondToAuthChallenge) and the OpenID discovery + JWKS serving path — the per-pool issuer URL it stores and embeds in iss must be rendered from the emulator's own announced endpoint for cognito-idp (plus the /<poolId> path segment), not from the real-AWS hostname template. Note the open issue 20261111T061507Z-cognito-idp-3-net-new-causes-the-id.md (Cause 1) and 20261109T124858Z-cognito-idp-5-net-new-causes-tokens-are.md (Cause 2) describe the inverse spelling of this same issuer field (iss was then the bare listener address with no cognito-idp.<region> labels); neither carries these two checks' replay keys, and the current value is an unserved real-AWS host, so this is filed separately — fold it into that cause if it is still open.

Traces: apigateway__cognito-idp__lambda.apigw-cognito-authorizer-claims-passthrough-to-lambda (assertion at check line 62) and apigateway__cognito-idp__lambda.apigw-cognito-authorizer-claims-reach-integration (assertion at check line 119).

## Evidence
Expected (AWS): the ID token's iss is the pool's issuer URL — https://cognito-idp.<region>.<domain>/<userPoolId> — served by the endpoint the caller reaches cognito-idp on, i.e. a host the emulator answers on, with a single path segment equal to the pool id.

Actual (emulator): AssertionError: "claims.iss 'https://cognito-idp.us-west-2.amazonaws.com/us-west-2_ae2ab068a' is not the pool's issuer: the emulator published https://cognito-idp.us-west-2.amazonaws.com/us-west-2_ae2ab068a: its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:45569/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener". The second check fails identically for pool us-west-2_bbf8003b1 (port 39033 in that run). Host/port/pool-id are per-run values; the invariant that fails is 'the announced issuer host is one the emulator serves'.

## Steps to Reproduce
1. CreateUserPool + CreateUserPoolClient; AdminCreateUser and AdminSetUserPassword (Permanent=true) to get a confirmed user.
2. AdminInitiateAuth (ADMIN_USER_PASSWORD_AUTH) or InitiateAuth (USER_PASSWORD_AUTH) and take AuthenticationResult.IdToken.
3. base64url-decode the token payload and read iss: it is https://cognito-idp.<region>.amazonaws.com/<poolId>, a host this emulator does not serve, instead of the emulator's own cognito-idp endpoint with path /<poolId>.
4. Equivalently: create a REST API method with a COGNITO_USER_POOLS authorizer on that pool and a Lambda proxy integration, invoke it with the ID token, and read event.requestContext.authorizer.claims.iss — the same unreachable issuer is surfaced to the integration.
