// SPDX-License-Identifier: CC0-1.0

//! The TEXT of the refusal every ARN-addressed tag operation answers a resource it cannot
//! resolve with, over the wire.
//!
//! A tag operation identifies its resource by nothing but the `ResourceARN` the request
//! carried, so the refusal names the thing it could not find BY that ARN: the sentence is
//! `Resource with ARN <ResourceARN> not found`, with the ARN copied from the request
//! verbatim and no slot for anything else — the same text for `TagResource`,
//! `UntagResource` and `ListTagsForResource`, since all three resolve the ARN the same way.
//!
//! Two tables span the class:
//!
//! * a resource of each taggable kind that WAS stored, tagged, and then deleted — the ARN
//!   the service itself minted, which now names nothing;
//! * ARNs that never named a resource here, spanning a kind no resource carries, a stored
//!   id under another account and under another Region, a kind this service does not tag,
//!   an identifier that is not an ARN at all, and both bounds of the modelled
//!   `AmazonResourceName` (`smithy_query operation=ListTagsForResource`, len 1..1011).
//!
//! The harness (`GameLift`, `gamelift_arn`, `error_code`, `error_message`) is the one
//! `hosting_loop.rs` defines and drives the production dispatch path with.

use serde_json::{json, Value};

use super::hosting_loop::{error_code, error_message, gamelift_arn, GameLift, REGION};
use super::scripts::{zip_bytes, zip_file_member};

/// The bounds of `ListTagsForResource$ResourceARN`, `TagResource$ResourceARN` and
/// `UntagResource$ResourceARN`: all three target `AmazonResourceName`, len 1..1011
/// (`smithy_query operation=ListTagsForResource,TagResource,UntagResource`).
const MIN_RESOURCE_ARN_LENGTH: usize = 1;
/// See [`MIN_RESOURCE_ARN_LENGTH`].
const MAX_RESOURCE_ARN_LENGTH: usize = 1011;

/// The rule set the matchmaking rows need: a body with the one team
/// `CreateMatchmakingRuleSet` accepts.
fn rule_set_body() -> String {
    json!({
        "name": "solo-duel",
        "ruleLanguageVersion": "1.0",
        "teams": [{"name": "solo", "minPlayers": 1, "maxPlayers": 2}],
    })
    .to_string()
}

/// One taggable resource kind: the create that makes one, where that create reports the
/// ARN, and the delete that takes the resource away again.
struct Kind {
    /// The resource kind, as its ARN names it.
    label: &'static str,
    create_op: &'static str,
    /// The two members the create's response nests the ARN under.
    record: &'static str,
    arn_member: &'static str,
    create: Value,
    delete_op: &'static str,
    delete_member: &'static str,
    /// The value the delete names the resource by, when that is not its ARN.
    delete_value: Option<&'static str>,
}

