// SPDX-License-Identifier: CC0-1.0

//! WHERE `RegisterSubscriptionProvider` reads the secret a registration names.
//!
//! The operation resolves the `SecretArn` through Secrets Manager, so the Region that
//! lookup is addressed in ends up in an endpoint URL's AUTHORITY. The rule under test:
//! such a Region is the Region LABEL the ARN itself names, and — for an ARN naming no
//! Region label at all — the Region the request was signed for; caller-supplied text that
//! is not a Region label is never the Region a lookup is addressed in, so it can never buy
//! a registration an admission the same request with a well-formed Region does not get.
//!
//! The Region field is where that rule bites, because the modelled `SecretArn` pattern is
//! far wider than the Region grammar:
//! `^arn:[a-z0-9-\.]{1,63}:secretsmanager:[a-z0-9-\.]{0,63}:[a-z0-9-\.]{0,63}:secret:[^/]{1,1023}$`
//! (`smithy_query(query="shape",shape="SecretArn")`) admits dots, the empty field and
//! lengths no Region label has, while a Region label is non-empty, at most
//! [`service_common::arn::MAX_REGION_LENGTH`] characters and composed only of `[a-z0-9-]`.
//!
//! Both sides of the rule are OBSERVABLE here, because Secrets Manager holds a secret in
//! the Region it was created in and resolves an ARN only in the Region the lookup is
//! addressed to: a fixture secret placed in the Region a row's ARN names is read — and the
//! registration taken — exactly when that field is a Region label the lookup may use. A row
//! whose field is outside the grammar places its fixture secret too, holding a perfectly
//! usable token, so the row's refusal can only come from the lookup not being addressed at
//! the Region that text names.
//!
//! The table spans what the rule generalises over: the Region the request itself is signed
//! for, another Region, a label at the grammar's length bound and one character past it,
//! fields carrying dots, a field of nothing but dots, and the longest field the modelled
//! pattern admits. `tests/all/provider_credential_validation.rs` spans what the secret
//! HOLDS; this suite varies only where it lives.

use serde_json::{json, Value};

use crate::harness::{
    create_secret_in, get_provider, harness_with_secrets_manager, jwt, list_providers, member,
    provider_arn, register, stored_provider_count, token_header, Harness, PENDING, RED_HAT, REGION,
    TOKEN_REFUSAL_MESSAGE,
};

/// A credential the service can use: the access token a `RedHat` registration needs is the
/// provider's OFFLINE token, a JSON Web Token whose payload declares the offline token type
/// (`SecretArn`: "the secret contains your Red Hat Offline token"). What the value has to
/// be is `tests/all/provider_credential_validation.rs`'s rule; here it is the same usable
/// token in every row, so nothing but the secret's Region can decide a row.
fn offline_token() -> String {
    jwt(
        &token_header(),
        &json!({
            "typ": "Offline",
            "iss": "https://sso.example-provider.com/auth/realms/external",
            "scope": "offline_access",
        }),
        false,
    )
}

/// A Region label of exactly `length` characters, built only from `[a-z0-9-]` — the
/// alphabet the Region grammar states — so a row at the grammar's length bound and the row
/// one character past it differ in NOTHING but their length.
fn label_of_length(length: usize) -> String {
    let prefix = "us-west-2-";
    format!("{prefix}{}", "x".repeat(length - prefix.len()))
}

/// One row: the Region the registration's secret is stored in — which is the Region field
/// of the `SecretArn` Secrets Manager mints for it — and whether the credential lookup
/// reaches that secret.
struct Case {
    /// How the row reads in an assertion message.
    label: String,
    /// The Region the fixture secret is created in.
    secret_region: String,
    /// Whether `RegisterSubscriptionProvider` must take a registration naming it: true
    /// exactly when the field is a Region label a lookup may be addressed at.
    reachable: bool,
}

impl Case {
    fn new(label: &str, secret_region: String, reachable: bool) -> Self {
        Self {
            label: label.to_string(),
            secret_region,
            reachable,
        }
    }
}

fn region_cases() -> Vec<Case> {
    let bound = service_common::arn::MAX_REGION_LENGTH;
    vec![
        Case::new(
            "the Region the request itself is signed for",
            REGION.to_string(),
            true,
        ),
        Case::new("another AWS Region", "us-east-1".to_string(), true),
        Case::new(
            "an AWS Region in another geography",
            "ap-southeast-2".to_string(),
            true,
        ),
        Case::new(
            "a Region label as long as the grammar allows",
            label_of_length(bound),
            true,
        ),
        Case::new(
            "a label one character past that length",
            label_of_length(bound + 1),
            false,
        ),
        Case::new(
            "a field carrying a dot, which the modelled pattern admits",
            "us-west-2.anything".to_string(),
            false,
        ),
        Case::new("a field of nothing but dots", "...".to_string(), false),
        Case::new(
            "a field as long as the modelled pattern admits",
            format!("us-west-2-{}", "x".repeat(53)),
            false,
        ),
    ]
}

