// SPDX-License-Identifier: CC0-1.0

//! The ASYNCHRONOUS validation `PutParameter` owes an `aws:ec2:image` parameter, end to end
//! over the front door.
//!
//! RULE under test: a `PutParameter` whose `DataType` is `aws:ec2:image` answers 200 with
//! the version it wrote and only afterwards validates the value against the catalogue of
//! images the account can see — "a successful HTTP 200 response does not guarantee that your
//! parameter was successfully created or updated … If you submit an invalid AMI value, the
//! PutParameter operation will return success, but the asynchronous validation will fail and
//! the parameter will not be created or updated"
//! (`smithy_query(query="doc",shape="PutParameterRequest",member="DataType")`). So a version
//! whose value is no image Amazon EC2 holds for the account is taken back out of the store:
//! a create's only version leaves no parameter behind, so every read of that name is refused
//! `ParameterNotFound` and no listing carries it, while an overwrite's leaves the versions
//! the parameter already had. A version written under any other data type owes no such check
//! and stands whatever its value looks like.
//!
//! Every call goes through the wire a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body for Parameter Store
//! (`OPERATIONS.md`, "Wire protocol"), and an `ec2Query` form POST for the Amazon EC2 read
//! that proves which ids that service really holds. Nothing reaches into a state struct.
//!
//! The rows differ along what the rule generalises over: the FORM of the value (an id of an
//! image the account can see, a well-formed long-form id no image has, the retired short-form
//! id, a value that is not an image id at all) and the `DataType` the value was written under
//! (the one whose check is asynchronous, the default one, and none at all).
//!
//! Nothing here waits a fixed time: the validation is a stored due instant, so a case LOOKS
//! AGAIN until the condition holds or [`DEADLINE`] passes. Every row's put is made before any
//! row is polled, so the spans run together rather than one after another.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{DEADLINE, POLL_PAUSE};
use crate::parameter_store::{call, error_code, ACCOUNT, REGION};
use crate::public_parameter_catalog::emulator;

/// The public Systems Manager parameter the AL2023 image id is resolved through — the id a
/// client stores in Parameter Store as its fleet-wide "current AMI".
const AL2023_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The same for a Windows Server image, so a row that must survive the validation can carry
/// a second id the account really holds.
const WINDOWS_PARAMETER: &str =
    "/aws/service/ami-windows-latest/Windows_Server-2022-English-Full-Base";

/// The `DataType` whose validation is asynchronous
/// (`smithy_query(query="doc",shape="PutParameterRequest",member="DataType")`: "text
/// aws:ec2:image aws:ssm:integration").
const DATA_TYPE_EC2_IMAGE: &str = "aws:ec2:image";

/// The `DataType` a parameter carries when the request names none (the same doc: "The
/// default is text").
const DATA_TYPE_TEXT: &str = "text";

/// A well-formed LONG-form AMI id — `ami-` and 17 lowercase hexadecimal digits — that no
/// image of this account has, which is the typo'd or deleted id a client stores by accident.
const ABSENT_LONG_FORM_ID: &str = "ami-0123456789abcdef0";

/// The retired SHORT form of the same thing — `ami-` and 8 digits — so the rows span both id
/// forms Amazon EC2 reads.
const ABSENT_SHORT_FORM_ID: &str = "ami-0abcd123";

/// A value that is no image id at all, which the same validation rejects: it is not "in the
/// required format" either.
const NOT_AN_IMAGE_ID: &str = "latest";

/// What a read of a parameter that never landed is refused with.
const PARAMETER_NOT_FOUND: &str = "ParameterNotFound";

/// One row: a value written under a data type, and whether the parameter it wrote survives
/// the validation the put left owing.
#[derive(Debug, Clone)]
struct Case {
    case: &'static str,
    /// The leaf name the row writes, under a hierarchy of its own.
    name: String,
    /// The value written.
    value: String,
    /// The `DataType` the request named, or `None` to name none.
    data_type: Option<&'static str>,
    /// Whether the parameter is still readable once the validation has fallen due.
    survives: bool,
}

