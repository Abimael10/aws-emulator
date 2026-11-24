Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-identity.deploy-cognito-identity-3-cfn-principal-tag-governs-tenant-access cognito-identity.role-mapping-ambiguous-resolution-deny-blocks-credentials cognito-identity.vended-credentials-bounded-by-role-policy
Issue-Shape-Id: com.amazonaws.cognitoidentity#AWSCognitoIdentityService
Issue-Key: cognito-identity.deploy-cognito-identity-3-cfn-principal-tag-governs-tenant-access|run#25|AssertionError|assert issuer == f"https://{provider}", (
Issue-Key: cognito-identity.role-mapping-ambiguous-resolution-deny-blocks-credentials|run#26|AssertionError|assert recheck["Credentials"]["AccessKeyId"].startswith("ASIA"), (
Issue-Key: cognito-identity.vended-credentials-bounded-by-role-policy|_poll_until#32|Timeout|time.sleep(interval_s)
Issue-Commit: df434ad525a41e469c72bb6203f43a3d7d3f2407

# cognito-identity: 3 net-new causes — a user-pool ID token's iss is the bare emulator endpoint instead of the pool's ProviderName, GetCredentialsForIdentity vends an 'EMULATOR'-prefixed access key id instead of an STS 'ASIA' one, and the vended-session authorization ceilings never resolve so the bounded-credentials check reaches no verdict

> _Note: this issue was identified against the code base at commit `df434ad525a41e469c72bb6203f43a3d7d3f2407`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCredentialsForIdentity

## Description
Three net-new root causes on the Cognito Identity credential-vending surface, all observed under the suite's `### cognito-identity` heading. None of the three failing checks was marked ALREADY FILED or OWNED by the replay report.

--------------------------------------------------------------------
Cause 1 of 3: the ID token a user pool issues carries an `iss` claim that is the emulator's own listener address, not the identity pool's configured ProviderName, so no CFN-deployed principal-tag / claim-mapping flow can be exercised.
Failing check: cognito-identity.deploy-cognito-identity-3-cfn-principal-tag-governs-tenant-access

AWS: a Cognito user pool's token issuer is `https://cognito-idp.<region>.<domain>/<userPoolId>` — host of the form `cognito-idp.<region>.<domain>`, single path segment equal to the pool id exactly as CreateUserPool minted it (pool ids match `^[\w-]+_[0-9a-zA-Z]+$`). That same string is the identity pool's `CognitoIdentityProviders[].ProviderName` and the key of the `Logins` map a caller must supply, so the token's `iss` and the pool's ProviderName are necessarily the SAME value. Smithy: cognito-identity GetId/GetOpenIdToken/GetCredentialsForIdentity take `Logins` (LoginsMap, 0..10 entries) keyed by that provider name; CreateIdentityPool's CognitoIdentityProviders carries ProviderName. The issuer URL itself is not a modelled member — recorded behavior only, and the recording shows scheme `https`, host `cognito-idp.<region>.<domain>`, path `/<userPoolId>`. The host's domain suffix is environment-dependent and is NOT the requirement; the `cognito-idp.<region>.` host form, and its equality with the pool's ProviderName, are.

Emulator: the issued ID token's `iss` is the emulator's bare host:port with the pool id as the path and no `cognito-idp.<region>.` host component at all, while the identity pool simultaneously reports ProviderName in the AWS form. The replay fails with `AssertionError: the ID token issuer 'https://127.0.0.1:44533/us-west-2_e120393b8' is not the pool's ProviderName 'cognito-idp.us-west-2.amazonaws.com/us-west-2_e120393b8'`. The host `127.0.0.1`, the port and the pool id are per-run values and are not the defect; the missing `cognito-idp.<region>.` host form — and the resulting disagreement with the emulator's own ProviderName — is. Because this assertion sits before the graded behaviour, the whole contract the check exists for (an AWS::Cognito::IdentityPoolPrincipalTag mapping `custom:tenant` to a session tag that alone scopes which tenant's S3 objects the vended credentials may read, the in-place move of enforcement to a new tag key, and UseDefaults=true swapping in {"client":"aud","username":"sub"}) is never reached.

