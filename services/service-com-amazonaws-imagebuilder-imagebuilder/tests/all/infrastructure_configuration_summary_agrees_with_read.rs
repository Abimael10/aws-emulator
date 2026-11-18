// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests pinning that `ListInfrastructureConfigurations` reports the SAME
//! stored record `GetInfrastructureConfiguration` reports.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — with the emulator's own IAM
//! mounted beside imagebuilder on one engine, exactly the way the assembled emulator runs
//! them, so the instance profile a configuration names is one a client really created.
//!
//! The rule under test: a listed entry is the stored configuration rendered as the modelled
//! `InfrastructureConfigurationSummary`, read off the record a read reports — so for every
//! member both shapes name, the listing and the read agree; a member the summary shape does
//! not name is reported by the read alone; a write (a create, a replacing update, a delete)
//! is what moves the listing; and a listing, being a read, leaves what a later caller sees
//! unchanged.
//!
//! The rows span what the rule generalises over: which optional members the configuration
//! carries (none beyond the required ones, a description, a list of instance types, resource
//! tags, its own tags, placement settings, and the members only a read reports), how many
//! configurations the store holds, and whether the record was written by a create, replaced
//! by an update, or removed by a delete.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every imagebuilder call here is signed for.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The instance profile this account holds, and the one a create and an update name.
const HELD_PROFILE: &str = "EC2InstanceProfileForImageBuilder";

/// A second instance profile this account holds, so a replacing update can name another one.
const OTHER_PROFILE: &str = "EC2OtherInstanceProfileForImageBuilder";

/// A trust policy admitting Amazon EC2, so each role below is one an instance profile may
/// carry. Written with no `&`, `=`, `+` or `%`, so it travels in an `awsQuery` form body as
/// written.
const EC2_TRUST_POLICY: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
     \"Principal\":{\"Service\":\"ec2.amazonaws.com\"},\"Action\":\"sts:AssumeRole\"}]}";

/// The members the modelled `InfrastructureConfigurationSummary` names
/// (`smithy_query(query="shape",shape="InfrastructureConfigurationSummary")`), which are the
/// members a listing and a read must agree about.
const SUMMARY_MEMBERS: &[&str] = &[
    "arn",
    "name",
    "description",
    "dateCreated",
    "dateUpdated",
    "resourceTags",
    "tags",
    "instanceTypes",
    "instanceProfileName",
    "placement",
];

/// Members the modelled `InfrastructureConfiguration` names and the summary does NOT
/// (`smithy_query(query="shape",shape="InfrastructureConfiguration,InfrastructureConfigurationSummary")`),
/// so a listed entry carries none of them however the configuration was created.
const READ_ONLY_MEMBERS: &[&str] = &[
    "securityGroupIds",
    "subnetId",
    "logging",
    "keyPair",
    "snsTopicArn",
    "terminateInstanceOnFailure",
    "terminateInstanceOnCompletion",
    "instanceMetadataOptions",
    "parentInstance",
];

/// One emulator: imagebuilder and IAM on a single shared engine behind the production
/// router — what `build_router` assembles, narrowed to the two services under test.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Status and parsed body of one imagebuilder call.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("imagebuilder.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDIMAGEBUILDER/20260101/{REGION}/imagebuilder/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        );
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(serde_json::to_vec(value).expect("serialize the request body"))
        }
        None => bytes::Bytes::new(),
    };
    let request = builder.body(payload).expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    (status, parsed)
}

