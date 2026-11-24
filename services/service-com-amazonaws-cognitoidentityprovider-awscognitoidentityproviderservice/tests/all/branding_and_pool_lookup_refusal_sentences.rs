// SPDX-License-Identifier: CC0-1.0

//! What a refused `CreateUserPoolDomain`, `CreateUserPoolReplica` or
//! `CreateManagedLoginBranding` SAYS, driven through the router exactly as an SDK
//! drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rules under test:
//!
//! * A refusal of a `UserPoolId` no stored pool answers to carries the asking
//!   operation's own fixed text with exactly ONE variable part, the `UserPoolId`
//!   the REQUEST carried, copied verbatim and interpolated nowhere else:
//!   `CreateUserPoolDomain` and `CreateUserPoolReplica` join `UserPool with ID: `
//!   to `does not exist.` with no separator at all, while
//!   `CreateManagedLoginBranding` words the same lookup `User pool <UserPoolId>
//!   does not exist.` A `UserPoolId` past the modelled length is refused by that
//!   constraint instead, so the lookup sentence is never reached.
//! * A `CreateManagedLoginBranding` that asks for the service-managed default
//!   style and supplies customizations as well is refused with ONE sentence that
//!   is fixed text in full: it interpolates nothing, whatever the request names
//!   and however much customization it carries.
//!
//! The rows of each table differ along what its rule generalises over — the form
//! and length of the `UserPoolId` asked for, the operation asking, and which
//! member carries the customization and how many image files it holds — and
//! every assertion names its case and reports the value it actually saw. Each
//! refusal is followed by a read proving the refusal stored nothing.

use serde_json::{json, Map, Value};

use crate::managed_login_branding::{
    asset, call, client, error_code, error_message, ok, pool, router, CATEGORIES, COLOR_MODES,
    EXTENSIONS,
};

/// The one sentence a request that asks for the service-managed defaults and
/// customizes the style as well is refused with: fixed text in full, naming the
/// members in the casing the service uses.
const DEFAULTS_OR_SETTINGS: &str =
    "useCognitoProvidedValues or settings should be specified (but not both)";

/// The modelled length maximum of `UserPoolIdType`
/// (`smithy_query(query="shape",shape="UserPoolIdType")`, `@length` 1..55 with
/// pattern `^[\w-]+_[0-9a-zA-Z]+$`).
const MAX_USER_POOL_ID_LENGTH: usize = 55;

/// The modelled length maximum of `AssetListType`
/// (`smithy_query(query="operation",operation="CreateManagedLoginBranding")`).
const MAX_ASSETS: usize = 40;

/// A `ClientIdType`-shaped app client id (pattern `^[\w+]+$`,
/// `smithy_query(query="shape",shape="ClientIdType")`) carried beside a
/// `UserPoolId` no create minted, so the POOL lookup is what the request fails
/// on rather than the app-client one.
const SOME_CLIENT_ID: &str = "neverminted1234";

/// How an operation words a `UserPoolId` no stored pool answers to: the sentence
/// belongs to the operation that answers it, not to the resource kind.
#[derive(Debug, Clone, Copy)]
enum Sentence {
    /// `UserPool with ID: ` + the requested id + `does not exist.`, joined with
    /// no separator before `does`.
    WithIdUnspaced,
    /// `User pool ` + the requested id + ` does not exist.`
    WithId,
}

impl Sentence {
    /// The whole sentence this wording gives for `user_pool_id`.
    fn rendered(self, user_pool_id: &str) -> String {
        match self {
            Self::WithIdUnspaced => format!("UserPool with ID: {user_pool_id}does not exist."),
            Self::WithId => format!("User pool {user_pool_id} does not exist."),
        }
    }
}

/// `UserPoolId`s satisfying `UserPoolIdType`'s pattern that no create minted, so
/// a LOOKUP is what fails rather than a modelled constraint. They differ along
/// the form and the length of the id: the shortest the pattern admits, a
/// lower-case suffix, a numeric one, an upper-case one, an id whose prefix itself
/// carries underscores, and one at the published length maximum.
fn absent_pool_ids() -> Vec<(&'static str, String)> {
    let longest = format!(
        "us-west-2_{}",
        "a".repeat(MAX_USER_POOL_ID_LENGTH - "us-west-2_".len())
    );
    vec![
        ("the shortest id the pattern admits", "a_b".to_string()),
        ("a lower-case suffix", "us-east-1_absentpool".to_string()),
        ("a numeric suffix", "eu-west-2_0000000000".to_string()),
        ("an upper-case suffix", "us-west-2_ZZZZZZZZZZ".to_string()),
        ("an underscored prefix", "my_own_pool_Abc123".to_string()),
        ("an id at the modelled 55-character maximum", longest),
    ]
}

