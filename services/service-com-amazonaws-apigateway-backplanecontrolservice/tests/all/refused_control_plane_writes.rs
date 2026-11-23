// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a control-plane request this service REFUSES leaves behind.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi` / `CreateDeployment` / `CreateDomainName` / `CreateBasePathMapping` /
//! `ImportRestApi` helpers are this crate's existing harness.
//!
//! The RULE under test: a control-plane request carrying a value this service does not
//! admit, or addressing a record another record still depends on, is answered with the
//! modelled `BadRequestException` (HTTP 400) carrying the sentence AWS states for that
//! condition — and it changes NOTHING a later caller can read: not the record it
//! addressed, and not ANY collection of the account, whichever collection the record it
//! would have written belongs to.
//!
//! The sibling suites assert each refusal against the reader of its own resource
//! (`control_plane_input_admission`, `base_path_mapping_references`,
//! `rest_api_import_model_reference_warnings`). This one asserts the ACCOUNT-WIDE half of
//! the same rule: one table spanning every refusal family of this control plane, each row
//! checked against a snapshot of every collection the estate's records are reported in.
//!
//! The rows differ along what the rule generalises over: which member carries the refused
//! value (a certificate reference, a base path, a quota offset), whether it arrives on a
//! create or on a patch, which resource the refused write would have belonged to, and the
//! two shapes of refusal — a value outside what the member admits, and a delete of a
//! record a `BasePathMapping` still names.
//!
//! The second test is the control that keeps the first from passing vacuously: the
//! ADMITTED counterpart of each family changes exactly the collection it belongs to.

use serde_json::{json, Value};

use crate::base_path_mapping::{assert_refusal, create_mapping, deployed_api};
use crate::base_path_mapping_references::stage_refusal;
use crate::domain_name_collection::created_domain;
use crate::rest_api_create_from_import::import;
use crate::rest_api_import_model_reference_warnings::{refusal, unreadable, unresolved};
use crate::rest_api_lifecycle::{page_items, string_member, Gateway};
use crate::usage_plan_lifecycle::create_usage_plan;

/// The modelled refusal of a request this control plane does not admit: `BadRequestException`
/// at HTTP 400, as `CreateDomainName`, `CreateBasePathMapping`, `CreateUsagePlan`,
/// `DeleteStage`, `DeleteRestApi` and `ImportRestApi` all declare it.
const BAD_REQUEST: (u16, &str) = (400, "BadRequestException");

/// The sentence this service states for a value an ARN-valued member carries that it could
/// not read as the ARN it names — written out here rather than taken from the handler's own
/// constant, because the FIXED text is what this test pins.
const INVALID_ARN: &str = "Invalid ARN specified in the request";

/// The sentence API Gateway states for a `basePath` outside the character class it admits:
/// fixed text that states the class and quotes back neither the value nor the member.
const INVALID_BASE_PATH: &str = "A base path may contain only letters, numbers and one of $-_.+!*'(),";

/// A certificate ARN as Certificate Manager writes one, for the records these cases are
/// refused against: `arn:aws:acm:<region>:<account>:certificate/<id>`.
const CERTIFICATE_ARN: &str = "arn:aws:acm:us-east-1:000000000000:certificate/refused-writes";

/// An OpenAPI 3 definition whose response schema points at a model the document does not
/// define, which is the warning `failOnWarnings` refuses to tolerate.
const UNRESOLVED_REFERENCE_DOCUMENT: &str = r##"{"openapi": "3.0.1",
    "info": {"title": "refused-import", "version": "1.0"},
    "paths": {"/imported": {"post": {"responses": {"200": {"description": "ok",
        "content": {"application/json": {"schema":
            {"$ref": "#/components/schemas/Item"}}}}}}}},
    "components": {"schemas": {}}}"##;

/// The same definition with nothing left unresolved, which an import reports no warning
/// for: the admitted counterpart of the document above.
const RESOLVED_DOCUMENT: &str = r##"{"openapi": "3.0.1",
    "info": {"title": "admitted-import", "version": "1.0"},
    "paths": {"/imported": {"post": {"responses": {"200": {"description": "ok"}}}}}}"##;

/// The records every case is refused against: one REST API with a deployed stage, one
/// custom domain name, the base path mapping that routes the domain name's base path to
/// that stage, and one usage plan whose quota meters a `MONTH` window.
struct Estate {
    api: String,
    stage: String,
    domain_name: String,
    base_path: String,
    usage_plan: String,
}

