Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role
Issue-Key: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role|run#42|AssertionError|assert (iss_parts is not None and iss_parts.scheme == "https"
Issue-Commit: e597f2d78fa346ea123ebf70f6b121a2210ad722

# cognito-identity__cognito-idp: the Cognito ID token's iss claim is the bare emulator endpoint instead of a cognito-idp.<region>.<domain>/<userPoolId> issuer, so identity-pool token role mapping cannot be exercised

> _Note: this issue was identified against the code base at commit `e597f2d78fa346ea123ebf70f6b121a2210ad722`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminInitiateAuth

## Description
One net-new root cause in the cognito-idp token-issuance path, observed across the cognito-identity/cognito-idp boundary.

Cause 1 of 1: every token a user pool issues carries an `iss` claim that is the emulator's bare socket address, not a Cognito user-pool issuer URL.

AWS: a user pool's token issuer is `https://cognito-idp.<region>.<domain>/<userPoolId>` — the host is the Cognito user-pools endpoint host for the pool's region, and the single path segment is the pool id exactly as (Admin)CreateUserPool minted it (`^[\w-]+_[0-9a-zA-Z]+$`). That URL is the `iss` claim of both the ID token and the access token, and it is the value Cognito Identity matches a `Logins` provider name against and the value any OIDC/JWT consumer dereferences for discovery and JWKS. Reference: amazon-cognito-user-pools-using-tokens-verifying-a-jwt. Smithy: AdminInitiateAuth returns `AuthenticationResult` (AuthenticationResultType) carrying IdToken/AccessToken; the issuer URL itself is not a modelled member — recorded behavior only, and the recording shows the scheme `https`, a host of the form `cognito-idp.<region>.<domain>`, and the path `/<userPoolId>`. The host's domain suffix is environment-dependent and is NOT the requirement; the `cognito-idp.<region>.` host form and the `/<userPoolId>` path are.

Emulator: the issued ID token's `iss` is the emulator's own listener address with no Cognito host form and no region component at all — the replay shows `iss='https://127.0.0.1:39819/us-west-2_cf62a0f4a'` where the pool id in the path is correct but the host is the bare emulator host:port (host and port are per-run values and are not themselves the defect; the absence of the `cognito-idp.<region>.` host form is). The check fails with `AssertionError: the ID token must be issued by https://cognito-idp.us-west-2.<domain>/us-west-2_cf62a0f4a, got iss='https://127.0.0.1:39819/us-west-2_cf62a0f4a'`. Because the assertion is on the issuer, the whole downstream contract the check exists for — an ambiguous Token role mapping (several `cognito:roles`, no `cognito:preferred_role`) vending the pool default under AuthenticatedRole, refusing under Deny, and honouring a CustomRoleArn drawn from `cognito:roles` — is never reached.

Why: a client that follows AWS's documented verification procedure reads `iss` from the token and uses it to select the JWKS/discovery document and to assert the token came from the expected pool; a bare host:port matches no pool and no configured provider. Cognito Identity's own `Logins` map is keyed by `cognito-idp.<region>.<domain>/<userPoolId>`, so a token whose `iss` disagrees with the key the caller must supply makes every user-pool-backed identity-pool flow (GetId, GetOpenIdToken, GetCredentialsForIdentity, Token role mapping, CustomRoleArn selection) unverifiable, and any JWT authorizer or OIDC library keying on the issuer rejects the token.

Fix: the token-issuance component of the cognito-idp service crate that computes the `iss` claim for ID and access tokens (and the user-pool issuer publication it shares with the OIDC discovery/JWKS listeners) must emit an issuer whose host has the `cognito-idp.<region>.` form on a domain the emulator actually serves, with the pool id as its only path segment, and must use that same value consistently wherever the issuer is published or matched.

Related open issues, none of which the replay report marked as owning this check: 20261109T124858Z-cognito-idp-5-net-new-causes-tokens-are.md and 20261111T061507Z-cognito-idp-3-net-new-causes-the-id.md both record `iss` as "the bare emulator endpoint" for cognito-idp's own checks, and 20261114T130559Z-cognito-idp-2-net-new-causes-issued-tokens.md records the inverse symptom (`iss` naming the real-AWS `cognito-idp.<region>.amazonaws.com` host the emulator does not serve). The value observed here is the bare-endpoint form, on the cognito-identity/cognito-idp cross-service surface, and this check carries none of those issues' replay keys.

Traces: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role.

## Evidence
Expected (AWS): AdminInitiateAuth(AuthFlow='ADMIN_USER_PASSWORD_AUTH') -> AuthenticationResult.IdToken whose base64url-decoded payload has `iss` with scheme `https`, host of the form `cognito-idp.<region>.<domain>`, and path `/<userPoolId>` (the id returned by CreateUserPool, matching ^[\w-]+_[0-9a-zA-Z]+$).

Actual (emulator): `iss` is the emulator's bare listener address with the pool id as its path and no `cognito-idp.<region>.` host component. Replay detail:
  [mismatch] cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role
  File "check_cognito_identity__cognito_idp_deep_cognito_identity_3_token_mapping_ambiguous_multi_role_claim_fallback_deny_and_custom_role.py", line 288, in run
    assert (iss_parts is not None and iss_parts.scheme == "https" ...
  AssertionError: the ID token must be issued by https://cognito-idp.us-west-2.<domain>/us-west-2_cf62a0f4a, got iss='https://127.0.0.1:39819/us-west-2_cf62a0f4a'

The host `127.0.0.1`, the port `39819` and the pool id `us-west-2_cf62a0f4a` are per-run values; the requirement violated is the issuer's `cognito-idp.<region>.` host form. Every setup call (CreateUserPool, CreateUserPoolClient, AdminCreateUser, AdminSetUserPassword, AdminInitiateAuth, CreateRole, SetIdentityPoolRoles) succeeded, so the divergence is isolated to the `iss` claim the token carries.

## Steps to Reproduce
1. cognito-idp CreateUserPool, then CreateUserPoolClient(GenerateSecret=False, ExplicitAuthFlows=['ALLOW_ADMIN_USER_PASSWORD_AUTH','ALLOW_REFRESH_TOKEN_AUTH']); note the returned UserPoolId.
2. AdminCreateUser + AdminSetUserPassword(Permanent=True).
3. AdminInitiateAuth(AuthFlow='ADMIN_USER_PASSWORD_AUTH') and take AuthenticationResult.IdToken.
4. Base64url-decode the token's payload and read `iss`.
   Expected: scheme https, host `cognito-idp.<region>.<domain>` (a domain the emulator serves), path `/<UserPoolId>`.
   Actual: `https://<emulator-host>:<port>/<UserPoolId>` — no `cognito-idp.<region>.` host form.
5. Consequently, continue the check: create an identity pool with that user pool as a CognitoIdentityProvider, two tied-precedence groups so the ID token carries several `cognito:roles` and no `cognito:preferred_role`, SetIdentityPoolRoles with a Token mapping under AuthenticatedRole then under Deny, and GetCredentialsForIdentity with Logins keyed by `cognito-idp.<region>.<domain>/<UserPoolId>`. None of this is reachable while the issuer disagrees with the Logins key.