/// The image id the public Systems Manager parameter `name` resolves to, read through this
/// service's own `GetParameter` — so the ids the rows call valid are the ids the catalogue
/// publishes rather than ones written here.
async fn published_image_id(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(router, "GetParameter", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "GetParameter {name} must resolve an image id, got {status} with {body}"
    );
    body["Parameter"]["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("GetParameter {name} reports a value, got {body}"))
        .to_string()
}

/// Whether Amazon EC2 itself holds an image under `image_id`, asked through its own
/// `DescribeImages` over the `ec2Query` wire — the catalogue the validation is decided
/// against, read here through the service that OWNS it.
async fn ec2_holds_image(router: &MultiServiceRouter, image_id: &str) -> bool {
    let body = format!("Action=DescribeImages&Version=2016-11-15&ImageId.1={image_id}");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://ec2.{REGION}.amazonaws.com/"))
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/ec2/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed form request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    status == 200 && text.contains(&format!("<imageId>{image_id}</imageId>"))
}

/// Write one version and answer the version number the put reported.
///
/// The put MUST answer 200 whatever the value is: that is the whole point of an
/// asynchronous validation, and a refusal here would be a different service.
async fn put(router: &MultiServiceRouter, row: &Case, overwrite: bool) -> i64 {
    let mut body = json!({
        "Name": row.name,
        "Type": "String",
        "Value": row.value,
    });
    if let Some(data_type) = row.data_type {
        body["DataType"] = json!(data_type);
    }
    if overwrite {
        body["Overwrite"] = json!(true);
    }
    let (status, answered) = call(router, "PutParameter", &body).await;
    assert_eq!(
        status, 200, "case {:?}: PutParameter must answer success whatever the value is, got {status} with {answered}",
        row.case
    );
    answered["Version"].as_i64().unwrap_or_else(|| {
        panic!(
            "case {:?}: PutParameter reports the version it wrote, got {answered}",
            row.case
        )
    })
}

/// `GetParameter` for `name`, as `(status, body)`.
async fn get(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    call(router, "GetParameter", &json!({ "Name": name })).await
}

/// Look again until `name` is refused `ParameterNotFound`, or [`DEADLINE`] passes.
async fn gone_when_validated(router: &MultiServiceRouter, case: &str, name: &str) {
    let until = std::time::Instant::now() + DEADLINE;
    loop {
        let (status, body) = get(router, name).await;
        if status != 200 {
            assert_eq!(
                error_code(&body),
                PARAMETER_NOT_FOUND,
                "case {case:?}: expected {PARAMETER_NOT_FOUND} once the validation failed, \
                 got {status} with {body}"
            );
            return;
        }
        assert!(
            std::time::Instant::now() < until,
            "case {case:?}: expected a parameter whose aws:ec2:image value failed validation \
             to stop being readable, and GetParameter still answered {body}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The rows, every one written before any one is polled.
async fn rows(router: &MultiServiceRouter) -> Vec<Case> {
    let al2023 = published_image_id(router, AL2023_PARAMETER).await;
    let windows = published_image_id(router, WINDOWS_PARAMETER).await;
    assert!(
        ec2_holds_image(router, &al2023).await && ec2_holds_image(router, &windows).await,
        "the ids the valid rows carry must be images Amazon EC2 holds: {al2023}, {windows}"
    );
    for absent in [ABSENT_LONG_FORM_ID, ABSENT_SHORT_FORM_ID] {
        assert!(
            !ec2_holds_image(router, absent).await,
            "the id {absent} the invalid rows carry must be one Amazon EC2 holds no image for"
        );
    }
    vec![
        Case {
            case: "an image id the account can see",
            name: "/fleet/ami/al2023".to_string(),
            value: al2023,
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            survives: true,
        },
        Case {
            case: "a second image id the account can see",
            name: "/fleet/ami/windows".to_string(),
            value: windows,
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            survives: true,
        },
        Case {
            case: "a well-formed long-form id no image has",
            name: "/fleet/ami/absent-long".to_string(),
            value: ABSENT_LONG_FORM_ID.to_string(),
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            survives: false,
        },
        Case {
            case: "a well-formed short-form id no image has",
            name: "/fleet/ami/absent-short".to_string(),
            value: ABSENT_SHORT_FORM_ID.to_string(),
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            survives: false,
        },
        Case {
            case: "a value that is no image id at all",
            name: "/fleet/ami/not-an-id".to_string(),
            value: NOT_AN_IMAGE_ID.to_string(),
            data_type: Some(DATA_TYPE_EC2_IMAGE),
            survives: false,
        },
        Case {
            case: "the same absent id written as text",
            name: "/fleet/text/absent-long".to_string(),
            value: ABSENT_LONG_FORM_ID.to_string(),
            data_type: Some(DATA_TYPE_TEXT),
            survives: true,
        },
        Case {
            case: "the same absent id written under no data type",
            name: "/fleet/default/absent-long".to_string(),
            value: ABSENT_LONG_FORM_ID.to_string(),
            data_type: None,
            survives: true,
        },
    ]
}

/// A value written under `aws:ec2:image` lands only when it names an image the account can
/// see; one written under any other data type lands whatever it names.
#[tokio::test]
async fn the_asynchronous_image_validation_discards_a_parameter_no_image_backs() {
    let router = emulator();
    let table = rows(&router).await;
    for row in &table {
        let version = put(&router, row, false).await;
        assert_eq!(
            version, 1,
            "case {:?}: the put of a new parameter reports its first version",
            row.case
        );
    }
    for row in &table {
        if row.survives {
            let (status, body) = get(&router, &row.name).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected the parameter to stay readable, got {status} with {body}",
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
        } else {
            gone_when_validated(&router, row.case, &row.name).await;
        }
    }
}

/// A parameter the validation discarded is gone from every read of Parameter Store, not only
/// from the read of its own name: it was never created, so no listing carries it and a later
/// create of the same name needs no `Overwrite`.
#[tokio::test]
async fn a_discarded_parameter_is_absent_from_every_read() {
    let router = emulator();
    let valid = published_image_id(&router, AL2023_PARAMETER).await;
    let kept = Case {
        case: "the parameter that survives",
        name: "/estate/ami/current".to_string(),
        value: valid.clone(),
        data_type: Some(DATA_TYPE_EC2_IMAGE),
        survives: true,
    };
    let discarded = Case {
        case: "the parameter the validation discards",
        name: "/estate/ami/typo".to_string(),
        value: ABSENT_LONG_FORM_ID.to_string(),
        data_type: Some(DATA_TYPE_EC2_IMAGE),
        survives: false,
    };
    put(&router, &kept, false).await;
    put(&router, &discarded, false).await;
    gone_when_validated(&router, discarded.case, &discarded.name).await;

    // The metadata listing of the hierarchy both were written into.
    let (status, body) = call(
        &router,
        "DescribeParameters",
        &json!({
            "ParameterFilters": [{
                "Key": "Path",
                "Option": "Recursive",
                "Values": ["/estate"],
            }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeParameters must answer, got {status} with {body}"
    );
    let described: Vec<&str> = body["Parameters"]
        .as_array()
        .map(|held| {
            held.iter()
                .filter_map(|entry| entry["Name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        described,
        vec![kept.name.as_str()],
        "case {:?}: expected only the parameter that survived to be described, got {body}",
        discarded.case
    );

    // The hierarchy read of the same path.
    let (status, body) = call(
        &router,
        "GetParametersByPath",
        &json!({ "Path": "/estate/ami", "Recursive": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetParametersByPath must answer, got {status} with {body}"
    );
    let read: Vec<&str> = body["Parameters"]
        .as_array()
        .map(|held| {
            held.iter()
                .filter_map(|entry| entry["Name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        read,
        vec![kept.name.as_str()],
        "case {:?}: expected only the parameter that survived to be read, got {body}",
        discarded.case
    );

    // "the parameter will not be created": a later create of that name is a create, so it
    // needs no `Overwrite` and starts at version 1 again.
    let created = Case {
        value: valid,
        ..discarded.clone()
    };
    let version = put(&router, &created, false).await;
    assert_eq!(
        version, 1,
        "case {:?}: expected the name to be free to create again, got version {version}",
        discarded.case
    );
    let (status, body) = get(&router, &created.name).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected the re-created parameter to be readable, got {status} with {body}",
        discarded.case
    );
    assert_eq!(
        body["Parameter"]["ARN"].as_str(),
        Some(format!("arn:aws:ssm:{REGION}:{ACCOUNT}:parameter{}", created.name).as_str()),
        "case {:?}: expected the parameter's own ARN, got {body}",
        discarded.case
    );
}

/// "the parameter will not be created OR UPDATED": an overwrite whose value fails the
/// validation leaves the versions the parameter already had, so the previous value is what a
/// read still reports.
#[tokio::test]
async fn an_overwrite_that_fails_validation_leaves_the_version_it_would_have_replaced() {
    let router = emulator();
    let valid = published_image_id(&router, AL2023_PARAMETER).await;
    let case = "an overwrite whose AMI does not exist";
    let row = Case {
        case,
        name: "/estate/ami/overwritten".to_string(),
        value: valid.clone(),
        data_type: Some(DATA_TYPE_EC2_IMAGE),
        survives: true,
    };
    assert_eq!(
        put(&router, &row, false).await,
        1,
        "case {case:?}: the create writes the first version"
    );
    let replacement = Case {
        value: ABSENT_LONG_FORM_ID.to_string(),
        ..row.clone()
    };
    assert_eq!(
        put(&router, &replacement, true).await,
        2,
        "case {case:?}: the overwrite answers success with the version it wrote"
    );

    // Look again until the version the overwrite wrote has been taken back out: the
    // parameter stays readable throughout, at the value the create wrote.
    let until = std::time::Instant::now() + DEADLINE;
    loop {
        let (status, body) = get(&router, &row.name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the parameter must stay readable, got {status} with {body}"
        );
        if body["Parameter"]["Version"].as_i64() == Some(1) {
            assert_eq!(
                body["Parameter"]["Value"].as_str(),
                Some(valid.as_str()),
                "case {case:?}: expected the value the create wrote, got {body}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "case {case:?}: expected the discarded overwrite to leave version 1 newest, and \
             GetParameter still answered {body}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }

    // The history of the parameter holds the surviving version and nothing of the discarded
    // one, so the version the overwrite wrote left no trace at all.
    let (status, body) = call(
        &router,
        "GetParameterHistory",
        &json!({ "Name": row.name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetParameterHistory must answer, got {status} with {body}"
    );
    let versions: Vec<i64> = body["Parameters"]
        .as_array()
        .map(|held| {
            held.iter()
                .filter_map(|entry| entry["Version"].as_i64())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        versions,
        vec![1],
        "case {case:?}: expected only the surviving version in the history, got {body}"
    );
}