/// The `CreateDomainName` body these cases use: a REGIONAL record presenting a certificate
/// ARN, so the record is AVAILABLE as soon as the create answers and nothing in the
/// collections a snapshot reads moves on its own.
fn regional_request() -> Value {
    json!({
        "endpointConfiguration": { "types": ["REGIONAL"] },
        "regionalCertificateArn": CERTIFICATE_ARN,
    })
}

/// `CreateUsagePlan` with `quota`, answering the plan's id. The create itself is the
/// `usage_plan_lifecycle` suite's one spelling of that call, reused rather than copied.
async fn created_usage_plan(gateway: &Gateway, name: &str, quota: Value) -> String {
    let body = create_usage_plan(gateway, name, json!({ "name": name, "quota": quota })).await;
    string_member(&body, "id", name)
}

/// Stand the estate up.
async fn estate(gateway: &Gateway) -> Estate {
    let (api, stage) = deployed_api(gateway, "refused-writes-api", "prod").await;
    let domain_name = "refused-writes.example.com".to_string();
    created_domain(gateway, &domain_name, regional_request()).await;
    let base_path = "v1".to_string();
    let created = create_mapping(
        gateway,
        &domain_name,
        json!({ "basePath": base_path, "restApiId": api, "stage": stage }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case the estate: expected 201 from CreateBasePathMapping, got {} with {:?}",
        created.status, created.body
    );
    let usage_plan = created_usage_plan(
        gateway,
        "refused-writes-plan",
        json!({ "limit": 500, "offset": 0, "period": "MONTH" }),
    )
    .await;
    Estate {
        api,
        stage,
        domain_name,
        base_path,
        usage_plan,
    }
}

/// The key each collection is held under in a snapshot, and the collection's own route.
fn collection_routes(estate: &Estate) -> Vec<(String, String)> {
    vec![
        ("restApis".to_string(), "/restapis".to_string()),
        (
            "stages".to_string(),
            format!("/restapis/{}/stages", estate.api),
        ),
        ("domainNames".to_string(), "/domainnames".to_string()),
        (
            "basePathMappings".to_string(),
            format!("/domainnames/{}/basepathmappings", estate.domain_name),
        ),
        ("usagePlans".to_string(), "/usageplans".to_string()),
    ]
}

/// Every collection the estate's records are reported in, read through this service's own
/// collection operations, as ONE comparable value.
async fn collections(gateway: &Gateway, estate: &Estate, case: &str) -> Value {
    let mut snapshot = serde_json::Map::new();
    for (key, uri) in collection_routes(estate) {
        let answer = gateway.call("GET", &uri, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200 from GET {uri}, got {} with {:?}",
            answer.status, answer.body
        );
        snapshot.insert(key, Value::Array(page_items(&answer.body, case)));
    }
    Value::Object(snapshot)
}

/// How many elements the collection `key` of `snapshot` holds.
fn held(snapshot: &Value, key: &str) -> usize {
    snapshot[key].as_array().map_or(0, Vec::len)
}

/// One refused request: the case, the call to make, and the sentence it must be refused
/// with.
struct Refusal {
    case: String,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    message: String,
}

/// One `replace` patch of `path` to `value`, the form every patch row here carries.
fn patch(path: &str, value: &str) -> Value {
    json!({ "patchOperations": [{ "op": "replace", "path": path, "value": value }] })
}

/// The table: one row per refusal family of this control plane, each against the estate.
fn refusals(estate: &Estate) -> Vec<Refusal> {
    let domain = estate.domain_name.as_str();
    vec![
        Refusal {
            case: "a certificate member set to a value that is no ARN at all".to_string(),
            method: "POST",
            uri: "/domainnames".to_string(),
            body: Some(json!({
                "domainName": "refused-no-arn.example.com",
                "certificateArn": "not-a-well-formed-acm-arn",
            })),
            message: INVALID_ARN.to_string(),
        },
        Refusal {
            case: "a certificate member set to another vendor's ARN".to_string(),
            method: "POST",
            uri: "/domainnames".to_string(),
            body: Some(json!({
                "domainName": "refused-other-vendor.example.com",
                "endpointConfiguration": { "types": ["REGIONAL"] },
                "regionalCertificateArn": "arn:aws:iam::000000000000:role/certificates",
            })),
            message: INVALID_ARN.to_string(),
        },
        Refusal {
            case: "a patch moving a certificate member onto another service's ARN".to_string(),
            method: "PATCH",
            uri: format!("/domainnames/{domain}"),
            body: Some(patch(
                "/regionalCertificateArn",
                "arn:aws:acm-pca:us-east-1:000000000000:certificate-authority/ca-1",
            )),
            message: INVALID_ARN.to_string(),
        },
        Refusal {
            case: "a base path carrying non-ASCII characters".to_string(),
            method: "POST",
            uri: format!("/domainnames/{domain}/basepathmappings"),
            body: Some(json!({
                "basePath": "café-日本語",
                "restApiId": estate.api,
                "stage": estate.stage,
            })),
            message: INVALID_BASE_PATH.to_string(),
        },
        Refusal {
            case: "a base path carrying a space".to_string(),
            method: "POST",
            uri: format!("/domainnames/{domain}/basepathmappings"),
            body: Some(json!({
                "basePath": "orders v2",
                "restApiId": estate.api,
                "stage": estate.stage,
            })),
            message: INVALID_BASE_PATH.to_string(),
        },
        Refusal {
            case: "a patch moving a mapping onto a multi-level base path".to_string(),
            method: "PATCH",
            uri: format!(
                "/domainnames/{domain}/basepathmappings/{}",
                estate.base_path
            ),
            body: Some(patch("/basePath", "v1/orders")),
            message: INVALID_BASE_PATH.to_string(),
        },
        Refusal {
            case: "a quota offset outside the window of the DAY period".to_string(),
            method: "POST",
            uri: "/usageplans".to_string(),
            body: Some(json!({
                "name": "refused-day-offset",
                "quota": { "limit": 50, "offset": 50, "period": "DAY" },
            })),
            message: "Usage Plan quota offset must be zero in the DAY period".to_string(),
        },
        Refusal {
            case: "a quota offset one past the window of the WEEK period".to_string(),
            method: "POST",
            uri: "/usageplans".to_string(),
            body: Some(json!({
                "name": "refused-week-offset",
                "quota": { "limit": 50, "offset": 7, "period": "WEEK" },
            })),
            message: "Usage Plan quota offset must be between 0 and 6 in the WEEK period"
                .to_string(),
        },
        Refusal {
            case: "a patch moving a quota offset past the window of its period".to_string(),
            method: "PATCH",
            uri: format!("/usageplans/{}", estate.usage_plan),
            body: Some(patch("/quota/offset", "28")),
            message: "Usage Plan quota offset must be between 0 and 27 in the MONTH period"
                .to_string(),
        },
        Refusal {
            case: "the delete of a stage a base path mapping still names".to_string(),
            method: "DELETE",
            uri: format!("/restapis/{}/stages/{}", estate.api, estate.stage),
            body: None,
            message: stage_refusal(&estate.stage, &[domain]),
        },
    ]
}

#[tokio::test]
async fn a_refused_control_plane_write_is_visible_in_no_collection_of_the_account() {
    let gateway = Gateway::new();
    let estate = estate(&gateway).await;
    let before = collections(&gateway, &estate, "the estate every row is refused against").await;

    for row in refusals(&estate) {
        let answer = gateway.call(row.method, &row.uri, row.body.clone()).await;
        // The crate's one modelled-refusal assertion: the status, the `x-amzn-errortype`
        // code and the exact sentence, asserted the way every sibling suite asserts one.
        assert_refusal(&answer, BAD_REQUEST, &row.message, &row.case);

        let after = collections(&gateway, &estate, &row.case).await;
        assert_eq!(
            after, before,
            "case {}: a refused request must leave every collection as it was, got {after:?}",
            row.case
        );
    }

    // The import family: `failOnWarnings` refuses over the warnings it would not tolerate,
    // naming each of them under API Gateway's fixed lead — the model the document's own
    // reference pointed at is the variable part — and creates no API, so the account's
    // collections are the ones it had.
    let case = "an import refused over the warnings it would not tolerate";
    // The WHOLE sentence, rendered by the sibling `rest_api_import_model_reference_warnings`
    // suite's own builders: API Gateway's fixed lead and one tab-indented line per warning
    // the served import reports, in that order. Every variable part comes from the posted
    // document — the model its reference names (resolved once for the model and once for the
    // binding of that model to the media type), then the media type and the verb the method
    // is left without a format for.
    let expected = refusal(&[
        unresolved("Item"),
        unresolved("Item"),
        unreadable("application/json", "POST"),
    ]);
    let refused = import(&gateway, UNRESOLVED_REFERENCE_DOCUMENT, true, &[]).await;
    assert_refusal(&refused, BAD_REQUEST, &expected, case);
    let after = collections(&gateway, &estate, case).await;
    assert_eq!(
        after, before,
        "case {case}: a refused import must leave every collection as it was, got {after:?}"
    );
}

#[tokio::test]
async fn an_admitted_control_plane_write_changes_the_collection_it_belongs_to() {
    let gateway = Gateway::new();
    let estate = estate(&gateway).await;

    // The admitted counterpart of every refused family above, each asserted against the
    // collection its record belongs to: this is what makes the snapshot above an assertion
    // about the REFUSAL rather than about a reader that reports nothing either way.
    let case = "a domain name whose certificate member carries a certificate ARN";
    let before = collections(&gateway, &estate, case).await;
    created_domain(&gateway, "admitted.example.com", regional_request()).await;
    let after = collections(&gateway, &estate, case).await;
    assert_eq!(
        held(&after, "domainNames"),
        held(&before, "domainNames") + 1,
        "case {case}: expected one more domain name than the {} before it, got {}",
        held(&before, "domainNames"),
        held(&after, "domainNames")
    );

    let case = "a base path inside the character class";
    let before = after;
    let created = create_mapping(
        &gateway,
        &estate.domain_name,
        json!({
            "basePath": "orders-v2",
            "restApiId": estate.api,
            "stage": estate.stage,
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected 201 from CreateBasePathMapping, got {} with {:?}",
        created.status, created.body
    );
    let after = collections(&gateway, &estate, case).await;
    assert_eq!(
        held(&after, "basePathMappings"),
        held(&before, "basePathMappings") + 1,
        "case {case}: expected one more base path mapping than the {} before it, got {}",
        held(&before, "basePathMappings"),
        held(&after, "basePathMappings")
    );

    let case = "a quota offset inside the window of its period";
    let before = after;
    created_usage_plan(
        &gateway,
        "admitted-plan",
        json!({ "limit": 50, "offset": 0, "period": "DAY" }),
    )
    .await;
    let after = collections(&gateway, &estate, case).await;
    assert_eq!(
        held(&after, "usagePlans"),
        held(&before, "usagePlans") + 1,
        "case {case}: expected one more usage plan than the {} before it, got {}",
        held(&before, "usagePlans"),
        held(&after, "usagePlans")
    );

    let case = "an import with nothing left unresolved";
    let before = after;
    let imported = import(&gateway, RESOLVED_DOCUMENT, false, &[]).await;
    assert_eq!(
        imported.status, 201,
        "case {case}: expected 201 from ImportRestApi, got {} with {:?}",
        imported.status, imported.body
    );
    let after = collections(&gateway, &estate, case).await;
    assert_eq!(
        held(&after, "restApis"),
        held(&before, "restApis") + 1,
        "case {case}: expected one more REST API than the {} before it, got {}",
        held(&before, "restApis"),
        held(&after, "restApis")
    );

    // And the delete family: once every mapping that names the stage is gone, the delete
    // the refusal above withheld goes through and the stage leaves its collection.
    let case = "the delete of a stage no base path mapping names any more";
    for base_path in [estate.base_path.as_str(), "orders-v2"] {
        let deleted = gateway
            .call(
                "DELETE",
                &format!(
                    "/domainnames/{}/basepathmappings/{base_path}",
                    estate.domain_name
                ),
                None,
            )
            .await;
        assert_eq!(
            deleted.status, 202,
            "case {case}: expected 202 from DeleteBasePathMapping({base_path:?}), got {} with \
             {:?}",
            deleted.status, deleted.body
        );
    }
    let before = collections(&gateway, &estate, case).await;
    let deleted = gateway
        .call(
            "DELETE",
            &format!("/restapis/{}/stages/{}", estate.api, estate.stage),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 202,
        "case {case}: expected 202 from DeleteStage, got {} with {:?}",
        deleted.status, deleted.body
    );
    let after = collections(&gateway, &estate, case).await;
    assert_eq!(
        held(&after, "stages") + 1,
        held(&before, "stages"),
        "case {case}: expected one fewer stage than the {} before it, got {}",
        held(&before, "stages"),
        held(&after, "stages")
    );
}
