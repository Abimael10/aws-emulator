// SPDX-License-Identifier: CC0-1.0

//! The FORM of a tagging call's `ResourceArn`: which values address a Glue resource at all,
//! and which are refused as the member rather than reported as a missing entity.
//!
//! RULE: a tagging call addresses a resource only through a Glue resource ARN whose resource
//! portion names a Glue resource TYPE — a type segment followed by the name of one resource
//! of that type, the account's own Data Catalog being the one form that is the type segment
//! alone — so a value whose resource portion names no Glue resource type addresses nothing
//! and is refused as invalid input before any lookup, while a value that DOES name one is an
//! address the surface looks an entity up by, and nothing at it is an absence.
//!
//! Both sides are the model's own 400s (`smithy_query operation=GetTags,TagResource,
//! UntagResource`): `InvalidInputException` for the member, `EntityNotFoundException` for the
//! absence. The refusal of the member carries no `Message` member at all.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: whether the resource portion carries
//! a type segment at all (a bare token, a colon-joined value, the empty portion), whether the
//! segment it carries is a Glue resource type (a near miss in number or in case, a type of
//! Glue's that carries no tag set, a taggable type), how many segments follow it, and which
//! of the three tagging operations is asked — each one, since the classification is the
//! surface's and not one operation's.

use serde_json::{json, Value};

use crate::resource_tagging::{
    glue_arn, TAGGING_ENTITY_NOT_FOUND, UNPLACEABLE_RESOURCE_ARN_NOT_FOUND,
};
use crate::tagging_resource_kinds::read_miss_message;
use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// The three operations of the tagging surface, so every form below is asked of all of them.
const TAGGING_OPERATIONS: [&str; 3] = ["GetTags", "TagResource", "UntagResource"];

/// The database the positive controls are served from, and the tag pair its create carries.
const TAGGED_DATABASE: &str = "arn_form_db";

/// The request body `operation` takes for `arn`, with the members its write legs require.
fn tagging_body(operation: &str, arn: &str) -> Value {
    match operation {
        "TagResource" => json!({ "ResourceArn": arn, "TagsToAdd": { "written": "no" } }),
        "UntagResource" => json!({ "ResourceArn": arn, "TagsToRemove": ["stack"] }),
        _ => json!({ "ResourceArn": arn }),
    }
}

/// Create the database every test here reads back, carrying one tag pair.
async fn given_tagged_database(router: &smithy_server_runtime::MultiServiceRouter) {
    ok(
        router,
        "CreateDatabase",
        &json!({
            "DatabaseInput": { "Name": TAGGED_DATABASE },
            "Tags": { "stack": "warehouse" },
        }),
    )
    .await;
}

/// The tag set the created database must still report: exactly what its create seeded, no
/// superset and no subset.
async fn assert_database_tags_served(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
) {
    let arn = glue_arn(REGION, ACCOUNT_ID, &format!("database/{TAGGED_DATABASE}"));
    let served = ok(router, "GetTags", &json!({ "ResourceArn": arn })).await;
    assert_eq!(
        served["Tags"],
        json!({ "stack": "warehouse" }),
        "case {case:?}: the tagged database must report exactly the map its create seeded, \
         got {served}"
    );
}

/// A Glue resource ARN whose resource portion names NO Glue resource type addresses no
/// resource of any kind, so the member itself is refused — 400 `InvalidInputException` with
/// no prose at all — on each of the three tagging operations, and no lookup is reached.
#[tokio::test]
async fn an_arn_whose_resource_portion_names_no_resource_type_is_invalid_input() {
    let router = emulator();
    given_tagged_database(&router).await;

    // (case, the resource portion of an otherwise legal Glue ARN). None of these names a
    // Glue resource type: there is no type segment at all, or the segment is no type of
    // Glue's. The type names come from the one table the crate resolves an ARN against
    // (`TAGGED_RESOURCE_TYPES` and `UNTAGGED_RESOURCE_TYPES` in
    // `src/state/tagged_resources.rs`), so a near miss in number or in case is no type.
    let portions: Vec<(&str, &str)> = vec![
        (
            "a bare token in place of a type and a name",
            "not-a-resource-type",
        ),
        ("another bare token", "nosuchtype"),
        ("a taggable type segment with no name at all", "database"),
        (
            "a type segment Glue has none of, with a name",
            "nosuchtype/orders",
        ),
        (
            "a taggable type segment in the plural",
            "databases/arn_form_db",
        ),
        (
            "a taggable type segment in another case",
            "DATABASE/arn_form_db",
        ),
        ("a colon-joined resource portion", "database:arn_form_db"),
        ("the empty resource portion", ""),
    ];

    for (case, portion) in &portions {
        let arn = glue_arn(REGION, ACCOUNT_ID, portion);
        for operation in TAGGING_OPERATIONS {
            let (status, body) = call(&router, operation, &tagging_body(operation, &arn)).await;
            let case = format!("{case} ({portion:?}) / {operation}");
            assert_eq!(
                status, 400,
                "case {case}: a ResourceArn addressing no Glue resource is refused with 400, \
                 got {status} with {body}"
            );
            assert!(
                error_code(&body).ends_with("InvalidInputException"),
                "case {case}: expected InvalidInputException, got {:?} in {body}",
                error_code(&body)
            );
            // The whole of the text is empty: the refusal carries neither the modelled
            // `Message` member nor the framework's lowercase `message`.
            assert!(
                body.get("Message").is_none() && body.get("message").is_none(),
                "case {case}: expected a refusal stating no message, got {body}"
            );
        }
        assert_database_tags_served(&router, case).await;
    }
}

