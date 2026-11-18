// SPDX-License-Identifier: CC0-1.0

//! Which `InputTransformer` templates `PutTargets` ACCEPTS, end to end over the
//! wire through the real `MultiServiceRouter` with Amazon SQS mounted beside
//! EventBridge — so an accepted target is read back through `ListTargetsByRule`
//! and shown to deliver, and a refused one is shown to have changed nothing.
//!
//! The RULE asserted here: an `InputTemplate` is "the input template where you
//! specify placeholders that will be filled with the values of the keys from
//! `InputPathsMap`", each placeholder written `<key>`
//! (`InputTransformer$InputTemplate`) — so every placeholder a template carries
//! must name something that fills it: a key the target's own `InputPathsMap`
//! declares, or one of the variables EventBridge fills from the delivery itself.
//! A template naming anything else stands for a value nothing can supply, and the
//! target is refused when it is CONFIGURED — HTTP 400, the service-common
//! `ValidationException`, the refusal naming the member, the target by its `Id`
//! and the invalid placeholder — leaving the rule's targets exactly as
//! they were. A template whose every placeholder is
//! filled is accepted, whether the map declares the key or EventBridge does, and
//! whether or not the map declares keys the template never uses.
//!
//! The table spans the class rather than the one recorded case: the rows differ
//! along WHAT the placeholder names (a declared key, a key no map declares, a
//! variable EventBridge fills, a name in that reserved namespace which is not one
//! of them), along WHERE it stands (as a JSON value, inside the template's own
//! string, in a sentence), along whether a map is supplied at all, and along how
//! many placeholders the template carries.
//!
//! The harness (the router with EventBridge and Amazon SQS on one engine) is
//! `tests/all/target_dispatch.rs`'s, and so is the grant a target queue needs.

use serde_json::{json, Value};

use crate::target_dispatch::{
    drain, emulator, events, given_queue, given_rule, publish_details, DETAIL_TYPE, SOURCE,
};

/// The rule every row writes its target on.
const RULE: &str = "transform";

/// The id of the target each row configures, and of the one the rule already
/// carries — which a refused request must leave alone.
const TARGET_ID: &str = "t1";
const KEPT_TARGET_ID: &str = "kept";

/// The event every accepted row is delivered, carrying a place for each path the
/// table's map declares.
fn detail() -> Value {
    json!({ "who": "alice", "user": { "name": "bob" } })
}

/// The map the rows that declare one carry: one path per key a template below
/// fills from the event.
fn paths() -> Value {
    json!({ "who": "$.detail.who", "deep": "$.detail.user.name" })
}

/// Every variable EventBridge fills itself, from the predefined variables its
/// target-input transformation documents: the matched rule's ARN and name, the
/// event's ingestion time, and the event itself in its two spellings. Each is
/// tested, not only the one the report cited.
const RESERVED_VARIABLES: [&str; 5] = [
    "aws.events.event",
    "aws.events.event.json",
    "aws.events.rule-arn",
    "aws.events.rule-name",
    "aws.events.event.ingestion-time",
];

/// One row: the template a target carries, the map it declares (`None` for a
/// target that declares none), whether `PutTargets` accepts it, and — for a row
/// it refuses — the placeholder the refusal must name.
struct Row {
    case: String,
    paths: Option<Value>,
    template: String,
    accepted: bool,
    /// The key of the placeholder nothing fills, for a row that is refused.
    unfilled: &'static str,
}

fn rows() -> Vec<Row> {
    let mut rows = vec![
        Row {
            case: "a placeholder the map declares, standing as a JSON value".to_string(),
            paths: Some(paths()),
            template: "{\"who\": <who>}".to_string(),
            accepted: true,
            unfilled: "",
        },
        Row {
            case: "every placeholder the map declares, as values and in a string".to_string(),
            paths: Some(paths()),
            template: "{\"who\": <who>, \"deep\": \"<deep>\", \"seen\": \"transformed\"}"
                .to_string(),
            accepted: true,
            unfilled: "",
        },
        Row {
            case: "a map declaring more keys than the template fills".to_string(),
            paths: Some(paths()),
            template: "{\"who\": <who>}".to_string(),
            accepted: true,
            unfilled: "",
        },
        Row {
            case: "a template carrying no placeholder at all".to_string(),
            paths: Some(paths()),
            template: "{\"seen\": \"transformed\"}".to_string(),
            accepted: true,
            unfilled: "",
        },
        Row {
            case: "a template carrying no placeholder and declaring no map".to_string(),
            paths: None,
            template: "{\"seen\": \"transformed\"}".to_string(),
            accepted: true,
            unfilled: "",
        },
        Row {
            case: "a placeholder no map key names, standing as a JSON value".to_string(),
            paths: Some(paths()),
            template: "{\"who\": <who>, \"nope\": <nope>}".to_string(),
            accepted: false,
            unfilled: "nope",
        },
        Row {
            case: "a placeholder no map key names, standing inside a string".to_string(),
            paths: Some(paths()),
            template: "{\"nope\": \"<nope>\"}".to_string(),
            accepted: false,
            unfilled: "nope",
        },
        Row {
            case: "a placeholder no map key names, in a sentence template".to_string(),
            paths: Some(paths()),
            template: "\"<who> told <nope>\"".to_string(),
            accepted: false,
            unfilled: "nope",
        },
        Row {
            case: "a placeholder with no map declared at all".to_string(),
            paths: None,
            template: "{\"who\": <who>}".to_string(),
            accepted: false,
            unfilled: "who",
        },
        Row {
            case: "a placeholder the map declares under another key".to_string(),
            paths: Some(json!({ "who": "$.detail.who" })),
            template: "{\"deep\": <deep>}".to_string(),
            accepted: false,
            unfilled: "deep",
        },
        Row {
            case: "a name in the reserved namespace that names no variable".to_string(),
            paths: Some(paths()),
            template: "{\"r\": <aws.events.nope>}".to_string(),
            accepted: false,
            unfilled: "aws.events.nope",
        },
    ];
    // Each variable EventBridge fills itself is accepted without the map
    // declaring it, because the map is not what supplies it.
    for variable in RESERVED_VARIABLES {
        rows.push(Row {
            case: format!("the variable {variable}, which EventBridge fills itself"),
            paths: None,
            template: format!("{{\"v\": <{variable}>}}"),
            accepted: true,
            unfilled: "",
        });
    }
    rows
}

