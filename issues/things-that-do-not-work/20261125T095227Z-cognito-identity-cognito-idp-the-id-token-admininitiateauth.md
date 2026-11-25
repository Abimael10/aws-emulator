Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role
Issue-Key: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role|run#42|AssertionError|assert (iss_parts is not None and iss_parts.scheme == "https"
Issue-Commit: 73c8f3a12cf9fd55bdc05cf114ea956347b5c801

# cognito-identity__cognito-idp: the ID token AdminInitiateAuth mints carries iss = the bare emulator endpoint (https://127.0.0.1:<port>/<poolId>) instead of a cognito-idp.<region>.<domain> issuer, so the token-mapping scenario can never be graded

> _Note: this issue was identified against the code base at commit `73c8f3a12cf9fd55bdc05cf114ea956347b5c801`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminInitiateAuth

## Description
Cause 1 of 1: the `iss` claim of the ID token returned by AdminInitiateAuth is the emulator's bare listener authority — scheme+host+port of the endpoint under test — with the user pool id as the path, instead of a user-pool token issuer whose host is `cognito-idp.<region>.<domain>`. Failing check ids for this cause: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role.

AWS: a user pool's token issuer is `https://cognito-idp.<region>.<domain>/<userPoolId>`, and that value is the `iss` claim of every ID and access token the pool issues. Smithy (cognito-identity model): CognitoIdentityProvider$ProviderName is documented as "The provider name for an Amazon Cognito user pool. For example, cognito-idp.us-east-1.amazonaws.com/us-east-1_123456789" — i.e. the pool's issuer identity is `cognito-idp.<region>.<domain>/<userPoolId>`, and that is the same string a Cognito Identity pool's Logins map is keyed by and the same string an ID token names in `iss`. Smithy (cognito-idp model): AdminInitiateAuth returns AuthenticationResult{IdToken, AccessToken, RefreshToken}; the claim set inside the JWS is recorded behavior only — the recording shows an RS256 JWS whose `iss` is the pool issuer, host labels `cognito-idp`, `<region>`, then the issuer domain, path `/<userPoolId>`, with no query and no fragment. No HTTP status or error code is involved: the call succeeds, the minted claim is wrong.

Emulator: AdminInitiateAuth succeeds and mints an RS256 ID token whose `iss` is the emulator's own serving authority with the pool id appended, e.g. `iss='https://127.0.0.1:43387/us-west-2_c3c565303'`. The requirement violated is the FORM of the host, not the per-run host/port: the first two labels must be `cognito-idp` and the request's region followed by at least one further non-empty label, and the path must be `/<userPoolId>`. Here the authority has no `cognito-idp` label and no region label at all, so nothing positional can be matched. Because the check reads the ambiguity (cognito:groups / cognito:roles / absent cognito:preferred_role) off the target's own ID token before it predicts any vending outcome, the whole downstream scenario — ambiguous Token role mapping vending the pool default under AuthenticatedRole, refusing under Deny with "The ambiguous role mapping rules for: <provider> denied this request.", honouring a CustomRoleArn present in cognito:roles and refusing one outside it with NotAuthorizedException "CustomRoleARN: <arn> is not present in the token in the cognito:roles claim" — is never exercised.

Why: a client cannot use the token at all. The `iss` is what identifies the pool to every consumer: a Cognito Identity pool's Logins key is `cognito-idp.<region>.<domain>/<poolId>` and must match the token's issuer, OIDC verification derives `{iss}/.well-known/openid-configuration` and `{iss}/.well-known/jwks.json` from it, and API Gateway / AppSync / ALB authenticate-cognito all gate on the issuer. Publishing the raw emulator authority means any correct client builds the wrong Logins key and the wrong discovery URLs, and no issuer-based verification or role mapping on a federated user-pool identity can be reached.

Fix: the token-issuance path of the cognito-idp crate — the component that computes the `iss` claim for ID/access tokens on the AdminInitiateAuth (and InitiateAuth) response — must publish the pool's issuer in the `cognito-idp.<region>.<domain>/<userPoolId>` form on an address the emulator actually serves, rather than echoing the listener authority it was reached on, and the Logins-key/issuer identity used by cognito-identity must agree with it.

Related open issues that do NOT carry this check's key: 20261125T015700Z-cognito-idp-reopens-the-token-issuer-the-emulator-stamps-into.md and 20261122T054242Z-cognito-idp-3-net-new-causes-id-token.md (cognito-idp heading, iss published as cognito-idp.<region>.localhost:<port>), 20261124T203931Z-apigateway-cognito-idp-lambda-the-cognito-id-token--r2.md, and 20261124T070730Z-cognito-identity-3-net-new-causes-a-user.md (cognito-identity heading). This filing records the AdminInitiateAuth-minted token under the cognito-identity__cognito-idp heading, where the value observed is the bare endpoint authority with no cognito-idp/region labels at all.

Traces: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role (assertion at check line 288, after AdminInitiateAuth with AuthFlow=ADMIN_USER_PASSWORD_AUTH).

## Evidence
Check cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role, mismatch:
```
File "check_cognito_identity__cognito_idp_deep_cognito_identity_3_token_mapping_ambiguous_multi_role_claim_fallback_deny_and_custom_role.py", line 288, in run
  assert (iss_parts is not None and iss_parts.scheme == "https"
AssertionError: the ID token must be issued by https://cognito-idp.us-west-2.<domain>/us-west-2_c3c565303, got iss='https://127.0.0.1:43387/us-west-2_c3c565303'
```
Expected: the IdToken from AdminInitiateAuth is an RS256 JWS with token_use='id' whose `iss` is `https://cognito-idp.<region>.<domain>/<userPoolId>` — scheme https, host label[0]='cognito-idp', label[1]=the request region, at least one further non-empty label, path exactly '/<userPoolId>', no query, no fragment. The region and pool id are values taken from the request/state; the `cognito-idp.<region>.` prefix and the `/<userPoolId>` path are the fixed form.
Actual: `iss` is the emulator's own serving authority with the pool id as path — scheme https, host '127.0.0.1', port = the emulator's listener port, path '/<userPoolId>'. The host/port shown are per-run values; the violated requirement is that the authority carries no `cognito-idp` label and no region label.
Consequence in the same check: the assertion is the first read of the token, so the ambiguous-Token-mapping legs that follow (vend under AuthenticatedRole, Deny refusal message "The ambiguous role mapping rules for: <provider> denied this request.", CustomRoleArn honoured when present in cognito:roles, NotAuthorizedException "CustomRoleARN: <arn> is not present in the token in the cognito:roles claim" when not) are never reached.

## Steps to Reproduce
1. cognito-idp: CreateUserPool, then CreateUserPoolClient with ExplicitAuthFlows allowing ADMIN_USER_PASSWORD_AUTH.
2. AdminCreateUser(MessageAction='SUPPRESS'), AdminSetUserPassword(Permanent=True).
3. AdminInitiateAuth(UserPoolId=<pool>, ClientId=<client>, AuthFlow='ADMIN_USER_PASSWORD_AUTH', AuthParameters={USERNAME, PASSWORD}) and base64url-decode AuthenticationResult.IdToken's payload.
4. Read the `iss` claim. Expected a host of the form cognito-idp.<region>.<domain> with path /<userPoolId>; observed the emulator's own authority, e.g. https://127.0.0.1:<port>/<userPoolId>.
5. Consequence: using that `iss` as the Logins key for cognito-identity GetId/GetCredentialsForIdentity, or deriving {iss}/.well-known/openid-configuration from it, cannot match the pool's documented ProviderName.