/// The taggable kinds whose resources a delete removes outright, so the ARN the create
/// reported then names nothing. The fleet is left to `resource_deletion.rs`: its delete is
/// a clock-owed teardown rather than an immediate removal.
fn kinds() -> Vec<Kind> {
    vec![
        Kind {
            label: "alias",
            create_op: "CreateAlias",
            record: "Alias",
            arn_member: "AliasArn",
            create: json!({
                "Name": "gone-alias",
                "RoutingStrategy": {"Type": "TERMINAL", "Message": "retired"},
                "Tags": [{"Key": "owner", "Value": "liveops"}],
            }),
            delete_op: "DeleteAlias",
            delete_member: "AliasId",
            delete_value: None,
        },
        Kind {
            label: "build",
            create_op: "CreateBuild",
            record: "Build",
            arn_member: "BuildArn",
            create: json!({
                "Name": "gone-build",
                "Version": "1.0.0",
                "OperatingSystem": "AMAZON_LINUX_2023",
                "Tags": [{"Key": "env", "Value": "dev"}],
            }),
            delete_op: "DeleteBuild",
            delete_member: "BuildId",
            delete_value: None,
        },
        Kind {
            label: "script",
            create_op: "CreateScript",
            record: "Script",
            arn_member: "ScriptArn",
            create: json!({
                "Name": "gone-script",
                "Version": "1.0.0",
                "NodeJsVersion": "24.x",
                "ZipFile": zip_file_member(&zip_bytes(256)),
            }),
            delete_op: "DeleteScript",
            delete_member: "ScriptId",
            delete_value: None,
        },
        Kind {
            label: "gamesessionqueue",
            create_op: "CreateGameSessionQueue",
            record: "GameSessionQueue",
            arn_member: "GameSessionQueueArn",
            create: json!({
                "Name": "gone-queue",
                "Tags": [{"Key": "pool", "Value": "spot"}],
            }),
            delete_op: "DeleteGameSessionQueue",
            delete_member: "Name",
            delete_value: Some("gone-queue"),
        },
        Kind {
            label: "matchmakingruleset",
            create_op: "CreateMatchmakingRuleSet",
            record: "RuleSet",
            arn_member: "RuleSetArn",
            create: json!({"Name": "gone-rules", "RuleSetBody": rule_set_body()}),
            delete_op: "DeleteMatchmakingRuleSet",
            delete_member: "Name",
            delete_value: Some("gone-rules"),
        },
        Kind {
            label: "matchmakingconfiguration",
            create_op: "CreateMatchmakingConfiguration",
            record: "Configuration",
            arn_member: "ConfigurationArn",
            create: json!({
                "Name": "gone-config",
                "RuleSetName": "kept-rules",
                "RequestTimeoutSeconds": 60,
                "AcceptanceRequired": false,
                "FlexMatchMode": "STANDALONE",
                "Tags": [{"Key": "team", "Value": "red"}],
            }),
            delete_op: "DeleteMatchmakingConfiguration",
            delete_member: "Name",
            delete_value: Some("gone-config"),
        },
        Kind {
            label: "location",
            create_op: "CreateLocation",
            record: "Location",
            arn_member: "LocationArn",
            create: json!({
                "LocationName": "custom-gone-location",
                "Tags": [{"Key": "pool", "Value": "on-premises"}],
            }),
            delete_op: "DeleteLocation",
            delete_member: "LocationName",
            delete_value: Some("custom-gone-location"),
        },
    ]
}

/// Every tag operation, each with the rest of its request held valid so the refusal is
/// about the resource the ARN named.
fn tag_requests(resource_arn: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "TagResource",
            json!({"ResourceARN": resource_arn, "Tags": [{"Key": "k", "Value": "v"}]}),
        ),
        (
            "UntagResource",
            json!({"ResourceARN": resource_arn, "TagKeys": ["k"]}),
        ),
        (
            "ListTagsForResource",
            json!({"ResourceARN": resource_arn}),
        ),
    ]
}

/// Assert each tag operation refuses `resource_arn` with HTTP 400, `NotFoundException` and
/// exactly the sentence the service words that class with.
async fn assert_every_tag_operation_refuses(gamelift: &GameLift, resource_arn: &str, case: &str) {
    let expected = format!("Resource with ARN {resource_arn} not found");
    for (operation, request) in tag_requests(resource_arn) {
        let (status, body) = gamelift.call(operation, &request).await;
        let got = format!("{}|{}", error_code(&body), error_message(&body));
        let wanted = format!("NotFoundException|{expected}");
        assert_eq!(
            (status, got.as_str()),
            (400, wanted.as_str()),
            "case {case:?} on {operation}: expected HTTP 400 with {wanted:?}, got HTTP \
             {status} with {got:?}"
        );
    }
}

