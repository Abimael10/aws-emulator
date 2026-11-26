Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 1ca498de069c7e00266deeedf9cf869db08aa1f5

# Expired provider access token is accepted and settles to ACTIVE, and the crate's INVALID / status-message path is unreachable

_Filed: 2026-11-18T08:02:24Z_
_Operation: RegisterSubscriptionProvider | Class: validation_

> _Note: this issue was identified against the code base at commit `1ca498de069c7e00266deeedf9cf869db08aa1f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The registration's credential check establishes only the FORM of the access token — that the secret holds a compact JWS whose payload declares the `typ: Offline` claim — and never consults the expiry the token itself carries, even though:

1. the refusal this operation states for a credential it cannot use is worded by this crate as "The subscription provider token is invalid or **expired**", i.e. expiry is part of the very verdict class the handler is deciding; and
2. the shared reader this check already calls (`service_common::jwt::read`, whose `CompactJws` the function holds) exposes `outside_window(now)` for exactly the `exp` question — one call away on the value already parsed.

So a secret holding a well-formed offline token whose `exp` is in the past is admitted: the call answers `PENDING`, and `SubscriptionProviderRecord::settle` then unconditionally writes `PROVIDER_STATUS_ACTIVE` plus a `LastSuccessfulDataRetrievalTime`. The service therefore claims it is retrieving third-party subscription data with a token it can see is expired — an unusable credential turned into a success.

The same unconditional `settle` is why the crate's own description of `PROVIDER_STATUS_INVALID` describes a state no code path can reach, and why `SubscriptionProviderRecord::status_message` is never assigned `Some(..)` anywhere in the crate — so the modelled `SubscriptionProviderStatusMessage` that `GetRegisteredSubscriptionProvider` is documented (by the register handler itself) to report can never be reported at all. A client has no way to tell a refused token from one still being checked, which is precisely the distinction the crate says that member exists for.

## Evidence

The crate states expiry as part of the verdict it is deciding:

`src/ops/register_subscription_provider.rs:460-472`:
```
/// The refusal this operation answers for EVERY credential it cannot use: the secret that
/// cannot be read, the secret holding no text value, and the value that is not an access
/// token the named provider issues.
...
pub(crate) const PROVIDER_TOKEN_REFUSAL_MESSAGE: &str =
    "The subscription provider token is invalid or expired";
```

But the check reads only the type claim — nothing about the token's window:

`src/ops/register_subscription_provider.rs:517-531`:
```
fn offline_token_defect(credential: &str) -> Option<String> {
    let token = match ::service_common::jwt::read(credential) {
        Ok(token) => token,
        Err(form) => return Some(form.to_string()),
    };
    match token.claim_str(TOKEN_TYPE_CLAIM) {
        Some(token_type) if token_type.eq_ignore_ascii_case(OFFLINE_TOKEN_TYPE) => None,
        _ => Some(format!(
            "its payload does not declare the {OFFLINE_TOKEN_TYPE:?} token type"
        )),
    }
}
```

The very reader it uses already answers the expiry question — `services/common/src/jwt.rs:229-236`:
```
    /// The two claims RFC 7519 defines it by: `exp`, "the expiration time on or after which
    ...
    pub fn outside_window(&self, now: ::std::time::SystemTime) -> Option<WindowDefect> {
```
(and `services/common/src/jwt.rs:54`: "which scope and what to do about a token carrying no expiry stay the caller's decision" — so the decision is the caller's and this caller never makes it).

The handler defers the "still honoured" verdict to the path behind the call — `src/ops/register_subscription_provider.rs:513-516`:
```
/// Whether a token OF that form is still one the provider honours is a question only the
/// provider can answer — that is the validation that runs behind an admitted registration
/// and lands on the resource's `SubscriptionProviderStatus`.
```
…but that path can only ever land on ACTIVE — `src/state.rs:209-220`:
```
    fn settle(&mut self, now: SystemTime) {
        let Some(due) = self.settles_at else { return; };
        if due > now { return; }
        self.settles_at = None;
        self.status = PROVIDER_STATUS_ACTIVE.to_string();
        self.status_message = None;
        self.last_successful_data_retrieval_time = Some(iso8601(due));
    }
```

And the crate admits the resulting dead state — `src/state.rs:50-59`:
```
/// `SubscriptionProviderStatus` of a registration whose access token the service rejected.
/// Terminal: the registration stays in the store reporting it, together with the
/// [`SubscriptionProviderRecord::status_message`] the service has about the token, which is
/// the only way a client can tell a refused token from one still being checked.
///
/// Nothing in the registration path reaches it: a registration is refused at the request
/// boundary or it is taken, and a taken one becomes usable
/// ([`SubscriptionProviderRecord::settle`]).
```

Test coverage: `tests/all/provider_credential_validation.rs:78-113` builds every "usable" row from `offline_payload()` = `{"typ":"Offline","iss":…,"scope":…,"session_state":…}` — no `exp` claim in any row, so no existing test pins the behaviour of a token that carries one (a fix must keep a token with NO `exp` acceptable, which `WindowDefect::NoExpiry` makes expressible). `tests/all/subscription_provider_registration.rs:295-316` asserts ACTIVE and no `SubscriptionProviderStatusMessage` only for registrations "nothing rejected", and `tests/all/harness.rs:51` exports `INVALID: &str = PROVIDER_STATUS_INVALID` which no test can produce.

## Steps to Reproduce

1. Mount this service beside the emulated Secrets Manager (as `tests/all/harness.rs::harness_with_secrets_manager` does).
2. Create a secret whose `SecretString` is a compact JWS (`<base64url header>.<base64url payload>.<signature>`) whose payload is `{"typ":"Offline","iss":"https://sso.example-provider.com","exp":1}` — a well-formed offline token whose expiry is long past.
3. Call `POST /subscription/RegisterSubscriptionProvider` with `SubscriptionProviderSource=RedHat` and that secret's `SecretArn`.
4. Observe HTTP 200 with `SubscriptionProviderStatus: PENDING` — the expired token is admitted, not refused with "The subscription provider token is invalid or expired".
5. Wait longer than `crate::state::TOKEN_VALIDATION_DELAY` and call `POST /subscription/GetRegisteredSubscriptionProvider` with the minted ARN.
6. Observe `SubscriptionProviderStatus: ACTIVE` with a `LastSuccessfulDataRetrievalTime` and no `SubscriptionProviderStatusMessage`: the registration reports it is usable, and no input can ever make the status `INVALID` or make a status message appear.
