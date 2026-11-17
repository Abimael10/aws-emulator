Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 2258f924775e20f7bfc7db5498fbab3354a8d6a7

# Read the SecretArn's Region through service_common::arn::parse instead of re-splitting the ARN by hand

_Filed: 2026-11-15T11:51:29Z_
_Operation: RegisterSubscriptionProvider | Class: maintainability_

> _Note: this issue was identified against the code base at commit `2258f924775e20f7bfc7db5498fbab3354a8d6a7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`RegisterSubscriptionProvider` needs the Region field of the caller-supplied `SecretArn` so it can address the in-process Secrets Manager hop in the Region the secret lives in. It gets that field by splitting the ARN string again by hand (`secret_arn.split(':').nth(3)`), even though this crate already depends on `service_common::arn` and already uses `service_common::arn::parse` for exactly this kind of field read in its own `src/service/authorization.rs`. `service_common::arn::parse` documents itself as THE one place a service reads "the region an ARN names" from, precisely so a second hand-rolled split cannot drift from it. The two copies implement the same rule ("field 4 of a well-formed resource ARN, empty meaning partition-global") in one crate, so they can diverge: a change to what counts as a well-formed ARN in the shared parse would not reach this handler.

The replacement is behaviour-identical for every value the modelled `SecretArn` pattern admits — `^arn:[a-z0-9-\.]{1,63}:secretsmanager:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:secret:[^/]{1,1023}$` guarantees the literal `arn` prefix and non-empty partition, vendor and resource fields, which is exactly what `parse` requires — so `parse(secret_arn).map(|arn| arn.region).filter(|r| !r.is_empty()).unwrap_or(request_region)` yields the same string as the hand-rolled split for every admitted input. No response changes and no existing test changes.

## Evidence

Hand-rolled copy — `services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/ops/register_subscription_provider.rs:331-338`:
```rust
fn secret_region(secret_arn: &str, request_region: &str) -> String {
    secret_arn
        .split(':')
        .nth(3)
        .filter(|segment| !segment.is_empty())
        .unwrap_or(request_region)
        .to_string()
}
```
and its consumer, `register_subscription_provider.rs:311`: `.endpoint_url(format!("https://secretsmanager.{region}.amazonaws.com"))`.

The existing helper the crate already reaches for — `services/common/src/arn.rs:82-108`:
```
/// `value`'s six fields, or `None` when `value` is not a well-formed resource ARN.
///
/// Well formed here means all six fields are present, the first is literally `arn`,
/// and the partition, the vendor and the resource are non-empty. The ONE parse: a
/// service that needs more than the vendor (the region an ARN names, the resource type
/// it addresses) reads it from here rather than splitting the value again, so no two
/// services can disagree about what an ARN is.
pub fn parse(value: &str) -> Option<Arn<'_>> {
```
with `services/common/src/arn.rs:61-62`: `/// The fourth field, empty for a partition-global resource.` / `pub region: &'a str,`.

This crate already calls that parse — `services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/service/authorization.rs:58`:
```rust
    let Some(parsed) = ::service_common::arn::parse(resource_arn) else {
```

The modelled grammar that makes the two equivalent — `OPERATIONS.md:85`: "`SecretArn` (string, pattern ^arn:[a-z0-9-\.]{1,63}:secretsmanager:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:secret:[^/]{1,1023}$, required)".

Test coverage that must stay green and unchanged: `tests/all/provider_credential_validation.rs` (secret ARNs in `us-west-2`, lines 212 and 347), `tests/all/provider_token_refusal_message.rs:113,189`, `tests/all/subscription_provider_registration.rs:61-64` (which exercises `arn:aws-us-gov:secretsmanager:us-gov-west-1:...` and the partition-global `arn:aws:secretsmanager:::secret:token` form), and `tests/all/signed_region_catalog.rs:256-299`.

## Steps to Reproduce

1. Open services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/ops/register_subscription_provider.rs and read `secret_region` at lines 331-338: it splits the ARN on ':' and takes field 3.
2. Open services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/service/authorization.rs line 58: the same crate reads an ARN's fields through `::service_common::arn::parse`.
3. Open services/common/src/arn.rs lines 82-108 and 61-62: `parse` is documented as the ONE place to read "the region an ARN names" and exposes it as `Arn::region`, 'rather than splitting the value again'.
4. Confirm the two implement one rule twice: field 4 of a resource ARN, with empty meaning partition-global (`secret_region`'s `.filter(|segment| !segment.is_empty())` vs `Arn::region`'s documented 'empty for a partition-global resource').
5. Run the crate's suites (`provider_credential_validation`, `provider_token_refusal_message`, `subscription_provider_registration`, `signed_region_catalog`) to see that no observable response depends on which of the two reads the field.
