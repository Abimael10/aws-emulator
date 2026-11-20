// SPDX-License-Identifier: CC0-1.0

//! How this service reports an INSTANT, over the wire (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! RULE: every timestamp member of this model is rendered as a number of seconds since the
//! Unix epoch whose fraction carries whole milliseconds and nothing finer — the resolution
//! Amazon Cognito reports an instant at — so the same stored instant renders identically on
//! every read of it.
//!
//! Every timestamp member of this service targets `DateType`, a plain `timestamp` with no
//! `@timestampFormat` override
//! (`smithy_query(query="shape",shape="DateType,UserPoolType,UserType,GroupType,TermsType,ClientSecretDescriptorType,ProviderDescription,WebAuthnCredentialDescription")`),
//! and the protocol is `awsJson1_1`, whose default timestamp serialization is epoch seconds
//! as a JSON number; the members' documentation states "Amazon Cognito returns this timestamp
//! in UNIX epoch time format". The clock is read at whatever resolution the host offers, so
//! the rule is about the RESOLUTION the reading is reported at, never about the reading.
//!
//! The rows span what the rule generalises over, so no row is the rule: the MEMBER NAME
//! (`CreationDate`, `LastModifiedDate`, `UserCreateDate`, `UserLastModifiedDate`,
//! `ClientSecretCreateDate`), the resource FAMILY the member belongs to (user pool, app
//! client, app-client secret, user, group, identity provider, terms documents), the NESTING
//! the member sits at (a top-level member of the response, a member of a nested structure, a
//! member of a list element), and the PATH that produced it (a create's own answer, a
//! single-resource read, a listing). Every date member of every answer is checked, not only
//! the ones a row names.

use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::error_wire::{ok, router};

/// The most fractional digits a rendered instant of this service carries: whole
/// milliseconds, and nothing finer.
const MOST_FRACTIONAL_DIGITS: usize = 3;

/// How far from the moment the test runs a freshly stamped instant may fall and still be
/// seconds since the epoch rather than some other unit. Deliberately generous — the window
/// is here to catch a value in the wrong UNIT (milliseconds since the epoch is about 1.8e12,
/// a thousand-fold away), never to time anything.
const PLAUSIBLE_WINDOW_SECONDS: f64 = 86_400.0;

/// Whether a response member named `name` carries an instant: the members of this model
/// that target `DateType` are the ones whose names end in `Date` (`CreationDate`,
/// `LastModifiedDate`, `UserCreateDate`, `ClientSecretCreateDate`, `StartDate`,
/// `CompletionDate`, `FeedbackDate`, ...) plus `WebAuthnCredentialDescription.CreatedAt`.
fn is_date_member(name: &str) -> bool {
    name.ends_with("Date") || name == "CreatedAt"
}

/// Every date member under `value`, as `path -> rendered value`, so an answer is checked
/// wherever it reports an instant rather than only where a row looked.
fn date_members(path: &str, value: &Value, found: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(members) => {
            for (name, member) in members {
                let child = format!("{path}.{name}");
                if is_date_member(name) {
                    found.insert(child.clone(), member.clone());
                }
                date_members(&child, member, found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                date_members(&format!("{path}[{index}]"), item, found);
            }
        }
        _ => {}
    }
}

/// The date members of `payload`, keyed by their JSON path.
fn dates(payload: &Value) -> BTreeMap<String, Value> {
    let mut found = BTreeMap::new();
    date_members("$", payload, &mut found);
    found
}

/// How many digits `seconds` carries after the decimal point, from the shortest decimal
/// rendering that reads back as the same number — which is what a client sees on the wire.
fn fractional_digits(seconds: f64) -> usize {
    let rendered = format!("{seconds}");
    rendered
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len())
}

/// Seconds since the Unix epoch at the moment of the call, for the plausibility window.
fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the host clock is at or past the Unix epoch")
        .as_secs_f64()
}

/// Assert the rule over every date member `payload` reports, and answer how many it found.
fn assert_millisecond_resolution(case: &str, payload: &Value) -> usize {
    let found = dates(payload);
    for (path, value) in &found {
        let seconds = value.as_f64().unwrap_or_else(|| {
            panic!(
                "case {case:?}: {path} is a timestamp with no format override, so it is \
                 rendered as a number of seconds since the epoch, got {value}"
            )
        });
        let digits = fractional_digits(seconds);
        assert!(
            digits <= MOST_FRACTIONAL_DIGITS,
            "case {case:?}: {path} carries at most {MOST_FRACTIONAL_DIGITS} fractional \
             digits, got {digits} in {seconds}"
        );
        let distance = (seconds - now_seconds()).abs();
        assert!(
            distance <= PLAUSIBLE_WINDOW_SECONDS,
            "case {case:?}: {path} is seconds since the epoch, got {seconds} \
             ({distance} away from now)"
        );
    }
    found.len()
}

/// One row: a call that reports an instant, and the paths it must report one at.
struct Row {
    case: &'static str,
    operation: &'static str,
    body: Value,
    reports: Vec<String>,
}