Why: a client follows AWS's documented verification procedure — read `iss` from the token, use it to select the discovery/JWKS document and to assert the token came from the expected pool, and use it as the `Logins` key. A bare host:port issuer matches no pool and no configured provider, so every user-pool-backed identity-pool flow (GetId, GetOpenIdToken, GetCredentialsForIdentity, principal-tag and role mapping) is unverifiable, and any JWT authorizer or OIDC library keying on the issuer rejects the token.

Fix: the token-issuance component that computes the `iss` claim for ID/access tokens (and the user-pool issuer value it shares with the OIDC discovery/JWKS publication) must emit an issuer whose host has the `cognito-idp.<region>.` form on a domain the emulator serves, with the pool id as its only path segment, and must use that identical string wherever the issuer is published or matched — including the identity pool's ProviderName.

Related open issue, which the replay report did NOT mark as owning this check: 20261115T035031Z-cognito-identity-cognito-idp-the-cognito-id-token.md records the same bare-endpoint `iss` for the cross-service check `cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-...`; this check carries none of its replay keys and fails on the deploy/CFN principal-tag path instead.

--------------------------------------------------------------------
Cause 2 of 3: GetCredentialsForIdentity hands back an access key id prefixed `EMULATOR` instead of an STS temporary-credential key id (`ASIA` class).
Failing check: cognito-identity.role-mapping-ambiguous-resolution-deny-blocks-credentials

AWS: GetCredentialsForIdentity returns `Credentials` whose AccessKeyId is the access-key portion of a real STS temporary session (the service internally performs AssumeRoleWithWebIdentity for the mapped role), so it is a temporary-credential key id — the `ASIA` class — paired with a SessionToken and an Expiration. Smithy (cognito-identity): `Credentials.AccessKeyId -> AccessKeyString`, documented only as "The Access Key portion of the credentials" with no pattern, so the key CLASS is recorded behavior only: every recorded vend returns an `ASIA`-prefixed id. The class is a property of the credential type, not a per-run value; the remaining 16 characters are per-run and are not graded.

Emulator: everything else in this check now matches AWS — a Rules mapping with a matching `custom:tier` claim vends a session of the rule's role (`:assumed-role/<rule role>/` in the STS caller ARN), and the unmapped login under `AmbiguousRoleResolution=Deny` is refused with exactly `NotAuthorizedException` / `The ambiguous role mapping rules for: <provider> denied this request.` (fixed text, with `<provider>` taken from the request's Logins key). The one divergence is the key class: the re-vend after the denial returns `AccessKeyId='EMULATORASUNVKYKFA2Q'` (HTTP 200) where AWS returns an `ASIA...` id, so the check fails `assert recheck["Credentials"]["AccessKeyId"].startswith("ASIA")`. `EMULATORASUNVKYKFA2Q` is one run's value; the requirement is the `ASIA` temporary-credential class.

Why: a client that classifies credentials by key-id prefix (SDK credential-type detection, audit/compliance tooling that refuses long-term `AKIA`-class keys, and any code asserting it holds a temporary federated session) reads an unknown `EMULATOR` class and either rejects the credentials or treats them as a non-temporary key, even though a SessionToken is present.

Fix: the GetCredentialsForIdentity vending path must mint the session through the same STS temporary-credential issuance used for AssumeRoleWithWebIdentity, so the access key id it returns carries the temporary-credential class rather than an emulator-authored prefix — the key-id generator in the credential/session issuance component, not this handler's response serializer.