/// `body` with `UserPoolId` set to `user_pool_id` and the members `beside`
/// carries added, which are the ones each operation needs besides the pool.
fn request(user_pool_id: &str, beside: &Value) -> Value {
    let mut body = Map::new();
    body.insert("UserPoolId".to_string(), json!(user_pool_id));
    for (member, value) in beside.as_object().expect("the members are an object") {
        body.insert(member.clone(), value.clone());
    }
    Value::Object(body)
}

#[tokio::test]
async fn a_pool_lookup_refusal_interpolates_only_the_pool_id_the_request_carried() {
    // (the operation, the wording it answers, the members it carries beside the
    // pool id, a request value the sentence must NOT name). The two operations
    // the sentence without a separator belongs to, and one that words the same
    // lookup its own way, so the wording is pinned to the asking operation.
    let operations: &[(&str, Sentence, Value, Option<&str>)] = &[
        (
            "CreateUserPoolDomain",
            Sentence::WithIdUnspaced,
            json!({ "Domain": "domain-of-an-absent-pool" }),
            Some("domain-of-an-absent-pool"),
        ),
        (
            "CreateUserPoolReplica",
            Sentence::WithIdUnspaced,
            json!({ "RegionName": "ap-south-1" }),
            Some("ap-south-1"),
        ),
        (
            "CreateManagedLoginBranding",
            Sentence::WithId,
            json!({ "ClientId": SOME_CLIENT_ID, "UseCognitoProvidedValues": true }),
            Some(SOME_CLIENT_ID),
        ),
    ];

    let router = router();
    // One pool that DOES exist, so a refusal cannot be the store being empty.
    let live_pool = pool(&router, "pool-lookup-sentence-live").await;

    for (way, absent) in absent_pool_ids() {
        for (operation, sentence, beside, forbidden) in operations {
            let case = format!("{operation} asked for {way}, {absent}");
            let (status, payload) = call(&router, operation, &request(&absent, beside)).await;
            assert_eq!(
                (status, error_code(&payload)),
                (400, "ResourceNotFoundException"),
                "case {case:?}: expected a 400 ResourceNotFoundException, got {status} \
                 with {payload}"
            );
            let message = error_message(&payload);
            let expected = sentence.rendered(&absent);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            // The id the request carried is the one variable part, so it reaches
            // the sentence once and nothing else the request carried reaches it.
            assert_eq!(
                message.matches(absent.as_str()).count(),
                1,
                "case {case:?}: the pool id must appear once, got {message:?}"
            );
            if let Some(forbidden) = forbidden {
                assert!(
                    !message.contains(forbidden),
                    "case {case:?}: the sentence must not name {forbidden:?}, got {message:?}"
                );
            }
            // Whether a space separates the id from `does` is part of each
            // operation's fixed text, so it is asserted on its own: the two
            // operations above join the halves with none.
            let spaced = format!("{absent} does");
            match sentence {
                Sentence::WithIdUnspaced => assert!(
                    !message.contains(&spaced),
                    "case {case:?}: no separator precedes `does`, got {message:?}"
                ),
                Sentence::WithId => assert!(
                    message.contains(&spaced),
                    "case {case:?}: a space precedes `does`, got {message:?}"
                ),
            }
        }
    }

    // One character past the published maximum: the modelled constraint is what
    // refuses it, so no lookup sentence is reached.
    let over_long = format!(
        "us-west-2_{}",
        "a".repeat(MAX_USER_POOL_ID_LENGTH + 1 - "us-west-2_".len())
    );
    for (operation, _, beside, _) in operations {
        let case = format!("{operation} asked for an id past the modelled maximum");
        let (status, payload) = call(&router, operation, &request(&over_long, beside)).await;
        assert_eq!(
            (status, error_code(&payload)),
            (400, "InvalidParameterException"),
            "case {case:?}: expected a 400 InvalidParameterException, got {status} with {payload}"
        );
        let message = error_message(&payload);
        assert!(
            !message.contains("does not exist"),
            "case {case:?}: the constraint is reported, not a lookup, got {message:?}"
        );
    }

    // None of those refusals configured anything: the stored pool still takes the
    // domain they were refused and reads it back, and it holds its own primary
    // replica alone.
    let case = "the stored pool after the refusals";
    ok(
        &router,
        "CreateUserPoolDomain",
        &json!({ "UserPoolId": live_pool, "Domain": "pool-lookup-sentence-live-domain" }),
    )
    .await;
    let described = ok(
        &router,
        "DescribeUserPoolDomain",
        &json!({ "Domain": "pool-lookup-sentence-live-domain" }),
    )
    .await;
    assert_eq!(
        described["DomainDescription"]["UserPoolId"],
        json!(live_pool),
        "case {case:?}: the stored domain must read back on its pool, got {described}"
    );
    let replicas = ok(
        &router,
        "ListUserPoolReplicas",
        &json!({ "UserPoolId": live_pool }),
    )
    .await;
    assert_eq!(
        replicas["UserPoolReplicas"]
            .as_array()
            .map(|listed| listed.len()),
        Some(1),
        "case {case:?}: the pool reports its own primary replica alone, got {replicas}"
    );
}

