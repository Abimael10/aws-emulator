// SPDX-License-Identifier: CC0-1.0

//! The WINDOW a client can observe around an `aws:ec2:image` put — that there is none — end
//! to end over the front door.
//!
//! RULE under test: a write whose value Parameter Store validates against another service's
//! catalogue becomes part of what the store serves only when that validation passes. The
//! call still answers success with the version number it allocated — "the PutParameter
//! operation will return success, but the asynchronous validation will fail and the
//! parameter will not be created or updated"
//! (`smithy_query(query="doc",shape="PutParameterRequest",member="DataType")`), and
//! `PutParameter` models no error for such a value — so the only thing a failed validation
//! changes is NOTHING: at the very next call the parameter is exactly as the put found it.
//! A create leaves no parameter behind and the name stays free to create, an overwrite
//! leaves the stored version, its value and its data type in place and adds nothing to the
//! parameter's history. Which data type is IN FORCE decides whether the validation is made
//! at all, so an update that names `text` lands a value no image backs.
//!
//! This is the half of the rule a span cannot express: every read here is made IMMEDIATELY
//! after the put, with no looking again and nothing waited for, because an unvalidated value
//! must never be served — not even once. The complementary suite, which spans the value
//! forms and data types the validation covers, is
//! [`crate::parameter_image_data_type_validation`].
//!
//! Every call goes through the wire a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body for Parameter Store
//! (`OPERATIONS.md`, "Wire protocol"), and an `ec2Query` form POST for the Amazon EC2 read
//! that proves which ids that service really holds. Nothing reaches into a state struct.
//!
//! The rows differ along what the rule generalises over: the form of the value (two ids of
//! images the account can see, a well-formed long-form id no image has, the retired
//! short-form id, a value that is no image id at all), the data type in force (the one
//! carrying the check, the default one, none at all, and the integration one), and whether
//! the put CREATES the parameter or OVERWRITES an existing one.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_image_data_type_validation::{
    ec2_holds_image, ABSENT_LONG_FORM_ID, ABSENT_SHORT_FORM_ID, DATA_TYPE_EC2_IMAGE,
    DATA_TYPE_TEXT, NOT_AN_IMAGE_ID, PARAMETER_NOT_FOUND,
};
use crate::parameter_store::{call, error_code};
use crate::public_parameter_catalog::{emulator, AMAZON_LINUX_NAMESPACE, WINDOWS_NAMESPACE};

/// The third `ParameterDataType` value, which names a subscription rather than an image
/// (`smithy_query(query="doc",shape="PutParameterRequest",member="DataType")`: "text
/// aws:ec2:image aws:ssm:integration"), so a row can show that the check belongs to ONE data
/// type rather than to every value the member takes.
const DATA_TYPE_INTEGRATION: &str = "aws:ssm:integration";

/// The published AMI-alias namespaces the valid rows take their ids from — AWS's own
/// published hierarchy, so a valid row carries an id the catalogue really resolves instead of
/// one written here.
fn published_ami_namespaces() -> [&'static str; 2] {
    [AMAZON_LINUX_NAMESPACE, WINDOWS_NAMESPACE]
}