Related open issues, none of which the replay report marked as owning this check: 20261026T234020Z-cognito-identity-a-web-identity-session-still-vends-an-emul-class-accesskeyid.md, 20261028T040721Z-cognito-identity-cognito-idp-iam-getcredentialsforidentity-vends-an.md, 20261028T221339Z-cognito-identity-3-net-new-getcredentialsforidentity-root-causes.md and 20261029T071507Z-cognito-identity-getcredentialsforidentity-under-a-token-role-mapping.md all record an EMUL/EMULATOR-class key id on other vending paths; this check reaches the symptom through a Rules mapping with AmbiguousRoleResolution=Deny and carries none of their replay keys.

--------------------------------------------------------------------
Cause 3 of 3: the two authorization ceilings that bound a GetCredentialsForIdentity session never resolve, so the bounded-credentials check burns its whole replay budget without reaching a verdict.
Failing check: cognito-identity.vended-credentials-bounded-by-role-policy

AWS: a session vended by GetCredentialsForIdentity is bounded by TWO ceilings, and the AccessDenied message says which one stopped the call. (a) The mapped role's identity-based policy: `User: <vended session ARN> is not authorized to perform: <action>[ on resource: <resource>] because no identity-based policy allows the <action> action` — fixed text, with the session ARN (`arn:<partition>:sts::<account>:assumed-role/<role name>/CognitoIdentityCredentials`), the action and the resource taken from the request and the caller's identity. (b) A session policy Cognito injects into every vended session, which no role grant can widen: the same sentence with `because no session policy allows the <action> action`, so `iam:ListRoles` stays refused even after it is granted on the role. Both messages end with IAM's per-request authorization-details pointer (a console URL and the authorization id, both per-run). Separately, Cognito Identity's own control plane refuses vended sessions outright with `AccessDeniedException` and the whole message `These credentials are not valid for accessing this resource`, whatever the role policy allows. Smithy: cognito-identity GetCredentialsForIdentity models NotAuthorizedException (403) and the `Credentials` structure; the IAM denial wording and the injected session policy are not modelled — recorded behavior only, as quoted above from the recordings.

Emulator: the check reaches no assertion at all — the replay reports `Check timed out after 60.0s`, i.e. the emulator kept a poll unsatisfied for the entire budget. Every poll in this check waits for a transition in exactly the vended-session authorization path described above: the vended session becoming usable by `sts:GetCallerIdentity`, a freshly granted inline role policy taking effect for a vended session (`s3:ListAllMyBuckets` going from AccessDenied to success), and `iam:ListRoles` being refused with `because no session policy allows` rather than by the role policy. Hypothesis: the stalled poll is one of the latter two — either the inline grant never reaches a vended session, or `iam:ListRoles` is permitted once granted because no Cognito-injected session policy exists, so the predicate looking for the `session policy` wording can never become true. Comparable loops in other cognito-identity checks complete well inside the same budget, so the stall is the emulator's, not the runner's.

Why: a client that relies on Cognito's scope-down guarantee gets credentials broader than AWS would ever vend — an emulator that hands back effectively account-wide or unceilinged sessions lets code pass locally that AWS refuses in production, and code that depends on the denial WORDING to distinguish "grant it on the role" from "this is outside Cognito's ceiling" cannot tell the two apart. The missing control-plane guard additionally lets vended credentials call Cognito Identity's own control plane, which AWS always refuses.

Fix: the GetCredentialsForIdentity session-issuance component must attach Cognito's scope-down session policy to every vended session and the authorization evaluator must report which ceiling refused a call (identity-based policy vs session policy) in the AccessDenied message, and the Cognito Identity control-plane request path must reject any request signed with a Cognito-vended session with AccessDeniedException and that fixed message. Whatever the stalled poll turns out to be, the first requirement is that each of these decisions is reached promptly rather than leaving a caller polling.

--------------------------------------------------------------------
Traces (examples): cognito-identity.deploy-cognito-identity-3-cfn-principal-tag-governs-tenant-access; cognito-identity.role-mapping-ambiguous-resolution-deny-blocks-credentials; cognito-identity.vended-credentials-bounded-by-role-policy.