/// A Glue resource ARN that DOES name a Glue resource type stays an entity lookup, so a type
/// this service places no entity under, and a type whose resource is absent, are both
/// reported as absences and never as a bad parameter.
#[tokio::test]
async fn an_arn_that_names_a_resource_type_is_an_absence_not_a_bad_parameter() {
    let router = emulator();
    given_tagged_database(&router).await;

    // (case, the resource portion). Each names a resource TYPE of Glue's: one that carries
    // no tag set, one whose own ARN is the type segment alone, and taggable ones whose
    // resource portion names no single resource of the type.
    let portions: Vec<(&str, String)> = vec![
        (
            "a catalog table, a type that carries no tag set",
            format!("table/{TAGGED_DATABASE}/orders"),
        ),
        (
            "a user defined function, a type that carries no tag set",
            format!("userDefinedFunction/{TAGGED_DATABASE}/to_upper"),
        ),
        ("a development endpoint", "devEndpoint/notebook".to_string()),
        (
            "a zero-ETL integration",
            "integration/zero-etl-1".to_string(),
        ),
        (
            "the record of one resource's integration properties",
            "integrationresourceproperty/connection/held-connection".to_string(),
        ),
        (
            "the account's own Data Catalog, a type segment with no name",
            "catalog".to_string(),
        ),
        (
            "a taggable type segment with an empty name",
            "database/".to_string(),
        ),
        (
            "a one-segment type carrying a second segment",
            format!("database/{TAGGED_DATABASE}/extra"),
        ),
    ];

    for (case, portion) in &portions {
        let arn = glue_arn(REGION, ACCOUNT_ID, portion);
        for operation in TAGGING_OPERATIONS {
            let (status, body) = call(&router, operation, &tagging_body(operation, &arn)).await;
            let case = format!("{case} ({portion:?}) / {operation}");
            assert_eq!(
                status, 400,
                "case {case}: a Glue ARN that places no entity is refused with 400, got \
                 {status} with {body}"
            );
            assert!(
                error_code(&body).ends_with("EntityNotFoundException"),
                "case {case}: expected EntityNotFoundException, got {:?} in {body}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                UNPLACEABLE_RESOURCE_ARN_NOT_FOUND,
                "case {case}: expected the absence worded {UNPLACEABLE_RESOURCE_ARN_NOT_FOUND:?}, \
                 got {:?}",
                error_message(&body)
            );
        }
    }

    // And a taggable type whose resource this endpoint holds none of keeps the per-surface
    // miss wording of the kind it looked up, which is the other side of the same line: the
    // ARN resolved, the resource was not there.
    for (segment, name) in [("database", "absent_db"), ("job", "absent-job")] {
        let arn = glue_arn(REGION, ACCOUNT_ID, &format!("{segment}/{name}"));
        for operation in TAGGING_OPERATIONS {
            let (status, body) = call(&router, operation, &tagging_body(operation, &arn)).await;
            let case = format!("a {segment} name nothing created / {operation}");
            let expected = if operation == "GetTags" {
                read_miss_message(segment, name)
            } else {
                TAGGING_ENTITY_NOT_FOUND.to_string()
            };
            assert_eq!(
                status, 400,
                "case {case}: an absent resource of a named type is refused with 400, got \
                 {status} with {body}"
            );
            assert!(
                error_code(&body).ends_with("EntityNotFoundException"),
                "case {case}: expected EntityNotFoundException, got {:?} in {body}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                expected,
                "case {case}: expected the absence worded {expected:?}, got {:?}",
                error_message(&body)
            );
        }
    }

    assert_database_tags_served(&router, "every refused form above").await;
}