/// Image ids the account can see, read out of the published AMI aliases and each confirmed
/// against Amazon EC2's own `DescribeImages`.
async fn image_ids_the_account_can_see(router: &MultiServiceRouter) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for namespace in published_ami_namespaces() {
        let (status, body) = call(
            router,
            "GetParametersByPath",
            &json!({ "Path": namespace, "Recursive": true }),
        )
        .await;
        assert_eq!(
            status, 200,
            "GetParametersByPath {namespace} must answer, got {status} with {body}"
        );
        let values: Vec<String> = body["Parameters"]
            .as_array()
            .map(|held| {
                held.iter()
                    .filter_map(|entry| entry["Value"].as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        for value in values {
            if !ids.contains(&value) {
                ids.push(value);
            }
        }
    }
    assert!(
        ids.len() >= 2,
        "the published AMI aliases must resolve at least two distinct image ids, got {ids:?}"
    );
    for id in &ids {
        assert!(
            ec2_holds_image(router, id).await,
            "the id {id} a valid row carries must be one Amazon EC2 holds an image for"
        );
    }
    for absent in [ABSENT_LONG_FORM_ID, ABSENT_SHORT_FORM_ID, NOT_AN_IMAGE_ID] {
        assert!(
            !ec2_holds_image(router, absent).await,
            "the value {absent} an invalid row carries must be one Amazon EC2 holds no image for"
        );
    }
    ids
}

/// One `PutParameter`, as the version number it answered.
///
/// The put MUST answer 200 whatever the value is: `PutParameter` models no error for an AMI
/// id the account cannot see, and a refusal here would be a different service.
async fn put(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    value: &str,
    data_type: Option<&str>,
    overwrite: bool,
) -> i64 {
    let mut body = json!({ "Name": name, "Type": "String", "Value": value });
    if let Some(data_type) = data_type {
        body["DataType"] = json!(data_type);
    }
    if overwrite {
        body["Overwrite"] = json!(true);
    }
    let (status, answered) = call(router, "PutParameter", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutParameter must answer success whatever the value is, got {status} \
         with {answered}"
    );
    answered["Version"].as_i64().unwrap_or_else(|| {
        panic!("case {case:?}: PutParameter reports the version it allocated, got {answered}")
    })
}

/// `GetParameter` for `name`, as `(status, body)`.
async fn get(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "GetParameter", &json!({ "Name": name })).await
}

/// The versions `GetParameterHistory` reports for `name`, with the value each holds.
async fn history(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<(i64, String)> {
    let (status, body) = call(router, "GetParameterHistory", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetParameterHistory must answer, got {status} with {body}"
    );
    body["Parameters"]
        .as_array()
        .map(|held| {
            held.iter()
                .filter_map(|entry| {
                    Some((
                        entry["Version"].as_i64()?,
                        entry["Value"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The names `DescribeParameters` reports under `path`.
async fn described_under(router: &MultiServiceRouter, path: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        "DescribeParameters",
        &json!({
            "ParameterFilters": [{ "Key": "Path", "Option": "Recursive", "Values": [path] }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeParameters under {path} must answer, got {status} with {body}"
    );
    let mut names: Vec<String> = body["Parameters"]
        .as_array()
        .map(|held| {
            held.iter()
                .filter_map(|entry| entry["Name"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// One row of the create table: a value written under a data type, and whether the create
/// LANDS.
#[derive(Debug, Clone)]
struct Create {
    case: &'static str,
    leaf: &'static str,
    value: String,
    data_type: Option<&'static str>,
    lands: bool,
}

/// A create whose value fails the validation creates nothing a client can ever read: the
/// name is refused `ParameterNotFound` at the very next call, no listing carries it, and it
/// is still free to create.
#[tokio::test]
async fn a_create_whose_image_value_fails_validation_is_never_served() {
    let router = emulator();
    let seen = image_ids_the_account_can_see(&router).await;
    let hierarchy = "/image-gate/create";
    let table = vec![
        Create {
            case: "an image id the account can see",
            leaf: "al",
            value: seen[0].clone(),
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            lands: true,
        },
        Create {
            case: "a second image id the account can see",
            leaf: "second",
            value: seen[1].clone(),
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            lands: true,
        },
        Create {
            case: "a well-formed long-form id no image has",
            leaf: "absent-long",
            value: ABSENT_LONG_FORM_ID.to_string(),
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            lands: false,
        },
        Create {
            case: "the retired short-form id no image has",
            leaf: "absent-short",
            value: ABSENT_SHORT_FORM_ID.to_string(),
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            lands: false,
        },
        Create {
            case: "a value that is no image id at all",
            leaf: "not-an-id",
            value: NOT_AN_IMAGE_ID.to_string(),
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            lands: false,
        },
        Create {
            case: "the same absent id under the default data type",
            leaf: "text",
            value: ABSENT_LONG_FORM_ID.to_string(),
            data_type: Some(DATA_TYPE_TEXT),
            lands: true,
        },
        Create {
            case: "the same absent id under no data type at all",
            leaf: "default",
            value: ABSENT_LONG_FORM_ID.to_string(),
            data_type: None,
            lands: true,
        },
        Create {
            case: "the same absent id under the integration data type",
            leaf: "integration",
            value: ABSENT_LONG_FORM_ID.to_string(),
            data_type: Some(DATA_TYPE_INTEGRATION),
            lands: true,
        },
    ];

    let mut landed: Vec<String> = Vec::new();
    for row in &table {
        let name = format!("{hierarchy}/{}", row.leaf);
        let version = put(
            &router,
            row.case,
            &name,
            &row.value,
            row.data_type,
            false,
        )
        .await;
        assert_eq!(
            version, 1,
            "case {:?}: the put of a new parameter reports the first version it allocated, \
             got {version}",
            row.case
        );
        // Immediately: no span, no looking again. An unvalidated value is never served.
        let (status, body) = get(&router, &name).await;
        if row.lands {
            assert_eq!(
                status, 200,
                "case {:?}: expected the parameter to be readable, got {status} with {body}",
                row.case
            );
            assert_eq!(
                body["Parameter"]["Value"].as_str(),
                Some(row.value.as_str()),
                "case {:?}: expected the stored value, got {body}",
                row.case
            );
            assert_eq!(
                body["Parameter"]["DataType"].as_str(),
                Some(row.data_type.unwrap_or(DATA_TYPE_TEXT)),
                "case {:?}: expected the data type in force, got {body}",
                row.case
            );
            landed.push(name.clone());
        } else {
            assert_ne!(
                status, 200,
                "case {:?}: expected no parameter to have been created, got {status} with \
                 {body}",
                row.case
            );
            assert_eq!(
                error_code(&body),
                PARAMETER_NOT_FOUND,
                "case {:?}: expected {PARAMETER_NOT_FOUND} for a parameter that was never \
                 created, got {status} with {body}",
                row.case
            );
        }
    }

    // "the parameter will not be created": the metadata listing of the hierarchy carries the
    // rows that landed and nothing of the rows that did not.
    landed.sort();
    assert_eq!(
        described_under(&router, hierarchy).await,
        landed,
        "expected only the creates that passed validation to be described"
    );

    // …and a name no create landed under is still free to create, so a put of a value that
    // does pass is a CREATE rather than an overwrite of something unreadable.
    let freed = format!("{hierarchy}/absent-long");
    let case = "the name a failed create left free";
    let version = put(
        &router,
        case,
        &freed,
        &seen[0],
        Some(DATA_TYPE_EC2_IMAGE),
        false,
    )
    .await;
    assert_eq!(
        version, 1,
        "case {case:?}: expected the name to be free to create again, got version {version}"
    );
    let (status, body) = get(&router, &freed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the created parameter to be readable, got {status} with {body}"
    );
    assert_eq!(
        body["Parameter"]["Value"].as_str(),
        Some(seen[0].as_str()),
        "case {case:?}: expected the value that passed validation, got {body}"
    );
}

/// An overwrite whose value fails the validation leaves the parameter exactly as it found
/// it: the stored version, its value and its data type are what the very next read reports,
/// and the parameter's history gains nothing. An overwrite whose value passes lands, and so
/// does one that puts a data type carrying no such check in force.
#[tokio::test]
async fn an_overwrite_whose_image_value_fails_validation_leaves_the_stored_version() {
    let router = emulator();
    let seen = image_ids_the_account_can_see(&router).await;
    let name = "/image-gate/overwrite/current";
    let validated = seen[0].clone();
    let case = "the create that stores a validated image id";
    assert_eq!(
        put(
            &router,
            case,
            name,
            &validated,
            Some(DATA_TYPE_EC2_IMAGE),
            false
        )
        .await,
        1,
        "case {case:?}: the create writes the first version"
    );

    // Every value the validation rejects, over the same parameter: each answers success with
    // the version it allocated, and each leaves version 1 exactly where it was.
    for row in [ABSENT_LONG_FORM_ID, ABSENT_SHORT_FORM_ID, NOT_AN_IMAGE_ID] {
        let case = "an overwrite whose value names no image the account can see";
        let version = put(&router, case, name, row, Some(DATA_TYPE_EC2_IMAGE), true).await;
        assert_eq!(
            version, 2,
            "case {case:?} with {row:?}: the overwrite answers the version it allocated, got \
             {version}"
        );
        let (status, body) = get(&router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?} with {row:?}: the parameter must stay readable, got {status} with \
             {body}"
        );
        assert_eq!(
            body["Parameter"]["Version"].as_i64(),
            Some(1),
            "case {case:?} with {row:?}: expected the version the create wrote, got {body}"
        );
        assert_eq!(
            body["Parameter"]["Value"].as_str(),
            Some(validated.as_str()),
            "case {case:?} with {row:?}: expected the validated value to be served, got {body}"
        );
        assert_eq!(
            body["Parameter"]["DataType"].as_str(),
            Some(DATA_TYPE_EC2_IMAGE),
            "case {case:?} with {row:?}: expected the stored data type, got {body}"
        );
        assert_eq!(
            history(&router, case, name).await,
            vec![(1, validated.clone())],
            "case {case:?} with {row:?}: expected only validated values in the history"
        );
    }

    // An overwrite whose value DOES name an image the account can see lands, at the next
    // version, and the history grows by exactly that version.
    let case = "an overwrite whose value names an image the account can see";
    let second = seen[1].clone();
    let version = put(
        &router,
        case,
        name,
        &second,
        Some(DATA_TYPE_EC2_IMAGE),
        true,
    )
    .await;
    assert_eq!(
        version, 2,
        "case {case:?}: the overwrite answers the version it wrote, got {version}"
    );
    let (status, body) = get(&router, name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the parameter must be readable, got {status} with {body}"
    );
    assert_eq!(
        body["Parameter"]["Value"].as_str(),
        Some(second.as_str()),
        "case {case:?}: expected the value the overwrite wrote, got {body}"
    );
    assert_eq!(
        history(&router, case, name).await,
        vec![(1, validated), (2, second.clone())],
        "case {case:?}: expected both validated versions in the history"
    );

    // Which data type is IN FORCE is what decides whether the validation is made: an update
    // that names `text` lands a value no image backs.
    let case = "an overwrite that puts the default data type in force";
    let version = put(
        &router,
        case,
        name,
        ABSENT_LONG_FORM_ID,
        Some(DATA_TYPE_TEXT),
        true,
    )
    .await;
    assert_eq!(
        version, 3,
        "case {case:?}: the overwrite answers the version it wrote, got {version}"
    );
    let (status, body) = get(&router, name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the parameter must be readable, got {status} with {body}"
    );
    assert_eq!(
        body["Parameter"]["Value"].as_str(),
        Some(ABSENT_LONG_FORM_ID),
        "case {case:?}: expected the value the overwrite wrote, got {body}"
    );
    assert_eq!(
        body["Parameter"]["DataType"].as_str(),
        Some(DATA_TYPE_TEXT),
        "case {case:?}: expected the data type the overwrite put in force, got {body}"
    );
}