#[tokio::test]
async fn every_date_member_is_epoch_seconds_at_millisecond_resolution() {
    let router = router();

    // One pool with one app client and one user, so the read and listing rows below have
    // something to report and the create rows add to it.
    let pool = ok(
        &router,
        "CreateUserPool",
        &json!({ "PoolName": "date-precision" }),
    )
    .await;
    let user_pool_id = pool["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    let client = ok(
        &router,
        "CreateUserPoolClient",
        &json!({ "UserPoolId": &user_pool_id, "ClientName": "date-precision-client" }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();
    ok(
        &router,
        "AdminCreateUser",
        &json!({ "UserPoolId": &user_pool_id, "Username": "alice" }),
    )
    .await;
    ok(
        &router,
        "CreateGroup",
        &json!({ "UserPoolId": &user_pool_id, "GroupName": "readers" }),
    )
    .await;
    ok(
        &router,
        "CreateIdentityProvider",
        &json!({
            "UserPoolId": &user_pool_id,
            "ProviderName": "AuthExampleOIDC",
            "ProviderType": "OIDC",
            "ProviderDetails": { "oidc_issuer": "https://auth.example.com" },
        }),
    )
    .await;

    let rows = vec![
        Row {
            case: "a user pool created",
            operation: "CreateUserPool",
            body: json!({ "PoolName": "date-precision-second" }),
            reports: vec![
                "$.UserPool.CreationDate".to_string(),
                "$.UserPool.LastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "the pool read back",
            operation: "DescribeUserPool",
            body: json!({ "UserPoolId": &user_pool_id }),
            reports: vec![
                "$.UserPool.CreationDate".to_string(),
                "$.UserPool.LastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "the pools listed",
            operation: "ListUserPools",
            body: json!({ "MaxResults": 10 }),
            reports: vec![
                "$.UserPools[0].CreationDate".to_string(),
                "$.UserPools[0].LastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "a user created",
            operation: "AdminCreateUser",
            body: json!({ "UserPoolId": &user_pool_id, "Username": "bob" }),
            reports: vec![
                "$.User.UserCreateDate".to_string(),
                "$.User.UserLastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "a user read back, whose instants are top-level response members",
            operation: "AdminGetUser",
            body: json!({ "UserPoolId": &user_pool_id, "Username": "alice" }),
            reports: vec![
                "$.UserCreateDate".to_string(),
                "$.UserLastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "the users listed",
            operation: "ListUsers",
            body: json!({ "UserPoolId": &user_pool_id }),
            reports: vec![
                "$.Users[0].UserCreateDate".to_string(),
                "$.Users[0].UserLastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "an app client created",
            operation: "CreateUserPoolClient",
            body: json!({ "UserPoolId": &user_pool_id, "ClientName": "date-precision-2" }),
            reports: vec![
                "$.UserPoolClient.CreationDate".to_string(),
                "$.UserPoolClient.LastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "an app client read back",
            operation: "DescribeUserPoolClient",
            body: json!({ "UserPoolId": &user_pool_id, "ClientId": &client_id }),
            reports: vec![
                "$.UserPoolClient.CreationDate".to_string(),
                "$.UserPoolClient.LastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "an app-client secret created",
            operation: "AddUserPoolClientSecret",
            body: json!({ "UserPoolId": &user_pool_id, "ClientId": &client_id }),
            reports: vec!["$.ClientSecretDescriptor.ClientSecretCreateDate".to_string()],
        },
        Row {
            case: "a group read back",
            operation: "GetGroup",
            body: json!({ "UserPoolId": &user_pool_id, "GroupName": "readers" }),
            reports: vec![
                "$.Group.CreationDate".to_string(),
                "$.Group.LastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "the groups listed",
            operation: "ListGroups",
            body: json!({ "UserPoolId": &user_pool_id }),
            reports: vec![
                "$.Groups[0].CreationDate".to_string(),
                "$.Groups[0].LastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "an identity provider read back",
            operation: "DescribeIdentityProvider",
            body: json!({ "UserPoolId": &user_pool_id, "ProviderName": "AuthExampleOIDC" }),
            reports: vec![
                "$.IdentityProvider.CreationDate".to_string(),
                "$.IdentityProvider.LastModifiedDate".to_string(),
            ],
        },
        Row {
            case: "the identity providers listed",
            operation: "ListIdentityProviders",
            body: json!({ "UserPoolId": &user_pool_id }),
            reports: vec![
                "$.Providers[0].CreationDate".to_string(),
                "$.Providers[0].LastModifiedDate".to_string(),
            ],
        },
        Row {
            // `TermsSource` and `Enforcement` each carry the one value their enum admits
            // (`smithy_query(query="operation",operation="CreateTerms")`).
            case: "a terms document created, whose instants are required members",
            operation: "CreateTerms",
            body: json!({
                "UserPoolId": &user_pool_id,
                "ClientId": &client_id,
                "TermsName": "terms-of-use",
                "TermsSource": "LINK",
                "Enforcement": "NONE",
                "Links": { "cognito:default": "https://example.com/terms" },
            }),
            reports: vec![
                "$.Terms.CreationDate".to_string(),
                "$.Terms.LastModifiedDate".to_string(),
            ],
        },
    ];

    for Row {
        case,
        operation,
        body,
        reports,
    } in rows
    {
        let payload = ok(&router, operation, &body).await;
        let found = dates(&payload);
        for path in &reports {
            assert!(
                found.contains_key(path),
                "case {case:?}: expected {operation} to report an instant at {path}, \
                 got {:?} in {payload}",
                found.keys().collect::<Vec<_>>()
            );
        }
        let checked = assert_millisecond_resolution(case, &payload);
        assert!(
            checked >= reports.len(),
            "case {case:?}: expected at least {} instants checked, got {checked}",
            reports.len()
        );
    }
}

/// The same rule as an INVARIANT: an unmodified resource reports the same instant on every
/// read of it, byte for byte, because the rendering has no digits the store does not hold.
#[tokio::test]
async fn an_unmodified_resource_reports_the_same_instant_on_every_read() {
    let router = router();
    let created_pool = ok(
        &router,
        "CreateUserPool",
        &json!({ "PoolName": "date-precision-stable" }),
    )
    .await;
    let user_pool_id = created_pool["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    let created_user = ok(
        &router,
        "AdminCreateUser",
        &json!({ "UserPoolId": &user_pool_id, "Username": "alice" }),
    )
    .await;

    // (case, the create's own answer, the member it reported the instant at, the read, the
    // member the read reports it at)
    let rows: Vec<(&str, &Value, &str, &str, Value, &str)> = vec![
        (
            "a pool's creation instant",
            &created_pool,
            "$.UserPool.CreationDate",
            "DescribeUserPool",
            json!({ "UserPoolId": &user_pool_id }),
            "$.UserPool.CreationDate",
        ),
        (
            "a pool's last-modified instant",
            &created_pool,
            "$.UserPool.LastModifiedDate",
            "DescribeUserPool",
            json!({ "UserPoolId": &user_pool_id }),
            "$.UserPool.LastModifiedDate",
        ),
        (
            "a user's creation instant",
            &created_user,
            "$.User.UserCreateDate",
            "AdminGetUser",
            json!({ "UserPoolId": &user_pool_id, "Username": "alice" }),
            "$.UserCreateDate",
        ),
        (
            "a user's last-modified instant",
            &created_user,
            "$.User.UserLastModifiedDate",
            "AdminGetUser",
            json!({ "UserPoolId": &user_pool_id, "Username": "alice" }),
            "$.UserLastModifiedDate",
        ),
    ];

    for (case, created, created_path, operation, body, read_path) in rows {
        let written = dates(created)
            .get(created_path)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the create reports {created_path}"));
        let first = ok(&router, operation, &body).await;
        let second = ok(&router, operation, &body).await;
        let first_read = dates(&first)
            .get(read_path)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: {operation} reports {read_path}"));
        let second_read = dates(&second)
            .get(read_path)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: {operation} reports {read_path}"));
        assert_eq!(
            first_read, written,
            "case {case:?}: expected the read to report the instant the create reported \
             ({written}), got {first_read}"
        );
        assert_eq!(
            second_read, first_read,
            "case {case:?}: expected two reads of an unmodified resource to report the \
             same instant ({first_read}), got {second_read}"
        );
    }
}

/// The digit-counting the rule is read with, pinned on values whose rendering is known, so a
/// failure above is about the service and not about the helper: one instant per resolution,
/// from whole seconds to the nanoseconds a host clock offers.
#[tokio::test]
async fn the_fraction_is_counted_from_the_rendering_a_client_sees() {
    // (case, rendered seconds, expected fractional digits)
    let rows = [
        ("a whole second", 1_794_991_550.0_f64, 0),
        ("a tenth", 1_794_991_550.2_f64, 1),
        ("a hundredth", 1_794_991_550.21_f64, 2),
        ("a millisecond", 1_794_991_550.211_f64, 3),
        ("a microsecond", 1_794_991_550.211_196_f64, 6),
        ("a nanosecond", 1_794_991_550.211_196_3_f64, 7),
    ];
    for (case, seconds, expected) in rows {
        let digits = fractional_digits(seconds);
        assert_eq!(
            digits, expected,
            "case {case:?}: expected {expected} fractional digits in {seconds}, got {digits}"
        );
    }
    // Only the first four rows are at or inside the resolution the service renders at.
    for (case, seconds, _) in &rows[..4] {
        assert!(
            fractional_digits(*seconds) <= MOST_FRACTIONAL_DIGITS,
            "case {case:?}: {seconds} is at millisecond resolution"
        );
    }
    for (case, seconds, _) in &rows[4..] {
        assert!(
            fractional_digits(*seconds) > MOST_FRACTIONAL_DIGITS,
            "case {case:?}: {seconds} is finer than millisecond resolution"
        );
    }
}