/// One row: the customization a request supplies ALONGSIDE
/// `UseCognitoProvidedValues: true`.
struct Conflict {
    case: &'static str,
    customization: Value,
}

/// The rows. They differ along which member carries the customization and how
/// much of it there is: a settings document naming one option, a document naming
/// many, one image file, and an image list at the modelled `AssetListType`
/// maximum together with a settings document.
fn conflicts() -> Vec<Conflict> {
    let many_assets: Vec<Value> = (0..MAX_ASSETS)
        .map(|index| {
            asset(
                CATEGORIES[index % CATEGORIES.len()],
                COLOR_MODES[index % COLOR_MODES.len()],
                EXTENSIONS[index % EXTENSIONS.len()],
                format!("image {index}").as_bytes(),
                None,
            )
        })
        .collect();
    vec![
        Conflict {
            case: "the defaults with a settings document naming one option",
            customization: json!({ "Settings": { "components": { "primaryButton": {} } } }),
        },
        Conflict {
            case: "the defaults with a settings document naming several options",
            customization: json!({
                "Settings": {
                    "componentClasses": { "buttons": { "borderRadius": 4 } },
                    "components": { "pageHeader": {}, "pageFooter": {} },
                },
            }),
        },
        Conflict {
            case: "the defaults with one image file",
            customization: json!({
                "Assets": [asset(CATEGORIES[11], COLOR_MODES[1], EXTENSIONS[2], b"a page background", None)],
            }),
        },
        Conflict {
            case: "the defaults with a settings document and the modelled maximum of image files",
            customization: json!({
                "Settings": { "components": { "form": {} } },
                "Assets": many_assets,
            }),
        },
    ]
}

#[tokio::test]
async fn the_defaults_and_customizations_conflict_carries_one_sentence_naming_no_request_value() {
    let router = router();
    let user_pool_id = pool(&router, "defaults-conflict-sentence-pool").await;

    for conflict in conflicts() {
        let case = conflict.case;
        // One app client per row, so the answer is about the parameters the row
        // carries and not about a style the client already has.
        let client_id = client(&router, &user_pool_id, case).await;
        let mut beside = conflict
            .customization
            .as_object()
            .cloned()
            .expect("each row's customization is an object");
        beside.insert("ClientId".to_string(), json!(client_id));
        beside.insert("UseCognitoProvidedValues".to_string(), json!(true));
        let body = request(&user_pool_id, &Value::Object(beside));

        let (status, payload) = call(&router, "CreateManagedLoginBranding", &body).await;
        assert_eq!(
            (status, error_code(&payload)),
            (400, "InvalidParameterException"),
            "case {case:?}: expected a 400 InvalidParameterException, got {status} with {payload}"
        );
        let message = error_message(&payload);
        assert_eq!(
            message, DEFAULTS_OR_SETTINGS,
            "case {case:?}: expected {DEFAULTS_OR_SETTINGS:?}, got {message:?}"
        );
        // The sentence above is fixed text in full; these are the values the
        // request carried that it must therefore not interpolate.
        for (what, value) in [
            ("the pool", user_pool_id.as_str()),
            ("the app client", client_id.as_str()),
        ] {
            assert!(
                !message.contains(value),
                "case {case:?}: the sentence must not name {what} ({value:?}), got {message:?}"
            );
        }

        // The refused create stored nothing, so the defaults alone are still this
        // app client's first style, and it reads back as the service's own.
        let created = ok(
            &router,
            "CreateManagedLoginBranding",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientId": client_id,
                "UseCognitoProvidedValues": true,
            }),
        )
        .await;
        let read = ok(
            &router,
            "DescribeManagedLoginBrandingByClient",
            &json!({ "UserPoolId": user_pool_id, "ClientId": client_id }),
        )
        .await;
        assert_eq!(
            read["ManagedLoginBranding"]["ManagedLoginBrandingId"],
            created["ManagedLoginBranding"]["ManagedLoginBrandingId"],
            "case {case:?}: the style the create stored must read back, got {read}"
        );
        assert_eq!(
            read["ManagedLoginBranding"]["UseCognitoProvidedValues"],
            json!(true),
            "case {case:?}: a refused create stores no style, so this one is the client's first, \
             got {read}"
        );
    }
}