/// The Region field of `secret_arn`: the fourth of the six colon-separated ARN fields.
fn region_field(secret_arn: &str) -> &str {
    secret_arn.split(':').nth(3).unwrap_or_default()
}

/// How many providers `ListRegisteredSubscriptionProviders` reports for `RedHat`.
async fn listed_count(h: &Harness) -> usize {
    let reply = list_providers(h, &json!({ "SubscriptionProviderSources": [RED_HAT] })).await;
    reply
        .ok("listing the registered providers")
        .get("RegisteredSubscriptionProviders")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

#[tokio::test]
async fn the_credential_is_looked_up_only_in_a_region_the_secret_arn_names() {
    for (index, case) in region_cases().into_iter().enumerate() {
        // An emulator per row: the account holds ONE registration per source, so a row
        // whose credential is reachable must be the call that creates it.
        let h = harness_with_secrets_manager();
        let label = &case.label;
        let secret_arn = create_secret_in(
            &h,
            &case.secret_region,
            &format!("provider-token-{index:02}"),
            &json!({ "SecretString": offline_token() }),
        )
        .await;
        // The fixture is only worth asserting on if Secrets Manager really stored the
        // secret in the row's Region: that is what the ARN's own Region field reports.
        assert_eq!(
            region_field(&secret_arn),
            case.secret_region,
            "case {label:?}: the fixture secret must live in that Region, got {secret_arn:?}"
        );

        let reply = register(
            &h,
            &json!({ "SubscriptionProviderSource": RED_HAT, "SecretArn": secret_arn }),
        )
        .await;

        if case.reachable {
            let arn = provider_arn(&reply, label);
            assert_eq!(
                member(&reply, label, "SubscriptionProviderStatus"),
                Some(PENDING),
                "case {label:?}: the registration must be taken, got {:?}",
                reply.body
            );
            // Round trip: the created registration is readable back through the read side,
            // reporting the very secret it was registered with.
            let fetched = get_provider(&h, &arn).await;
            assert_eq!(
                member(&fetched, label, "SecretArn"),
                Some(secret_arn.as_str()),
                "case {label:?}: the created provider must report the secret it was \
                 registered with, got {:?}",
                fetched.body
            );
            assert_eq!(
                stored_provider_count(&h),
                1,
                "case {label:?}: a taken registration creates exactly one provider"
            );
            assert_eq!(
                listed_count(&h).await,
                1,
                "case {label:?}: the created provider must be listed"
            );
        } else {
            reply.error(label, 400, "ValidationException");
            assert_eq!(
                reply.message(),
                TOKEN_REFUSAL_MESSAGE,
                "case {label:?}: expected {TOKEN_REFUSAL_MESSAGE:?}, got {:?}",
                reply.message()
            );
            assert_eq!(
                stored_provider_count(&h),
                0,
                "case {label:?}: a refused registration must create nothing"
            );
            assert_eq!(
                listed_count(&h).await,
                0,
                "case {label:?}: a refused registration must leave the list empty"
            );
        }
    }
}

/// The `SecretArn` forms whose Region field names no secret the account holds anywhere:
/// every one of them is the same refusal, so a field outside the Region grammar is worth
/// no more to a caller than a well-formed one naming nothing.
///
/// The forms span the Region field's whole modelled width: a Region label, the EMPTY field
/// of the partition-global form the pattern admits, a dotted field, and a label past the
/// grammar's length bound.
fn unresolvable_secret_arns() -> Vec<(String, String)> {
    let bound = service_common::arn::MAX_REGION_LENGTH;
    [
        ("a well-formed Region label".to_string(), REGION.to_string()),
        (
            "no Region field at all".to_string(),
            String::new(),
        ),
        (
            "a Region field carrying a dot".to_string(),
            "us-west-2.anything".to_string(),
        ),
        (
            "a Region field past the grammar's length bound".to_string(),
            label_of_length(bound + 1),
        ),
        ("a Region field of one dot".to_string(), ".".to_string()),
    ]
    .into_iter()
    .map(|(label, region)| {
        (
            label,
            format!("arn:aws:secretsmanager:{region}:000000000000:secret:absent-token"),
        )
    })
    .collect()
}

#[tokio::test]
async fn a_secret_arn_naming_no_secret_is_refused_whatever_its_region_field_carries() {
    for (label, secret_arn) in unresolvable_secret_arns() {
        let h = harness_with_secrets_manager();

        let reply = register(
            &h,
            &json!({ "SubscriptionProviderSource": RED_HAT, "SecretArn": secret_arn }),
        )
        .await;

        reply.error(&label, 400, "ValidationException");
        assert_eq!(
            reply.message(),
            TOKEN_REFUSAL_MESSAGE,
            "case {label:?}: expected {TOKEN_REFUSAL_MESSAGE:?}, got {:?}",
            reply.message()
        );
        assert_eq!(
            stored_provider_count(&h),
            0,
            "case {label:?}: a refused registration must create nothing"
        );
        assert_eq!(
            listed_count(&h).await,
            0,
            "case {label:?}: a refused registration must leave the list empty"
        );
    }
}
