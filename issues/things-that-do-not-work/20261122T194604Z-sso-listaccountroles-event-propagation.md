Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: d8571c81dd15a8f22d7c2603104b67b8a6bcc578

# The Identity Center user an identity-bound access token records never reaches the portal's sign-in: the subject becomes the application ARN and every principal's assignments are reported

_Filed: 2026-11-22T19:46:04Z_
_Operation: ListAccountRoles | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `d8571c81dd15a8f22d7c2603104b67b8a6bcc578`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The portal resolves a presented bearer token against the emulator's own OIDC mint (`crate::state::sign_in::minted_sign_in`) and against IAM Identity Center's assignment rows (`assigned_accounts`). Both readings DROP the identity-store principal the emulator actually records for the sign-in, and both justify the drop with a statement that is false about the very record they read.

1. `minted_sign_in` reads the mint's `IssuedToken` and takes `subject: issued.client_id.clone()` — the client registration (for a token minted by the JWT-bearer exchange, `client_id` is the APPLICATION ARN the exchange was made for). The same `IssuedToken` the portal is reading carries `identity: Option<BoundIdentity>`, which holds the `user_id` of the identity-store user the exchange resolved, written back into the mint's store precisely "so a later read of the token resolves the same identity the caller was handed". The portal is that later read, and ignores it. The consequence is observable through this crate's own public surface: `signed_in_subject` is documented as answering "the IAM Identity Center identity a request signed with that credential carries, which is what a service requiring a federated caller resolves a request through", but for such a sign-in it answers the application ARN rather than that user.

2. `assigned_accounts` reports every account assignment under the instance to any sign-in, with the stated reason that "the emulator records no mapping from a device-authorization sign-in to one identity-store principal". For an identity-bound token the mapping IS recorded (point 1), and the assignment rows this function walks carry `principal_type`/`principal_id` — so the sign-in of a token bound to user X is served the accounts and (via `ListAccountRoles`) the permission-set names the administrator assigned to an entirely different principal, and `GetRoleCredentials` will then mint role credentials for them.

The defect is that the identity the OIDC mint recorded for the sign-in never propagates into the portal's `SignedIn`, so neither the subject a minted credential speaks for nor the assignment view is the sign-in's own; the crate's comments assert that identity does not exist, while the struct it dereferences two fields away carries it.

## Evidence

The portal's read of the mint — `services/service-com-amazonaws-sso-swbportalservice/src/state/sign_in.rs:183-199`:
```rust
fn minted_sign_in<Eng: Send + Sync + 'static>(... ) -> Option<MintedSignIn> {
    let tokens = oidc::state::service_state(engine)?;
    let tokens = tokens.lock();
    let issued = tokens.access_tokens.get(access_token)?;
    (!issued.is_expired(now)).then(|| MintedSignIn {
        subject: issued.client_id.clone(),
```
its stated justification — `src/state/sign_in.rs:173-178`:
```
/// IAM Identity Center attributes a sign-in to the identity-store user who approved it at
/// the access portal, and that approval is made at the portal page rather than through any
/// operation of this API or of the OIDC API, so the registration the token was ISSUED TO
/// is the identity the emulator records for it.
```
and the second justification, on the assignment reading — `src/state/sign_in.rs:218-224`:
```
/// Which PRINCIPAL an assignment names is not a filter here. The portal reports the
/// accounts assigned to the signed-in user, and the emulator records no mapping from a
/// device-authorization sign-in to one identity-store principal — the approval that would
/// name one is made outside every API the emulator serves — so what a sign-in reports is
/// the assignments the administrator made in the account's directory, all of them.
```