/// Drive one `awsQuery` IAM action: `POST /` with the parameters in the form body. Every
/// value here is free of `&`, `=`, `+` and `%`, so it needs no escaping.
async fn iam(router: &MultiServiceRouter, action: &str, parameters: &[(&str, &str)]) -> u16 {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (key, value) in parameters {
        body.push_str(&format!("&{key}={value}"));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://iam.{REGION}.amazonaws.com/"))
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iam/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed IAM request");
    router.dispatch(request).await.status().as_u16()
}

/// Create the instance profiles the configurations below name, each built the way AWS builds
/// one: a role, a profile, and the role added to it.
async fn stage_iam(router: &MultiServiceRouter) {
    for profile in [HELD_PROFILE, OTHER_PROFILE] {
        for (action, parameters) in [
            (
                "CreateRole",
                vec![
                    ("RoleName", profile),
                    ("AssumeRolePolicyDocument", EC2_TRUST_POLICY),
                ],
            ),
            (
                "CreateInstanceProfile",
                vec![("InstanceProfileName", profile), ("Path", "/")],
            ),
            (
                "AddRoleToInstanceProfile",
                vec![("InstanceProfileName", profile), ("RoleName", profile)],
            ),
        ] {
            let status = iam(router, action, &parameters).await;
            assert_eq!(
                status, 200,
                "setup: IAM {action} for {profile:?} must answer 200, got {status}"
            );
        }
    }
}

/// The ARN a configuration named `normalized` carries in [`REGION`].
fn arn_of(normalized: &str) -> String {
    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:infrastructure-configuration/{normalized}")
}

/// `ListInfrastructureConfigurations` with `body`.
async fn list(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(
        router,
        "POST",
        "/ListInfrastructureConfigurations",
        Some(body),
    )
    .await
}

/// The summary list one page carries.
fn summaries(case: &str, body: &Value) -> Vec<Value> {
    body.get("infrastructureConfigurationSummaryList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected a summary list, got {body}"))
        .clone()
}

/// The one summary the listing reports for the configuration named `name`, found through the
/// `name` filter the operation documents.
async fn summary_of(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, listed) = list(
        router,
        &json!({ "filters": [{ "name": "name", "values": [name] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the listing, got {status} with {listed}"
    );
    let found = summaries(case, &listed);
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one entry for {name:?}, got {found:?}"
    );
    found[0].clone()
}

/// The `InfrastructureConfiguration` a read reports for `arn`.
async fn read(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, body) = call(
        router,
        "GET",
        &format!("/GetInfrastructureConfiguration?infrastructureConfigurationArn={arn}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the read, got {status} with {body}"
    );
    body["infrastructureConfiguration"].clone()
}

/// One JSON member, with an absent member read as `null` so "both report nothing" compares
/// equal to itself.
fn member(value: &Value, name: &str) -> Value {
    value.get(name).cloned().unwrap_or(Value::Null)
}

/// Assert the listed entry and the read report the same stored record.
fn assert_agrees(case: &str, summary: &Value, view: &Value) {
    for name in SUMMARY_MEMBERS {
        let listed = member(summary, name);
        let reported = member(view, name);
        assert_eq!(
            listed, reported,
            "case {case:?}: the listing and the read must agree about {name}, got {listed} \
             listed and {reported} read"
        );
    }
    for name in READ_ONLY_MEMBERS {
        assert!(
            summary.get(name).is_none(),
            "case {case:?}: the summary shape does not name {name}, got {summary}"
        );
    }
}

/// One row: a configuration to create, named by the optional members it carries.
struct Case {
    case: &'static str,
    /// The configuration's name, which is also the normalized name its ARN carries.
    name: &'static str,
    /// The optional members the create adds to the two required ones.
    extra: Value,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "only the required members",
            name: "agree-minimal",
            extra: json!({}),
        },
        Case {
            case: "a description",
            name: "agree-described",
            extra: json!({ "description": "the configuration a read and a listing share" }),
        },
        Case {
            case: "three instance types, in the order the request carried them",
            name: "agree-instance-types",
            extra: json!({ "instanceTypes": ["t3.micro", "m5.large", "c6i.xlarge"] }),
        },
        Case {
            case: "resource tags and the configuration's own tags",
            name: "agree-tagged",
            extra: json!({
                "resourceTags": { "build": "nightly", "team": "imaging" },
                "tags": { "owner": "platform" },
            }),
        },
        Case {
            case: "placement settings",
            name: "agree-placed",
            extra: json!({
                "placement": { "availabilityZone": "us-west-2b", "tenancy": "dedicated" },
            }),
        },
        Case {
            case: "members only a read reports",
            name: "agree-read-only-members",
            extra: json!({
                "subnetId": "subnet-0123456789abcdef0",
                "securityGroupIds": ["sg-0123456789abcdef0"],
                "keyPair": "imagebuilder-keypair",
                "terminateInstanceOnFailure": false,
                "terminateInstanceOnCompletion": true,
                "snsTopicArn": format!("arn:aws:sns:{REGION}:{ACCOUNT}:imagebuilder-events"),
                "logging": { "s3Logs": { "s3BucketName": "imagebuilder-logs", "s3KeyPrefix": "builds" } },
                "instanceMetadataOptions": { "httpTokens": "required", "httpPutResponseHopLimit": 2 },
            }),
        },
        Case {
            case: "every optional member the summary names at once",
            name: "agree-everything",
            extra: json!({
                "description": "every summarised member at once",
                "instanceTypes": ["t3.small"],
                "resourceTags": { "purpose": "test" },
                "tags": { "suite": "listing" },
                "placement": { "availabilityZone": "us-west-2c", "tenancy": "default" },
            }),
        },
    ]
}

