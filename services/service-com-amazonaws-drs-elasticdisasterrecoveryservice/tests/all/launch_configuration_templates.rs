// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the DRS Launch Configuration Template lifecycle:
//! `CreateLaunchConfigurationTemplate`, `DescribeLaunchConfigurationTemplates` and
//! `DeleteLaunchConfigurationTemplate`.
//!
//! Every test drives the service the way the router does — one buffered
//! `http::Request` per call through [`MultiServiceRouter::dispatch`] — so what is
//! asserted is the wire behaviour (status, error code, JSON body), not a handler's Rust
//! return value. The harness (the router, `call_in`, `Answer`) is the one
//! `replication_configuration_template_lifecycle` builds.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one recorded
//! request:
//!
//! 1. A create persists every member the request supplied, mints the modelled id and the
//!    ARN that id addresses, and reports the STORED template — so a following Describe
//!    reads back the same values.
//! 2. A member whose value is outside the set its modelled enumeration lists is refused
//!    with HTTP 400, the error code `BadRequestException` and the message
//!    `Invalid request body` — the one answer this service gives a request body that does
//!    not satisfy its model — and nothing is created for it.
//! 3. The `@paginated` read walks each stored template exactly once, in a stable order,
//!    for any `maxResults`, selects by id, and changes nothing a later caller sees.
//! 4. A delete makes the template stop being reported; an id the store does not hold is
//!    the modelled `ResourceNotFoundException`.

