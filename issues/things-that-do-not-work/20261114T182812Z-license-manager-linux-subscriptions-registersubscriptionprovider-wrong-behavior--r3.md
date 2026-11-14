Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 5d29039a13ade7f4799e124583f04c353fc26479

# RegisterSubscriptionProvider puts the SecretArn's unchecked Region field straight into the Secrets Manager endpoint authority

_Filed: 2026-11-14T18:28:12Z_
_Operation: RegisterSubscriptionProvider | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `5d29039a13ade7f4799e124583f04c353fc26479`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The credential check this operation is built around resolves the secret through a Secrets Manager client whose endpoint URL is built by interpolating the Region field read out of the caller-supplied `SecretArn`:

    .endpoint_url(format!("https://secretsmanager.{region}.amazonaws.com"))

and that Region is read by a hand-rolled `secret_arn.split(':').nth(3)` which checks nothing except emptiness. The modelled `SecretArn` pattern admits `[a-z0-9-\.]{0,63}` in that field — dots included — so a model-legal request such as `arn:aws:secretsmanager:us-west-2.anything:000000000000:secret:rhel-token` makes the handler address `GetSecretValue` at `https://secretsmanager.us-west-2.anything.amazonaws.com`, i.e. a host/Region this service never established, and the credential verdict it then reaches is not a verdict about the secret the caller named. Because an unanswered or uninterpretable lookup is deliberately admitted (`CredentialLookup::Indeterminate`, "the registration proceeds rather than being refused"), a registration can pass the credential gate that the very same ARN with a well-formed Region fails — the suite pins that a `SecretArn` of the modelled form "naming no secret at all" is refused with the token refusal.

This is the ONE place in the crate where a caller-supplied Region is used without being held to a Region grammar. Every sibling path does hold it: `crate::state::request_region` filters the signed scope through `is_known_region` precisely because the value is then RECORDED, `UpdateServiceSettings` refuses a `SourceRegions` entry outside the catalog, and `state/discovery.rs` builds its EC2 endpoint only from those catalog-checked Regions. The shared reader written for exactly this case already exists and this crate already depends on it (`service_common::arn`, used in `service/authorization.rs`): `service_common::arn::region_of` reads an ARN's Region under the published Region grammar "for a service that resolves a caller-supplied ARN against the vendor's endpoint in the ARN's own Region", returning `None` (so the caller falls back to its own default, which `secret_region` already does for an empty field) when the field is not a Region label.

## Evidence

Handler, hand-rolled read and the endpoint it feeds — `services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/ops/register_subscription_provider.rs:311`:
    `.endpoint_url(format!("https://secretsmanager.{region}.amazonaws.com"))`
and :328-338:
    `/// The Region the secret `secret_arn` names lives in: the Region segment of the ARN`
    `/// itself, which the modelled `SecretArn` pattern allows to be empty — and then the`
    `/// Region the request was signed for, the only other place the secret could be.`
    `fn secret_region(secret_arn: &str, request_region: &str) -> String {`
    `    secret_arn.split(':').nth(3).filter(|segment| !segment.is_empty()).unwrap_or(request_region).to_string()`

The model lets that field carry dots — OPERATIONS.md:85 / Smithy `SecretArn`:
    `pattern ^arn:[a-z0-9-\.]{1,63}:secretsmanager:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:secret:[^/]{1,1023}$`

The same crate's stated rule for every OTHER Region it uses — `src/state.rs:948-957`:
    `/// The scope is caller-supplied text, and this is what the service then RECORDS — the`
    `/// account's home Region and the Region segment of every minted`
    `/// `SubscriptionProviderArn` — so a scope is usable only when its Region names an AWS`
    `/// Region ([`is_known_region`])`
and `src/ops/update_service_settings.rs:183-190` (`.find(|source| !crate::state::is_known_region(source))` → `ValidationException`), which is what guards the sibling endpoint build in `src/state/discovery.rs:142` (`.endpoint_url(format!("https://ec2.{region}.amazonaws.com"))`).

The shared reader that exists for this exact case, in a crate already a dependency (`Cargo.toml:35 service-common = { path = "../common" }`, used at `src/service/authorization.rs:58` `::service_common::arn::parse(resource_arn)`) — `services/common/src/arn.rs:24-27`:
    `//! [`region_of`] is the one reader with a grammar of its own: a service that resolves a`
    `//! caller-supplied ARN against the vendor's endpoint in the ARN's own Region puts that`
    `//! label in a URL, so it is read under the Region grammar the models state rather than`
    `//! as the arbitrary text [`Arn::region`] reports.`
and :226-237:
    `/// For a service that has to ACT on a caller-supplied ARN's Region — reaching the`
    `/// vendor's endpoint in that Region to resolve the resource the ARN names. A Region read`
    `/// for that is not just data: it ends up in an endpoint URL's authority, so this is`
    `/// stricter than [`Arn::region`] ... The label must be non-empty, at most [`MAX_REGION_LENGTH`] characters, and composed only of `[a-z0-9-]``

The admitted-on-indeterminate path the unchecked endpoint lands in — `register_subscription_provider.rs:418-427`:
    `if client_error && about_the_secret { return unusable_credential(...); }`
    `::tracing::warn!(... "Secrets Manager gave no answer about this registration's SecretArn; admitting it");`
    `CredentialLookup::Indeterminate`

Test coverage: `tests/all/provider_credential_validation.rs:182-186` pins `"a SecretArn naming no secret at all"` → `accepted: false` (refused with the fixed token message), using only well-formed Regions (`:210-214` `arn:aws:secretsmanager:us-west-2:000000000000:secret:absent-token-{index:02}`); `tests/all/subscription_provider_registration.rs:61-64` and `provider_status_transition.rs:48-52` likewise use catalog Regions or an empty Region field, so no existing test exercises or pins a dotted Region field.

## Steps to Reproduce

1. Mount this service beside the Secrets Manager emulator (as `tests/all/harness.rs:133` `build_harness(true)` does) and create no secret.
2. Call `RegisterSubscriptionProvider` with `{"SubscriptionProviderSource":"RedHat","SecretArn":"arn:aws:secretsmanager:us-west-2:000000000000:secret:absent-token"}` — the credential lookup reaches Secrets Manager, the secret is absent, and the call is refused 400 `ValidationException` with `The subscription provider token is invalid or expired` (the rule `tests/all/provider_credential_validation.rs:182-186` pins).
3. Repeat with the Region field of that same ARN written with a dot, which the modelled `@pattern` admits: `"SecretArn":"arn:aws:secretsmanager:us-west-2.anything:000000000000:secret:absent-token"`.
4. Observe `secret_region` (`src/ops/register_subscription_provider.rs:331-338`) hand back `us-west-2.anything` unchecked and the client in `secrets_manager_client` (:311) be pointed at `https://secretsmanager.us-west-2.anything.amazonaws.com` — a host derived entirely from caller text, so the verdict `resolve_credential` reaches is not about the secret the ARN names, and an unanswered lookup is admitted (:423-427) instead of refused.
5. Compare with the sibling Region paths that do check: `src/state.rs:958-962` (`request_region` filtered by `is_known_region`) and `src/ops/update_service_settings.rs:183-190` guarding `src/state/discovery.rs:142`; and with `service_common::arn::region_of` (`services/common/src/arn.rs:239-241`), the shared reader this crate already has access to, which holds the field to `[a-z0-9-]{1,20}` and otherwise reports no Region at all so the caller falls back to the request Region — the same fallback `secret_region` already applies to an empty field, leaving every existing test's outcome unchanged.