/// The create body one row submits.
fn create_body(case: &Case, token: &str) -> Value {
    let mut body = json!({
        "name": case.name,
        "instanceProfileName": HELD_PROFILE,
        "clientToken": token,
    });
    for (member, value) in case
        .extra
        .as_object()
        .expect("each row's extra members are an object")
    {
        body[member] = value.clone();
    }
    body
}

#[tokio::test]
async fn a_listed_entry_reports_the_same_record_a_read_reports() {
    let router = emulator();
    stage_iam(&router).await;

    let rows = cases();
    for (index, row) in rows.iter().enumerate() {
        let (status, created) = call(
            &router,
            "PUT",
            "/CreateInfrastructureConfiguration",
            Some(&create_body(row, &format!("agree-token-{index}"))),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: creating it must answer 200, got {status} with {created}",
            row.case
        );
        let arn = arn_of(row.name);
        assert_eq!(
            created["infrastructureConfigurationArn"],
            json!(arn.clone()),
            "case {:?}: expected the create to report {arn}, got {created}",
            row.case
        );
    }

    for row in &rows {
        let summary = summary_of(&router, row.case, row.name).await;
        let view = read(&router, row.case, &arn_of(row.name)).await;
        assert_agrees(row.case, &summary, &view);
        assert!(
            member(&summary, "dateCreated")
                .as_str()
                .is_some_and(|at| !at.is_empty()),
            "case {:?}: expected the entry to report when it was created, got {summary}",
            row.case
        );
        assert!(
            summary.get("dateUpdated").is_none(),
            "case {:?}: expected no dateUpdated before any update, got {summary}",
            row.case
        );
    }

    // Every one of them is in the unfiltered listing too, and the listing is a read: asking
    // again reports exactly what the first call reported.
    let (status, first) = list(&router, &json!({})).await;
    assert_eq!(
        status, 200,
        "case \"the whole listing\": expected HTTP 200, got {status} with {first}"
    );
    let listed = summaries("the whole listing", &first);
    let mut expected: Vec<String> = rows.iter().map(|row| arn_of(row.name)).collect();
    expected.sort();
    let actual: Vec<String> = listed
        .iter()
        .map(|summary| member(summary, "arn").as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        actual, expected,
        "case \"the whole listing\": expected {expected:?} in ARN order, got {actual:?}"
    );
    let (status, again) = list(&router, &json!({})).await;
    assert_eq!(
        status, 200,
        "case \"the listing read twice\": expected HTTP 200, got {status} with {again}"
    );
    let repeated = summaries("the listing read twice", &again);
    assert_eq!(
        repeated, listed,
        "case \"the listing read twice\": a read must leave the listing unchanged, got \
         {repeated:?} after {listed:?}"
    );
}