## Evidence
Cause 1 — cognito-identity.deploy-cognito-identity-3-cfn-principal-tag-governs-tenant-access
Expected (AWS): the ID token minted for a user-pool user carries `iss == https://cognito-idp.<region>.<domain>/<userPoolId>`, which is the same string as the identity pool's CognitoIdentityProviders[].ProviderName and the Logins key.
Actual (emulator):
  File "check_cognito_identity_deploy_cognito_identity_3_cfn_principal_tag_governs_tenant_access.py", line 125, in run
    assert issuer == f"https://{provider}"
  AssertionError: the ID token issuer 'https://127.0.0.1:44533/us-west-2_e120393b8' is not the pool's ProviderName 'cognito-idp.us-west-2.amazonaws.com/us-west-2_e120393b8'
The host/port and the pool id are per-run; the violated requirement is the `cognito-idp.<region>.` issuer host form and its equality with the pool's own ProviderName. All setup (CFN create_stack of the user pool, client, identity pool, role, bucket; AdminCreateUser/AdminSetUserPassword/AdminInitiateAuth) succeeded, so the divergence is isolated to the `iss` claim.

Cause 2 — cognito-identity.role-mapping-ambiguous-resolution-deny-blocks-credentials
Expected (AWS): GetCredentialsForIdentity -> 200 with Credentials.AccessKeyId in the STS temporary-credential class, i.e. beginning `ASIA`, alongside SecretKey/SessionToken/Expiration.
Actual (emulator):
  File "check_cognito_identity_role_mapping_ambiguous_resolution_deny_blocks_credentials.py", line 260, in run
    assert recheck["Credentials"]["AccessKeyId"].startswith("ASIA")
  AssertionError: the mapped login stopped working after the denial: {'IdentityId': 'us-west-2:2a012315-...', 'Credentials': {'AccessKeyId': 'EMULATORASUNVKYKFA2Q', 'SecretKey': '...', 'SessionToken': '...', 'Expiration': datetime(...)}, 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}
The call itself is HTTP 200 and the earlier graded behaviour matched AWS: the matching-claim vend assumed the rule role (`:assumed-role/<rule role>/` in the STS caller ARN) and the unmapped login was refused with NotAuthorizedException and exactly `The ambiguous role mapping rules for: <provider> denied this request.` The id `EMULATORASUNVKYKFA2Q` and the identity id are per-run values; the requirement is the `ASIA` key class.

