Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role
Issue-Key: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role|run#74|AssertionError|assert code == "NotAuthorizedException", (
Issue-Commit: 9a1cab38181c81438a46747e76c041451acccd46

# cognito-identity GetCredentialsForIdentity refuses a CustomRoleArn absent from cognito:roles with 400 InvalidIdentityPoolConfigurationException instead of 403 NotAuthorizedException

> _Note: this issue was identified against the code base at commit `9a1cab38181c81438a46747e76c041451acccd46`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCredentialsForIdentity

## Description
One net-new root cause.

Cause 1 of 1: with a Token-type role mapping whose AmbiguousRoleResolution is Deny, GetCredentialsForIdentity called with a CustomRoleArn that is NOT one of the ARNs in the token's `cognito:roles` claim is refused with the wrong error class and the wrong message. The emulator treats it as a pool-configuration problem; AWS treats it as an authorization refusal scoped to the named role.

AWS: Smithy (cognito-identity): GetCredentialsForIdentity declares both `NotAuthorizedException` (httpError 403) and `InvalidIdentityPoolConfigurationException` (httpError 400); input member `CustomRoleArn` targets ARNString (@length 20-2048). Recorded behavior: when CustomRoleArn names a role that is not present in the ID token's `cognito:roles` claim, the call is refused 403 `NotAuthorizedException` with the message `CustomRoleARN: <roleArn> is not present in the token in the cognito:roles claim`. Fixed text: `CustomRoleARN: ` ... ` is not present in the token in the cognito:roles claim`; the only variable part is the role ARN taken verbatim from the request's CustomRoleArn. The same refusal applies when CustomRoleArn names the identity pool's own default authenticated role, because that role is not in the claim either. `InvalidIdentityPoolConfigurationException` is reserved for a pool whose role configuration itself is unusable, which is not the case here: the identical request with an in-claim CustomRoleArn vends credentials, and the bare request (no CustomRoleArn) is refused with the ambiguous-mapping Deny message `The ambiguous role mapping rules for: <provider> denied this request.`

Emulator: the call is refused `InvalidIdentityPoolConfigurationException` with the message `Invalid identity pool configuration. Check assigned IAM roles for this pool.` (HTTP 400). Replay output: `AssertionError: Arm C: a CustomRoleArn absent from cognito:roles must be refused with NotAuthorizedException, got 'InvalidIdentityPoolConfigurationException' ('Invalid identity pool configuration. Check assigned IAM roles for this pool.')`.

Why: a client cannot distinguish "this caller may not assume the role it asked for" (403, retry with a different role or re-authenticate) from "this identity pool is mis-provisioned" (400, an operator must fix the pool's role assignments). SDK retry/classification differs between a 400 client-configuration fault and a 403 authorization refusal, and the message no longer names the rejected role ARN, so the caller cannot tell which of several CustomRoleArn candidates was refused. Error-handling code that branches on NotAuthorizedException falls through to a generic failure path.

Fix: the GetCredentialsForIdentity handler's CustomRoleArn authorization check (the step that compares the requested role against the resolved `cognito:roles` claim) and its error construction — it must raise NotAuthorizedException (403) with the claim-membership message instead of the pool-configuration error, and must keep the ambiguous-resolution Deny message for the bare call.

Traces: cognito-identity__cognito-idp.deep-cognito-identity-3-token-mapping-ambiguous-multi-role-claim-fallback-deny-and-custom-role (Arm C).

## Evidence
Expected (AWS, recorded): GetCredentialsForIdentity(IdentityId, Logins={<provider>: <idToken>}, CustomRoleArn=<role not in cognito:roles>) -> HTTP 403, Error.Code 'NotAuthorizedException', Error.Message 'CustomRoleARN: <that exact role ARN from the request> is not present in the token in the cognito:roles claim'. Same for CustomRoleArn = the pool's default authenticated role.

Actual (emulator): HTTP 400, Error.Code 'InvalidIdentityPoolConfigurationException', Error.Message 'Invalid identity pool configuration. Check assigned IAM roles for this pool.'

Replay detail: AssertionError: Arm C: a CustomRoleArn absent from cognito:roles must be refused with NotAuthorizedException, got 'InvalidIdentityPoolConfigurationException' ('Invalid identity pool configuration. Check assigned IAM roles for this pool.')

Controls in the same check that do behave: an in-claim CustomRoleArn vends credentials, and the bare call is refused NotAuthorizedException with 'The ambiguous role mapping rules for: <provider> denied this request.' — so the pool's role configuration is usable and only the claim-membership path misclassifies.

## Steps to Reproduce
1. CreateUserPool + app client; create two user-pool groups at the SAME Precedence, each with a distinct IAM role whose trust policy allows cognito-identity.amazonaws.com AssumeRoleWithWebIdentity; add the user to both groups so the ID token carries several cognito:roles and no cognito:preferred_role.
2. CreateIdentityPool with that user pool as a Cognito provider; SetIdentityPoolRoles with an 'authenticated' default role and RoleMappings={'<userPoolId>:<clientId>': {'Type': 'Token', 'AmbiguousRoleResolution': 'Deny'}}.
3. GetId / GetOpenIdTokenForDeveloperIdentity-free path: GetCredentialsForIdentity(IdentityId, Logins={'<provider>': idToken}) -> expect 403 NotAuthorizedException 'The ambiguous role mapping rules for: <provider> denied this request.' (control).
4. Create a third IAM role with the SAME trust policy but do NOT add it to any group, so it is absent from cognito:roles.
5. GetCredentialsForIdentity(IdentityId, Logins={'<provider>': idToken}, CustomRoleArn=<that third role ARN>).
   Expected: 403 NotAuthorizedException 'CustomRoleARN: <that ARN> is not present in the token in the cognito:roles claim'.
   Actual: 400 InvalidIdentityPoolConfigurationException 'Invalid identity pool configuration. Check assigned IAM roles for this pool.'
6. Repeat step 5 with CustomRoleArn = the identity pool's default authenticated role ARN: same divergence.