/// A resource that was stored, tagged and then deleted is gone for the tag operations too:
/// each answers the ARN the service minted for it with the one not-found sentence, for
/// every taggable kind.
#[tokio::test]
async fn a_deleted_resource_is_refused_by_every_tag_operation_by_its_own_arn() {
    let gamelift = GameLift::new();
    // The rule set the matchmaking configuration row matches with, which outlives it.
    gamelift
        .ok(
            "CreateMatchmakingRuleSet",
            json!({"Name": "kept-rules", "RuleSetBody": rule_set_body()}),
        )
        .await;

    for kind in kinds() {
        let label = kind.label;
        let created = gamelift.ok(kind.create_op, kind.create.clone()).await;
        let resource_arn = created[kind.record][kind.arn_member]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {label:?}: {} must report an ARN, got {created}", kind.create_op)
            })
            .to_string();

        // While it is stored, its tags are served: the refusal below is the deletion, not a
        // blanket answer.
        let listed = gamelift
            .ok(
                "ListTagsForResource",
                json!({"ResourceARN": &resource_arn}),
            )
            .await;
        assert!(
            listed["Tags"].is_array(),
            "case {label:?}: the stored resource must report its tags, got {listed}"
        );

        let delete_value = kind
            .delete_value
            .map(str::to_string)
            .unwrap_or_else(|| resource_arn.clone());
        let mut delete_request = serde_json::Map::new();
        delete_request.insert(kind.delete_member.to_string(), json!(delete_value));
        gamelift
            .ok(kind.delete_op, Value::Object(delete_request))
            .await;

        assert_every_tag_operation_refuses(
            &gamelift,
            &resource_arn,
            &format!("the deleted {label}"),
        )
        .await;
    }
}

/// An ARN that never named a resource this service holds is refused by every tag operation
/// with the same sentence, carrying the value the request supplied verbatim.
///
/// The rows differ in the kind the ARN names, in its account and Region, in whether it is
/// an ARN at all, and in its length — both modelled bounds of `AmazonResourceName`.
#[tokio::test]
async fn an_arn_that_names_nothing_is_refused_with_the_arn_the_request_carried() {
    let gamelift = GameLift::new();
    let created = gamelift
        .ok(
            "CreateGameSessionQueue",
            json!({
                "Name": "kept-queue",
                "Tags": [{"Key": "owner", "Value": "liveops"}],
            }),
        )
        .await;
    let stored_arn = created["GameSessionQueue"]["GameSessionQueueArn"]
        .as_str()
        .expect("CreateGameSessionQueue must report an ARN")
        .to_string();

    // The longest and the shortest value the member accepts, neither of which resolves.
    let longest = {
        let prefix = gamelift_arn("alias/alias-");
        format!(
            "{prefix}{}",
            "z".repeat(MAX_RESOURCE_ARN_LENGTH - prefix.len())
        )
    };
    let shortest = "z".repeat(MIN_RESOURCE_ARN_LENGTH);

    let rows = vec![
        (
            "an ARN of a taggable kind no resource carries",
            gamelift_arn("fleet/fleet-never-created"),
        ),
        (
            "a stored resource's name under another account",
            format!("arn:aws:gamelift:{REGION}:999999999999:gamesessionqueue/kept-queue"),
        ),
        (
            "a stored resource's name under another Region",
            format!(
                "arn:aws:gamelift:eu-central-1:{}:gamesessionqueue/kept-queue",
                engine::emulator_account_id()
            ),
        ),
        (
            "an ARN of a kind this service does not tag",
            gamelift_arn("gamesession/fleet-abc/session-abc"),
        ),
        (
            "an ARN of another service's resource",
            format!(
                "arn:aws:s3:{REGION}:{}:bucket/not-a-gamelift-resource",
                engine::emulator_account_id()
            ),
        ),
        ("an identifier that is not an ARN", "kept-queue".to_string()),
        (
            "the longest value the model allows",
            longest.clone(),
        ),
        ("the shortest value the model allows", shortest.clone()),
    ];

    for (label, resource_arn) in &rows {
        assert!(
            (MIN_RESOURCE_ARN_LENGTH..=MAX_RESOURCE_ARN_LENGTH).contains(&resource_arn.len()),
            "case {label:?}: the row must stay inside the modelled length, got {} characters",
            resource_arn.len()
        );
        assert_every_tag_operation_refuses(&gamelift, resource_arn, label).await;
    }

    // The control: the resource that IS stored still serves the tags its create assigned,
    // so none of the refusals above touched it.
    let listed = gamelift
        .ok("ListTagsForResource", json!({"ResourceARN": &stored_arn}))
        .await;
    assert_eq!(
        listed["Tags"],
        json!([{"Key": "owner", "Value": "liveops"}]),
        "the stored queue must still carry the tags its create assigned, got {listed}"
    );
}