The record that mapping is in, on the SAME struct the portal dereferences — `services/service-com-amazonaws-ssooidc-awsssooidcservice/src/state.rs:466-471`:
```rust
    /// The Identity Center identity this token is BOUND to, for a token minted from a
    /// grant that authenticated a user of an identity store (the JWT-bearer grant's
    /// assertion), and `None` for a token of a grant that authenticated only a client.
    /// It is what [`Self::id_token`] states and what makes the token identity-bound, so
    /// it is recorded on the token rather than re-derived from whoever presents it.
    pub identity: Option<BoundIdentity>,
```
what it holds — `services/service-com-amazonaws-ssooidc-awsssooidcservice/src/state/trusted_identity.rs:187-193`:
```rust
pub struct BoundIdentity {
    pub instance_arn: String,
    pub identity_store_id: String,
    /// The `UserId` of the user the assertion's claim resolved to.
    pub user_id: String,
```
and the fact that it is persisted under the access token the portal looks up — `services/service-com-amazonaws-ssooidc-awsssooidcservice/src/state.rs:934-938`:
```rust
        token.identity = Some(identity);
        // The record this store holds is what was answered, so a later read of the token
        // resolves the same identity the caller was handed.
        self.access_tokens
            .insert(token.access_token.clone(), token.clone());
```
(that token's `client_id` is the application ARN: `issue_identity_bound_token(application_arn, ...)` calls `self.issue_token(application_arn, ...)`, `state.rs:903-918`, and `issue_token` stores `client_id: client_id.to_string()` at `state.rs:856`.)

The principal dimension the portal's assignment walk never reads — `services/service-com-amazonaws-ssoadmin-swbexternalservice/src/state.rs:2300-2306`:
```rust
pub struct AccountAssignmentRecord {
    pub instance_arn: String,
    pub account_id: String,
    pub target_type: String,
    pub permission_set_arn: String,
    pub principal_type: String,
    pub principal_id: String,
```
versus the portal's loop over those rows, which filters on instance, operation status and permission set only — `src/state/sign_in.rs:232-257`:
```rust
    for assignment in &identity_center.account_assignments {
        if !identity_center.instances.contains_key(&assignment.instance_arn) { continue; }
        ...
        roles.entry(assignment.account_id.clone()).or_default().insert(permission_set.name.clone());
```

What the dropped subject is claimed to be — `services/service-com-amazonaws-sso-swbportalservice/src/state.rs:544-548`:
```
    /// This is the IAM Identity Center identity a request signed with that credential
    /// carries, which is what a service requiring a federated caller resolves a request
    /// through.
```
(consumed as such outside this crate: `services/service-com-amazonaws-qapps-qappsservice/src/service/identity_center.rs:118` calls `.signed_in_subject(&access_key_id, SystemTime::now())`.)

Test coverage: `tests/all/minted_sign_in.rs` only ever presents tokens from the device-authorization/refresh flows (`emulator.rotated`, `emulator.refreshed`, `:380-410`) and asserts `signed_in_subject(...).is_some()` (`:815`, `:1065`, `:1147`) — it never presents an identity-bound token, and no test in `tests/all/` asserts which principal's assignments a sign-in may see, so both readings above are unpinned.

## Steps to Reproduce

1. Stand the emulator up as `core/router/src/lib.rs:959` does, with the portal, sso-oidc, sso-admin and organizations services on the one shared engine.
2. Through sso-oidc, mint an identity-bound access token (the JWT-bearer exchange of `CreateTokenWithIAM`), so the mint stores `IssuedToken { client_id: <application ARN>, identity: Some(BoundIdentity { user_id: <identity-store UserId>, .. }) }` — services/service-com-amazonaws-ssooidc-awsssooidcservice/src/state.rs:903-939.
3. Through sso-admin, create an account assignment for a DIFFERENT principal (another `principal_id`) in account 111111111111 with permission set `AdministratorAccess`, and let its creation operation reach SUCCEEDED.
4. Call `GET /assignment/roles?account_id=111111111111` with `x-amz-sso_bearer_token: <the identity-bound access token>`. Observed: `roleList` reports `AdministratorAccess`, because `assigned_accounts` (src/state/sign_in.rs:232-257) never reads `assignment.principal_id`/`principal_type` — although the token record names the identity-store user this sign-in is for. The same holds for `GET /assignment/accounts`.
5. Call `GET /federation/credentials?account_id=111111111111&role_name=AdministratorAccess` with the same token, then read `crate::state::service_state(&engine).lock().signed_in_subject(&accessKeyId, SystemTime::now())`. Observed: the application ARN (the token's `client_id`). Expected per src/state.rs:544-548 and the token record at ssooidc state.rs:466-471: the Identity Center identity the token is bound to (`BoundIdentity::user_id`).
6. Contrast the comments that justify both readings (src/state/sign_in.rs:173-178 and :218-224): they state that no identity-store principal is recorded for a sign-in, while the struct the function on the line below dereferences carries exactly that.