/// The `InputTransformer` a row configures its target with.
fn transformer(row: &Row) -> Value {
    let mut transformer = json!({ "InputTemplate": row.template });
    if let Some(paths) = &row.paths {
        transformer["InputPathsMap"] = paths.clone();
    }
    transformer
}

/// The ids of the targets the rule carries, as `ListTargetsByRule` reports them.
async fn stored_target_ids(
    emulator: &crate::target_dispatch::Emulator,
    case: &str,
) -> Vec<String> {
    let (status, listed) = events(emulator, "ListTargetsByRule", &json!({ "Rule": RULE })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTargetsByRule answered {status} {listed}"
    );
    let mut ids: Vec<String> = listed["Targets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|target| target["Id"].as_str().unwrap_or_default().to_string())
        .collect();
    ids.sort();
    ids
}

/// A template whose every placeholder is filled is accepted and delivers; one
/// naming a placeholder nothing fills is refused and changes nothing.
#[tokio::test]
async fn a_template_placeholder_nothing_fills_is_refused_when_the_target_is_configured() {
    for row in rows() {
        let case = row.case.as_str();
        let emulator = emulator();
        let (url, arn) = given_queue(&emulator, "transformed").await;
        // The rule already carries a target, so a refusal is read against a
        // target list that must not change.
        given_rule(
            &emulator,
            RULE,
            json!([{ "Id": KEPT_TARGET_ID, "Arn": arn }]),
        )
        .await;

        let (status, body) = events(
            &emulator,
            "PutTargets",
            &json!({
                "Rule": RULE,
                "Targets": [{
                    "Id": TARGET_ID,
                    "Arn": arn,
                    "InputTransformer": transformer(&row),
                }],
            }),
        )
        .await;

        if !row.accepted {
            assert_eq!(
                status, 400,
                "case {case:?}: expected {:?} to be refused, got {status} {body}",
                row.template
            );
            let message = body["message"].as_str().unwrap_or_default();
            assert_eq!(
                message,
                format!(
                    "InputTemplate for target {TARGET_ID} contains invalid placeholder {}.",
                    row.unfilled
                ),
                "case {case:?}: expected the refusal naming the member, the target and the \
                 placeholder, got {message:?}"
            );
            let ids = stored_target_ids(&emulator, case).await;
            assert_eq!(
                ids,
                vec![KEPT_TARGET_ID.to_string()],
                "case {case:?}: a refused request leaves the rule's targets alone, got {ids:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected {:?} to be accepted, got {status} {body}",
            row.template
        );
        assert_eq!(
            body["FailedEntryCount"],
            json!(0),
            "case {case:?}: PutTargets reported a failed entry: {body}"
        );
        let ids = stored_target_ids(&emulator, case).await;
        assert_eq!(
            ids,
            vec![KEPT_TARGET_ID.to_string(), TARGET_ID.to_string()],
            "case {case:?}: expected the configured target beside the one the rule had, got \
             {ids:?}"
        );

        // An accepted template is one every delivery can render, so the event
        // reaches the target: both of the rule's targets are the one queue, so
        // the untransformed envelope and the rendering both land there.
        publish_details(&emulator, &[detail().to_string()]).await;
        let bodies: Vec<String> = drain(&emulator, &url)
            .await
            .iter()
            .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            bodies.len(),
            2,
            "case {case:?}: expected the envelope and the rendering on the queue, got {bodies:?}"
        );
        let envelope = json!({ "source": SOURCE, "detail-type": DETAIL_TYPE });
        let rendered: Vec<&String> = bodies
            .iter()
            .filter(|body| {
                let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                parsed["source"] != envelope["source"]
                    || parsed["detail-type"] != envelope["detail-type"]
            })
            .collect();
        assert_eq!(
            rendered.len(),
            1,
            "case {case:?}: expected exactly one of the two deliveries to be the rendering, \
             got {bodies:?}"
        );
    }
}