#[tokio::test]
async fn the_listing_follows_a_replacing_update_and_a_delete() {
    let router = emulator();
    stage_iam(&router).await;

    let replaced = "follows-replaced";
    let removed = "follows-removed";
    let kept = "follows-kept";
    for (index, name) in [replaced, removed, kept].iter().enumerate() {
        let body = json!({
            "name": name,
            "instanceProfileName": HELD_PROFILE,
            "clientToken": format!("follows-token-{index}"),
            "description": "as created",
            "instanceTypes": ["t3.micro"],
        });
        let (status, created) = call(
            &router,
            "PUT",
            "/CreateInfrastructureConfiguration",
            Some(&body),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: creating {name:?} must answer 200, got {status} with {created}"
        );
    }

    let created_at = member(
        &summary_of(&router, "before the update", replaced).await,
        "dateCreated",
    );

    // "The request replaces the configuration": the members it describes are written as
    // submitted and the optional ones it omits are cleared, so the listing reports the
    // replacement and not what it replaced.
    let (status, updated) = call(
        &router,
        "PUT",
        "/UpdateInfrastructureConfiguration",
        Some(&json!({
            "infrastructureConfigurationArn": arn_of(replaced),
            "instanceProfileName": OTHER_PROFILE,
            "instanceTypes": ["m5.large", "m5.xlarge"],
            "resourceTags": { "stage": "replaced" },
            "clientToken": "follows-update-token",
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a replacing update\": expected HTTP 200, got {status} with {updated}"
    );

    let summary = summary_of(&router, "a replacing update", replaced).await;
    let view = read(&router, "a replacing update", &arn_of(replaced)).await;
    assert_agrees("a replacing update", &summary, &view);
    for (name, expected) in [
        ("instanceProfileName", json!(OTHER_PROFILE)),
        ("instanceTypes", json!(["m5.large", "m5.xlarge"])),
        ("resourceTags", json!({ "stage": "replaced" })),
        ("dateCreated", created_at.clone()),
        // An omitted optional property is cleared, so the description the create stored is
        // gone from the listing too.
        ("description", Value::Null),
    ] {
        let actual = member(&summary, name);
        assert_eq!(
            actual, expected,
            "case \"a replacing update\": expected {name} {expected}, got {actual} in {summary}"
        );
    }
    assert!(
        member(&summary, "dateUpdated")
            .as_str()
            .is_some_and(|at| !at.is_empty()),
        "case \"a replacing update\": expected the entry to report when it was updated, got \
         {summary}"
    );

    // A configuration nothing updated still reports no `dateUpdated`, so the member tracks
    // the record and not the store.
    let untouched = summary_of(&router, "a configuration no update replaced", kept).await;
    assert!(
        untouched.get("dateUpdated").is_none(),
        "case \"a configuration no update replaced\": expected no dateUpdated, got {untouched}"
    );

    // A delete removes the record, so the listing stops reporting it and reports the rest
    // exactly as before.
    let before = summaries(
        "before the delete",
        &list(&router, &json!({})).await.1,
    );
    let (status, deleted) = call(
        &router,
        "DELETE",
        &format!(
            "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn={}",
            arn_of(removed)
        ),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a delete\": expected HTTP 200, got {status} with {deleted}"
    );
    let after = summaries("after the delete", &list(&router, &json!({})).await.1);
    let expected: Vec<Value> = before
        .into_iter()
        .filter(|summary| member(summary, "arn") != json!(arn_of(removed)))
        .collect();
    assert_eq!(
        after, expected,
        "case \"a delete\": expected {expected:?} to remain, got {after:?}"
    );
}