use serde_json::{json, Map, Value};
use service_com_amazonaws_drs_elasticdisasterrecoveryservice::ops::types::{
    LAUNCH_DISPOSITION_VALUES, RECOVERY_MODE_VALUES,
    TARGET_INSTANCE_TYPE_RIGHT_SIZING_METHOD_VALUES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::replication_configuration_template_lifecycle::{
    call, call_in, drs, items, Answer, REGION,
};

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The error code this service answers a request body that does not satisfy its model
/// with, and the whole message that refusal carries — written out as the literals a
/// client reads rather than taken from the implementation's constants.
const BAD_REQUEST_CODE: &str = "BadRequestException";
const INVALID_REQUEST_BODY: &str = "Invalid request body";

/// The code the model declares for a template this service does not hold
/// (`smithy_query(query="operation",operation="DeleteLaunchConfigurationTemplate")`
/// lists `ResourceNotFoundException`, HTTP 404).
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// `^lct-[0-9a-zA-Z]{17}$`, the model's `LaunchConfigurationTemplateID` pattern
/// (length 21..21), checked without pulling in a regex engine.
fn is_template_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("lct-") else {
        return false;
    };
    suffix.len() == 17 && suffix.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The `launchConfigurationTemplate` member of a create's answer.
fn created(answer: &Answer) -> &Value {
    answer
        .member("launchConfigurationTemplate")
        .unwrap_or_else(|| panic!("expected a created template, got {answer:?}"))
}

/// A string member of a JSON object.
fn text(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Create a template from `body`, asserting the modelled 201, and answer the created
/// template.
async fn create_template(router: &MultiServiceRouter, body: &Value) -> Value {
    let answer = call(router, "CreateLaunchConfigurationTemplate", body).await;
    assert_eq!(
        answer.status, 201,
        "create {body}: expected 201, got {answer:?}"
    );
    created(&answer).clone()
}

/// `DescribeLaunchConfigurationTemplates` with `body`.
async fn describe(router: &MultiServiceRouter, body: &Value) -> Answer {
    let answer = call(router, "DescribeLaunchConfigurationTemplates", body).await;
    assert_eq!(
        answer.status, 200,
        "describe {body}: expected 200, got {answer:?}"
    );
    answer
}

/// The ids every stored template is reported under, walking every page.
async fn all_ids(router: &MultiServiceRouter) -> Vec<String> {
    let mut ids = Vec::new();
    let mut body = json!({});
    loop {
        let answer = describe(router, &body).await;
        for item in items(&answer) {
            ids.push(text(item, "launchConfigurationTemplateID"));
        }
        match answer.member("nextToken").and_then(Value::as_str) {
            Some(token) => body = json!({"nextToken": token}),
            None => return ids,
        }
    }
}

/// A one-entry tag map built from values a test holds, which `json!` cannot take as a
/// key.
fn one_tag(key: &str, value: &str) -> Value {
    let mut map = Map::new();
    map.insert(key.to_string(), Value::String(value.to_string()));
    Value::Object(map)
}

/// The create bodies rule 1 spans: the rows differ along which members they supply and
/// which value each carries. Every modelled enumeration is covered at EVERY one of its
/// values, from the value sets the model generated
/// (`crate::…::ops::types::{LAUNCH_DISPOSITION_VALUES,
/// TARGET_INSTANCE_TYPE_RIGHT_SIZING_METHOD_VALUES, RECOVERY_MODE_VALUES}`), so no row
/// is the one the issue cited and none is hard-coded here.
fn create_rows() -> Vec<(String, Value)> {
    let mut rows: Vec<(String, Value)> = vec![
        ("no member at all".to_string(), json!({})),
        ("the empty tag set".to_string(), json!({"tags": {}})),
        (
            "several tags".to_string(),
            json!({"tags": {"env": "prod", "team": "dr"}}),
        ),
        (
            "a key and a value at the greatest modelled length".to_string(),
            json!({"tags": one_tag(&"k".repeat(256), &"v".repeat(256))}),
        ),
        (
            "every boolean set".to_string(),
            json!({
                "copyPrivateIp": true,
                "copyTags": true,
                "postLaunchEnabled": true,
                "launchIntoSourceInstance": true,
            }),
        ),
        (
            "every boolean cleared".to_string(),
            json!({
                "copyPrivateIp": false,
                "copyTags": false,
                "postLaunchEnabled": false,
                "launchIntoSourceInstance": false,
            }),
        ),
        (
            "licensing and an export bucket".to_string(),
            json!({
                "licensing": {"osByol": true},
                "exportBucketArn": format!("arn:aws:s3:::drs-exports-{ACCOUNT}"),
            }),
        ),
    ];
    for value in LAUNCH_DISPOSITION_VALUES {
        rows.push((
            format!("launchDisposition {value}"),
            json!({"launchDisposition": value}),
        ));
    }
    for value in TARGET_INSTANCE_TYPE_RIGHT_SIZING_METHOD_VALUES {
        rows.push((
            format!("targetInstanceTypeRightSizingMethod {value}"),
            json!({"targetInstanceTypeRightSizingMethod": value}),
        ));
    }
    for value in RECOVERY_MODE_VALUES {
        rows.push((format!("recoveryMode {value}"), json!({"recoveryMode": value})));
    }
    rows.push((
        "every member at once".to_string(),
        json!({
            "tags": {"env": "prod"},
            "launchDisposition": LAUNCH_DISPOSITION_VALUES[1],
            "targetInstanceTypeRightSizingMethod": TARGET_INSTANCE_TYPE_RIGHT_SIZING_METHOD_VALUES[2],
            "recoveryMode": RECOVERY_MODE_VALUES[0],
            "copyPrivateIp": true,
            "copyTags": false,
            "postLaunchEnabled": true,
            "launchIntoSourceInstance": false,
            "licensing": {"osByol": false},
            "exportBucketArn": format!("arn:aws:s3:::drs-exports-{ACCOUNT}"),
        }),
    ));
    rows
}

// ---------------------------------------------------------------------------
// Rule 1 — a create persists what it was given, and a read returns it
// ---------------------------------------------------------------------------

/// Every member a create supplies is stored and reported, the id and ARN are the ones
/// the model describes, and a Describe of that id reads the same template back.
#[tokio::test]
async fn a_create_persists_every_supplied_member_and_a_read_returns_it() {
    let router = drs();
    for (case, body) in create_rows() {
        let template = create_template(&router, &body).await;

        let id = text(&template, "launchConfigurationTemplateID");
        assert!(
            is_template_id(&id),
            "case {case:?}: expected an lct- id of the modelled form, got {id:?}"
        );
        assert_eq!(
            text(&template, "arn"),
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:launch-configuration-template/{id}"),
            "case {case:?}: wrong ARN in {template}"
        );

        // Every member the request named is reported with the value it named. A create
        // that named no tag set still reports one, because a template always carries a
        // tag set.
        for (member, value) in body.as_object().expect("a JSON object body") {
            assert_eq!(
                template.get(member),
                Some(value),
                "case {case:?}: expected {member} to be {value}, got {template}"
            );
        }
        if body.get("tags").is_none() {
            assert_eq!(
                template.get("tags"),
                Some(&json!({})),
                "case {case:?}: a template created with no tags carries the empty set, \
                 got {template}"
            );
        }

        // The stored template, read back through the operation that reports it.
        let answer = describe(&router, &json!({"launchConfigurationTemplateIDs": [id]})).await;
        assert_eq!(
            items(&answer),
            &vec![template.clone()],
            "case {case:?}: the read must report the created template, got {answer:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — a value outside a modelled enumeration
// ---------------------------------------------------------------------------

/// A member carrying a value its modelled enumeration does not list is refused as a
/// request body this service cannot read: HTTP 400, error code `BadRequestException`,
/// message `Invalid request body` — never a per-constraint report — and nothing is
/// created. The rows span every enum member of the operation and the ways a value can
/// fall outside its set: an unknown token, the wrong case of a listed value, a value
/// listed by ANOTHER of the operation's enumerations, and the empty string.
#[tokio::test]
async fn a_value_outside_a_modelled_enumeration_is_an_invalid_request_body() {
    let router = drs();
    // One template that IS created, so the refusals below can be shown to create
    // nothing beside it.
    let present = create_template(&router, &json!({"tags": {"keep": "me"}})).await;
    let present_id = text(&present, "launchConfigurationTemplateID");

    // (member, the values its enumeration lists, a value another member's does)
    let members: [(&str, &[&str], &str); 3] = [
        (
            "launchDisposition",
            LAUNCH_DISPOSITION_VALUES,
            RECOVERY_MODE_VALUES[0],
        ),
        (
            "targetInstanceTypeRightSizingMethod",
            TARGET_INSTANCE_TYPE_RIGHT_SIZING_METHOD_VALUES,
            LAUNCH_DISPOSITION_VALUES[0],
        ),
        (
            "recoveryMode",
            RECOVERY_MODE_VALUES,
            TARGET_INSTANCE_TYPE_RIGHT_SIZING_METHOD_VALUES[0],
        ),
    ];

    for (member, listed, foreign) in members {
        let lowercased = listed[0].to_ascii_lowercase();
        let outside = [
            ("an unknown token", "BOGUS_VALUE"),
            ("the wrong case of a listed value", lowercased.as_str()),
            ("a value of another enumeration", foreign),
            ("the empty string", ""),
            ("a listed value with trailing text", "NONE_OF_THESE"),
        ];
        for (why, value) in outside {
            // Only a value this enumeration does not list can be refused; a foreign
            // value that happens to be listed here is not such a row.
            if listed.contains(&value) {
                continue;
            }
            let case = format!("{member} = {value:?} ({why})");
            let mut body = Map::new();
            body.insert(member.to_string(), Value::String(value.to_string()));
            let answer = call(&router, "CreateLaunchConfigurationTemplate", &Value::Object(body))
                .await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected 400, got {answer:?}"
            );
            assert_eq!(
                answer.error_code.as_deref(),
                Some(BAD_REQUEST_CODE),
                "case {case:?}: expected {BAD_REQUEST_CODE}, got {answer:?}"
            );
            assert_eq!(
                text(&answer.body, "message"),
                INVALID_REQUEST_BODY,
                "case {case:?}: wrong message in {answer:?}"
            );
            assert!(
                answer.member("launchConfigurationTemplate").is_none(),
                "case {case:?}: a refusal must report no template, got {answer:?}"
            );
        }
    }

    // Nothing was created for any refused row: the one template created above is still
    // the only one the store holds.
    assert_eq!(
        all_ids(&router).await,
        vec![present_id],
        "a refused create must leave the store exactly as it was"
    );
}

// ---------------------------------------------------------------------------
// Rule 3 — the paginated read
// ---------------------------------------------------------------------------

/// The read walks every stored template exactly once, in creation order, whatever
/// `maxResults` asks for, and changes nothing: the same walk answers the same way again.
#[tokio::test]
async fn the_read_walks_every_template_once_for_any_page_size() {
    let router = drs();
    let mut expected = Vec::new();
    for index in 0..5 {
        let template = create_template(&router, &json!({"tags": {"i": index.to_string()}})).await;
        expected.push(text(&template, "launchConfigurationTemplateID"));
    }

    for max_results in [1, 2, 3, 5, 1000] {
        let mut seen = Vec::new();
        let mut body = json!({"maxResults": max_results});
        loop {
            let answer = describe(&router, &body).await;
            let page = items(&answer);
            assert!(
                page.len() <= usize::try_from(max_results).expect("a positive page size"),
                "case maxResults={max_results}: a page carried {} items, got {answer:?}",
                page.len()
            );
            for item in page {
                seen.push(text(item, "launchConfigurationTemplateID"));
            }
            match answer.member("nextToken").and_then(Value::as_str) {
                Some(token) => body = json!({"maxResults": max_results, "nextToken": token}),
                None => break,
            }
        }
        assert_eq!(
            seen, expected,
            "case maxResults={max_results}: expected every template once in creation order"
        );
    }

    // Reading selects: an id the store does not hold selects nothing rather than
    // refusing the read.
    let answer = describe(
        &router,
        &json!({"launchConfigurationTemplateIDs": ["lct-00000000000000000"]}),
    )
    .await;
    assert!(
        items(&answer).is_empty(),
        "an id nothing minted must select no template, got {answer:?}"
    );

    // Every read above left the store as it was.
    assert_eq!(
        all_ids(&router).await,
        expected,
        "the reads must have changed nothing a later caller sees"
    );
}

// ---------------------------------------------------------------------------
// Rule 4 — a delete, and an id the store does not hold
// ---------------------------------------------------------------------------

/// A delete answers the modelled empty 204 and the template stops being reported; the
/// same id, and any id nothing minted, is then the modelled `ResourceNotFoundException`
/// naming the id the request asked for. The rows span the ids a delete can name: one the
/// store held and has given up, one nothing ever minted, and one of a template under
/// another Region (the store is keyed by id, so that one IS held — a row that pins which
/// of the two readings the delete performs is the Region-scoped ARN of the tagging
/// route, not this id).
#[tokio::test]
async fn a_delete_makes_the_template_stop_being_reported() {
    let router = drs();
    let keep = create_template(&router, &json!({"tags": {"keep": "me"}})).await;
    let keep_id = text(&keep, "launchConfigurationTemplateID");
    let gone = create_template(&router, &json!({})).await;
    let gone_id = text(&gone, "launchConfigurationTemplateID");

    let answer = call(
        &router,
        "DeleteLaunchConfigurationTemplate",
        &json!({"launchConfigurationTemplateID": gone_id}),
    )
    .await;
    assert_eq!(
        answer.status, 204,
        "deleting {gone_id}: expected 204, got {answer:?}"
    );

    assert_eq!(
        all_ids(&router).await,
        vec![keep_id],
        "the deleted template must stop being reported"
    );

    for (case, id) in [
        ("an id the store has given up", gone_id.as_str()),
        ("an id nothing minted", "lct-00000000000000000"),
    ] {
        let answer = call(
            &router,
            "DeleteLaunchConfigurationTemplate",
            &json!({"launchConfigurationTemplateID": id}),
        )
        .await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected 404 for {id}, got {answer:?}"
        );
        assert_eq!(
            answer.error_code.as_deref(),
            Some(NOT_FOUND_CODE),
            "case {case:?}: expected {NOT_FOUND_CODE}, got {answer:?}"
        );
        assert_eq!(
            text(&answer.body, "resourceId"),
            id,
            "case {case:?}: resourceId must be the id the request named, got {answer:?}"
        );
        assert_eq!(
            text(&answer.body, "message"),
            format!("Launch Configuration Template with Id: '{id}' was not found"),
            "case {case:?}: wrong message in {answer:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The ARN a template is reported under carries the Region the caller signed for
// ---------------------------------------------------------------------------

/// A template's ARN is built from the Region the request was signed for, not from the
/// emulator's default, so two creates signed for different Regions report ARNs in those
/// Regions.
#[tokio::test]
async fn a_template_arn_carries_the_region_the_caller_signed_for() {
    let router = drs();
    for region in ["eu-west-1", "us-east-1", "ap-southeast-2"] {
        let answer = call_in(
            &router,
            region,
            "CreateLaunchConfigurationTemplate",
            &json!({}),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case {region:?}: expected 201, got {answer:?}"
        );
        let template = created(&answer);
        let id = text(template, "launchConfigurationTemplateID");
        assert_eq!(
            text(template, "arn"),
            format!("arn:aws:drs:{region}:{ACCOUNT}:launch-configuration-template/{id}"),
            "case {region:?}: wrong ARN in {template}"
        );
    }
}