Cause 3 — cognito-identity.vended-credentials-bounded-by-role-policy
Expected (AWS): with no permission policy on the mapped role, s3:ListAllMyBuckets under the vended session is refused `AccessDenied` / `User: <session ARN> is not authorized to perform: s3:ListAllMyBuckets because no identity-based policy allows the s3:ListAllMyBuckets action`; cognito-identity:ListIdentityPools is refused `AccessDeniedException` with the same identity-based-policy sentence naming `arn:<partition>:cognito-identity:<region>:<account>:identitypool/`; after an inline grant of s3:ListAllMyBuckets, iam:ListRoles and cognito-identity:ListIdentityPools, s3:ListAllMyBuckets succeeds, iam:ListRoles is STILL refused but now `because no session policy allows the iam:ListRoles action` (plus IAM's per-request authorization-details pointer, whose region and authorization id are per-run), and cognito-identity:ListIdentityPools stays refused `AccessDeniedException` with the whole message `These credentials are not valid for accessing this resource`.
Actual (emulator):
  [mismatch] cognito-identity.vended-credentials-bounded-by-role-policy
  Error: Check timed out after 60.0s
No assertion was reached: a poll over the vended-session authorization path (session usable by STS; the inline grant taking effect for a vended session; iam:ListRoles refused by the injected session policy) never satisfied inside the replay budget, while comparable vend-and-call loops in other cognito-identity checks complete well inside it.

## Steps to Reproduce
Cause 1 (ID token issuer vs ProviderName):
1. cognito-idp CreateUserPool; CreateUserPoolClient(GenerateSecret=False, ExplicitAuthFlows=['ALLOW_ADMIN_USER_PASSWORD_AUTH','ALLOW_REFRESH_TOKEN_AUTH']); note UserPoolId.
2. AdminCreateUser(MessageAction='SUPPRESS') + AdminSetUserPassword(Permanent=True); AdminInitiateAuth(AuthFlow='ADMIN_USER_PASSWORD_AUTH') and take AuthenticationResult.IdToken.
3. Create an identity pool whose CognitoIdentityProviders[].ProviderName is `cognito-idp.<region>.<domain>/<UserPoolId>` (the value the service itself reports).
4. Base64url-decode the ID token payload and compare `iss` to `https://<that ProviderName>`.
   Expected: identical. Actual: `iss` is `https://<emulator-host>:<port>/<UserPoolId>` — no `cognito-idp.<region>.` host form, so it disagrees with the pool's own ProviderName and with the Logins key.
5. Consequently the CFN AWS::Cognito::IdentityPoolPrincipalTag flow (PrincipalTags {tenant: custom:tenant}, in-place retag, UseDefaults=true -> {"client":"aud","username":"sub"}, and the tenant-scoped S3 read/AccessDenied) is unreachable.

Cause 2 (EMULATOR-class access key id):
1. Create a user pool with a custom attribute `tier` and an app client; mint two ID tokens, one with custom:tier=admin and one with custom:tier=basic.
2. CreateIdentityPool(AllowUnauthenticatedIdentities=True, CognitoIdentityProviders=[{ProviderName, ClientId, ServerSideTokenCheck:False}]).
3. Create unauth/auth/rule roles trusting cognito-identity.amazonaws.com for that pool; SetIdentityPoolRoles with RoleMappings {'<provider>:<clientId>': {Type:'Rules', AmbiguousRoleResolution:'Deny', RulesConfiguration:{Rules:[{Claim:'custom:tier', MatchType:'Equals', Value:'admin', RoleARN:<rule role>}]}}}.
4. GetId + GetCredentialsForIdentity with the admin token (succeeds, assumes the rule role); GetCredentialsForIdentity with the basic token (correctly refused NotAuthorizedException).
5. GetCredentialsForIdentity again with the admin token and inspect Credentials.AccessKeyId.
   Expected: an STS temporary-credential key id, i.e. prefix `ASIA`. Actual: prefix `EMULATOR` (e.g. `EMULATORASUNVKYKFA2Q`), HTTP 200.

Cause 3 (vended-session ceilings / no verdict):
1. CreateIdentityPool(AllowUnauthenticatedIdentities=True); create two roles trusting cognito-identity.amazonaws.com with no permission policy; SetIdentityPoolRoles.
2. GetId + GetCredentialsForIdentity to vend a guest session; sts:GetCallerIdentity with it and check the ARN is `:assumed-role/<unauth role>/CognitoIdentityCredentials`.
3. s3:ListBuckets under the session -> expect AccessDenied `...because no identity-based policy allows the s3:ListAllMyBuckets action`; cognito-identity:ListIdentityPools -> expect AccessDeniedException with the identity-based-policy sentence.
4. iam:PutRolePolicy granting s3:ListAllMyBuckets, iam:ListRoles and cognito-identity:ListIdentityPools on the unauth role; re-vend a fresh session.
5. s3:ListBuckets -> expect success. iam:ListRoles -> expect AccessDenied `...because no session policy allows the iam:ListRoles action`. cognito-identity:ListIdentityPools -> expect AccessDeniedException `These credentials are not valid for accessing this resource`.
   Actual: the check never reaches a verdict — it is cut off at the 60 s replay budget while polling for one of these transitions.
