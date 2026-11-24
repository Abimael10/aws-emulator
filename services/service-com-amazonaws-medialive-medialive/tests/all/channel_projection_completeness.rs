// SPDX-License-Identifier: CC0-1.0

//! The configuration a channel is STORED with is COMPLETE, so every
//! channel-shaped response reports it complete, end to end over the restJson1
//! wire.
//!
//! Two kinds of member are at stake, and both are about what a client reads
//! rather than about what one request spelled:
//!
//! * a collection member of the channel's configuration the request supplied
//!   nothing for is reported as an EMPTY collection, never as an absent member,
//!   so a client that iterates it walks a collection with no entries instead of
//!   a null one;
//! * `logLevel` — "The log level being written to CloudWatch Logs" — is reported
//!   for every channel, at the level the request chose or at the one the service
//!   applies to a channel that chose none.
//!
//! Every test drives the service the way a client does — through
//! `MultiServiceRouter::dispatch` with real HTTP requests — so what is asserted
//! is the emulator's wire behaviour, not an internal function's return value.
//!
//! The suites are tables whose rows differ along the SHAPE of the configuration
//! the request supplies: which collections it names, how many entries each
//! carries, how many destinations, attachments, output groups and outputs there
//! are, and which kind of output group settings each group carries.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, medialive};

/// Every value `LogLevel` declares, in the model's own order
/// (`smithy_query(query="shape", shape="LogLevel")`: ERROR | WARNING | INFO |
/// DEBUG | DISABLED), which is the set a channel's reported level comes from.
const LOG_LEVELS: [&str; 5] = ["ERROR", "WARNING", "INFO", "DEBUG", "DISABLED"];

/// The level a channel whose request chose none writes at: it writes no log,
/// which is the value `LogLevel` declares for writing none.
const LEVEL_FOR_NO_LOGGING: &str = "DISABLED";

/// One covered collection of a channel-shaped body: the path a client reads it
/// at, and how many entries it is expected to carry.
type Expected = (&'static str, usize);

/// Create one channel, with the references and the encoder settings a channel
/// configuration needs filled in by the shared helper.
async fn create(router: &MultiServiceRouter, case: &str, body: Value) -> Value {
    let body = crate::support::resolved_channel_body(router, body).await;
    let (status, response) = call(router, "POST", "/prod/channels", Some(body)).await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateChannel: expected 201, got {status}: {response}"
    );
    response["channel"].clone()
}

async fn describe(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(router, "GET", &format!("/prod/channels/{id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeChannel: expected 200, got {status}: {body}"
    );
    body
}

/// The `ChannelSummary` `ListChannels` reports for `id`.
async fn summary(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, listed) = call(router, "GET", "/prod/channels", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListChannels: expected 200, got {status}: {listed}"
    );
    listed["channels"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|channel| channel["id"] == json!(id))
        .unwrap_or(Value::Null)
}

/// The channel once the clock has settled the create into `IDLE`, which is the
/// state a delete and an update are accepted in.
///
/// The poll itself is the one the channel suites share
/// ([`crate::channel_lifecycle::described_once_in`]): it waits for the CONDITION — the
/// state the channel reports — rather than a fixed time, and gives up only on a deadline
/// generous enough that a loaded host cannot fail it.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    crate::channel_lifecycle::described_once_in(router, case, id, "IDLE").await
}

/// The entries of one member of `body`, or an empty list when the body carries
/// no such member.
fn entries(body: &Value, member: &str) -> Vec<Value> {
    body[member].as_array().cloned().unwrap_or_default()
}

/// Every collection of one channel-shaped body the completeness rule covers,
/// each with the path a client reads it at: the collections of the channel's
/// input attachments and output destinations, and — in the projections that
/// carry `encoderSettings`, which the `ListChannels` summary does not — the
/// collections of its encoder settings, output groups and outputs.
fn collections(body: &Value) -> Vec<(String, Value)> {
    let mut found: Vec<(String, Value)> = Vec::new();
    for (index, attachment) in entries(body, "inputAttachments").iter().enumerate() {
        found.push((
            format!("inputAttachments[{index}].logicalInterfaceNames"),
            attachment["logicalInterfaceNames"].clone(),
        ));
    }
    for (index, destination) in entries(body, "destinations").iter().enumerate() {
        for member in [
            "mediaPackageSettings",
            "settings",
            "srtSettings",
            "logicalInterfaceNames",
        ] {
            found.push((
                format!("destinations[{index}].{member}"),
                destination[member].clone(),
            ));
        }
    }
    let encoder = &body["encoderSettings"];
    if encoder.is_null() {
        return found;
    }
    found.push((
        "encoderSettings.captionDescriptions".to_string(),
        encoder["captionDescriptions"].clone(),
    ));
    for (group_index, group) in entries(encoder, "outputGroups").iter().enumerate() {
        let group_path = format!("encoderSettings.outputGroups[{group_index}]");
        let hls = &group["outputGroupSettings"]["hlsGroupSettings"];
        if !hls.is_null() {
            for member in ["adMarkers", "captionLanguageMappings"] {
                found.push((
                    format!("{group_path}.outputGroupSettings.hlsGroupSettings.{member}"),
                    hls[member].clone(),
                ));
            }
        }
        for (output_index, output) in entries(group, "outputs").iter().enumerate() {
            for member in ["audioDescriptionNames", "captionDescriptionNames"] {
                found.push((
                    format!("{group_path}.outputs[{output_index}].{member}"),
                    output[member].clone(),
                ));
            }
        }
    }
    found
}

/// Assert that one channel-shaped body reports exactly the covered collections
/// `expected` names, each as a collection carrying the number of entries the row
/// says it does.
fn assert_collections(case: &str, read: &str, body: &Value, expected: &[Expected]) {
    let found = collections(body);
    let paths: Vec<&str> = found.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(
        found.len(),
        expected.len(),
        "case {case:?}: {read}: expected the covered collections {expected:?}, got {paths:?}"
    );
    for (path, value) in &found {
        let Some((_, wanted)) = expected.iter().find(|(wanted, _)| wanted == path) else {
            panic!("case {case:?}: {read}: {path} is not one of the covered collections {expected:?}");
        };
        assert_eq!(
            value.as_array().map(Vec::len),
            Some(*wanted),
            "case {case:?}: {read}: expected {path} to be a collection of {wanted} entry/entries, got {value:?}"
        );
    }
}

/// Assert the level one channel-shaped body reports.
fn assert_log_level(case: &str, read: &str, body: &Value, wanted: &str) {
    assert_eq!(
        body["logLevel"],
        json!(wanted),
        "case {case:?}: {read}: expected logLevel {wanted:?}, got {:?}",
        body["logLevel"]
    );
}

/// `EncoderSettings` carrying the members the model marks required
/// (`smithy_query(query="shape", shape="EncoderSettings")`: AudioDescriptions,
/// OutputGroups, TimecodeConfig, VideoDescriptions) and the `output_groups` the
/// row describes.
fn encoder_settings(output_groups: Value) -> Value {
    json!({
        "audioDescriptions": [],
        "videoDescriptions": [],
        "timecodeConfig": { "source": "EMBEDDED" },
        "outputGroups": output_groups,
    })
}

/// Rule: a channel's configuration is stored complete, so every channel-shaped
/// response reports every collection of that configuration AS a collection —
/// empty where the request supplied nothing for it, and carrying exactly the
/// entries the request supplied where it did.
///
/// The rows differ along the shape of the configuration: a channel that supplies
/// no collection at all (the case the issue reports), one whose destinations and
/// attachments supply some of theirs, and one whose encoder settings carry two
/// output groups of different kinds with several outputs between them. Each row
/// is asserted on all four channel-shaped responses a client reads the channel
/// through: the create, the describe, the list summary and the delete.
#[tokio::test]
async fn every_channel_response_reports_the_configuration_collections_as_collections() {
    // (case, the CreateChannel body, the covered collections and their entry
    //  counts)
    let table: [(&str, Value, &[Expected]); 3] = [
        (
            "no collection supplied",
            json!({
                "name": "channel-bare",
                "destinations": [{ "id": "d0" }],
                "inputAttachments": [{ "inputId": "input-a", "inputAttachmentName": "a" }],
            }),
            &[
                ("inputAttachments[0].logicalInterfaceNames", 0),
                ("destinations[0].mediaPackageSettings", 0),
                ("destinations[0].settings", 0),
                ("destinations[0].srtSettings", 0),
                ("destinations[0].logicalInterfaceNames", 0),
                ("encoderSettings.captionDescriptions", 0),
            ],
        ),
        (
            "destinations and attachments supplying some of their collections",
            json!({
                "name": "channel-destinations",
                "destinations": [
                    {
                        "id": "d0",
                        // One per-encoder setting per pipeline of the class this
                        // channel has (`OutputDestination$Settings`: "one
                        // destination for each redundant encoder"), which is
                        // what a create requires — the body names no class, so
                        // the service's two-pipeline default applies.
                        "settings": [
                            { "url": "rtmp://example.com/live/a" },
                            { "url": "rtmp://example.com/live/b" },
                        ],
                        "logicalInterfaceNames": ["eth0"],
                    },
                    {
                        "id": "d1",
                        "srtSettings": [
                            { "url": "srt://example.com:9000" },
                            { "url": "srt://example.com:9001" },
                        ],
                    },
                ],
                "inputAttachments": [
                    {
                        "inputId": "input-a",
                        "inputAttachmentName": "a",
                        "logicalInterfaceNames": ["eth0", "eth1"],
                    },
                    { "inputId": "input-b", "inputAttachmentName": "b" },
                ],
            }),
            &[
                ("inputAttachments[0].logicalInterfaceNames", 2),
                ("inputAttachments[1].logicalInterfaceNames", 0),
                ("destinations[0].mediaPackageSettings", 0),
                ("destinations[0].settings", 2),
                ("destinations[0].srtSettings", 0),
                ("destinations[0].logicalInterfaceNames", 1),
                ("destinations[1].mediaPackageSettings", 0),
                ("destinations[1].settings", 0),
                ("destinations[1].srtSettings", 2),
                ("destinations[1].logicalInterfaceNames", 0),
                ("encoderSettings.captionDescriptions", 0),
            ],
        ),
        (
            "two output groups of different kinds",
            json!({
                "name": "channel-output-groups",
                "encoderSettings": encoder_settings(json!([
                    {
                        "name": "hls-plain",
                        "outputGroupSettings": {
                            "hlsGroupSettings": { "destination": { "destinationRefId": "d0" } },
                        },
                        "outputs": [
                            { "outputName": "o0", "outputSettings": {} },
                            {
                                "outputName": "o1",
                                "outputSettings": {},
                                "audioDescriptionNames": ["audio-0"],
                                "captionDescriptionNames": ["caption-0", "caption-1"],
                            },
                        ],
                    },
                    {
                        "name": "hls-marked",
                        "outputGroupSettings": {
                            "hlsGroupSettings": {
                                "destination": { "destinationRefId": "d0" },
                                // `HlsAdMarkers` (`smithy_query(query="shape",
                                // shape="HlsAdMarkers")`: ADOBE | ELEMENTAL |
                                // ELEMENTAL_SCTE35).
                                "adMarkers": ["ELEMENTAL", "ELEMENTAL_SCTE35"],
                                "captionLanguageMappings": [{
                                    "captionChannel": 1,
                                    "languageCode": "eng",
                                    "languageDescription": "English",
                                }],
                            },
                        },
                        "outputs": [{ "outputName": "o2", "outputSettings": {} }],
                    },
                    {
                        "name": "archive",
                        "outputGroupSettings": {
                            "archiveGroupSettings": {
                                "destination": { "destinationRefId": "d0" },
                            },
                        },
                        "outputs": [{ "outputName": "o3", "outputSettings": {} }],
                    },
                ])),
            }),
            &[
                ("encoderSettings.captionDescriptions", 0),
                ("encoderSettings.outputGroups[0].outputGroupSettings.hlsGroupSettings.adMarkers", 0),
                ("encoderSettings.outputGroups[0].outputGroupSettings.hlsGroupSettings.captionLanguageMappings", 0),
                ("encoderSettings.outputGroups[0].outputs[0].audioDescriptionNames", 0),
                ("encoderSettings.outputGroups[0].outputs[0].captionDescriptionNames", 0),
                ("encoderSettings.outputGroups[0].outputs[1].audioDescriptionNames", 1),
                ("encoderSettings.outputGroups[0].outputs[1].captionDescriptionNames", 2),
                ("encoderSettings.outputGroups[1].outputGroupSettings.hlsGroupSettings.adMarkers", 2),
                ("encoderSettings.outputGroups[1].outputGroupSettings.hlsGroupSettings.captionLanguageMappings", 1),
                ("encoderSettings.outputGroups[1].outputs[0].audioDescriptionNames", 0),
                ("encoderSettings.outputGroups[1].outputs[0].captionDescriptionNames", 0),
                ("encoderSettings.outputGroups[2].outputs[0].audioDescriptionNames", 0),
                ("encoderSettings.outputGroups[2].outputs[0].captionDescriptionNames", 0),
            ],
        ),
    ];

    for (case, body, expected) in table {
        let router = medialive();
        let created = create(&router, case, body).await;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        assert_collections(case, "CreateChannel", &created, expected);

        // The same collections read back out of the store, through every
        // channel-shaped response a client reads the channel through. The list
        // summary carries no `encoderSettings` member at all
        // (`smithy_query(query="shape", shape="ChannelSummary")`), so the
        // encoder's own collections are not among the ones it reports.
        let described = settled(&router, case, &id).await;
        assert_collections(case, "DescribeChannel", &described, expected);

        let summary_expected: Vec<Expected> = expected
            .iter()
            .copied()
            .filter(|(path, _)| !path.starts_with("encoderSettings"))
            .collect();
        assert_collections(
            case,
            "the ListChannels summary",
            &summary(&router, case, &id).await,
            &summary_expected,
        );

        let (status, deleted) = call(&router, "DELETE", &format!("/prod/channels/{id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteChannel: expected 200, got {status}: {deleted}"
        );
        assert_collections(case, "DeleteChannel", &deleted, expected);
    }
}

/// Rule: a channel reports the log level it is writing at — the level the
/// request chose, or the level for writing no log at all where the request chose
/// none — in every channel-shaped response.
///
/// The rows span the `LogLevel` enum, including the four values the issue did not
/// cite, plus the row that names no level at all.
#[tokio::test]
async fn every_channel_response_reports_the_log_level_the_channel_writes_at() {
    let mut table: Vec<(String, Option<&str>, &str)> = vec![(
        "no logLevel named".to_string(),
        None,
        LEVEL_FOR_NO_LOGGING,
    )];
    for level in LOG_LEVELS {
        table.push((format!("logLevel {level}"), Some(level), level));
    }

    for (case, requested, wanted) in table {
        let case = case.as_str();
        let router = medialive();
        let mut body = json!({ "name": "channel-log-level" });
        if let Some(level) = requested {
            body["logLevel"] = json!(level);
        }

        let created = create(&router, case, body).await;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        assert_log_level(case, "CreateChannel", &created, wanted);
        assert_log_level(
            case,
            "DescribeChannel",
            &describe(&router, case, &id).await,
            wanted,
        );
        assert_log_level(
            case,
            "the ListChannels summary",
            &summary(&router, case, &id).await,
            wanted,
        );
    }
}

/// Rule: an update stores the configuration it rewrites just as complete as a
/// create does — the collections it supplies nothing for are empty in the
/// response to the update itself and in every later read, and a channel that
/// still has no level of its own goes on reporting the level for writing no log.
///
/// The rows differ along which part of the configuration the update rewrites.
#[tokio::test]
async fn an_update_stores_the_rewritten_configuration_just_as_complete() {
    // (case, the UpdateChannel body, the covered collections and their entry
    //  counts)
    let table: [(&str, Value, &[Expected]); 2] = [
        (
            "an update that rewrites the destinations",
            json!({ "destinations": [{ "id": "d1" }, { "id": "d2" }] }),
            &[
                ("destinations[0].mediaPackageSettings", 0),
                ("destinations[0].settings", 0),
                ("destinations[0].srtSettings", 0),
                ("destinations[0].logicalInterfaceNames", 0),
                ("destinations[1].mediaPackageSettings", 0),
                ("destinations[1].settings", 0),
                ("destinations[1].srtSettings", 0),
                ("destinations[1].logicalInterfaceNames", 0),
                ("encoderSettings.captionDescriptions", 0),
            ],
        ),
        (
            "an update that rewrites the encoder settings",
            json!({
                "encoderSettings": encoder_settings(json!([{
                    "name": "hls",
                    "outputGroupSettings": {
                        "hlsGroupSettings": { "destination": { "destinationRefId": "d0" } },
                    },
                    "outputs": [{ "outputName": "o0", "outputSettings": {} }],
                }])),
            }),
            &[
                ("encoderSettings.captionDescriptions", 0),
                ("encoderSettings.outputGroups[0].outputGroupSettings.hlsGroupSettings.adMarkers", 0),
                ("encoderSettings.outputGroups[0].outputGroupSettings.hlsGroupSettings.captionLanguageMappings", 0),
                ("encoderSettings.outputGroups[0].outputs[0].audioDescriptionNames", 0),
                ("encoderSettings.outputGroups[0].outputs[0].captionDescriptionNames", 0),
            ],
        ),
    ];

    for (case, patch, expected) in table {
        let router = medialive();
        let created = create(&router, case, json!({ "name": "channel-to-update" })).await;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        // An update is accepted on a channel that has stopped, so the create's
        // transition has to have settled first.
        settled(&router, case, &id).await;

        let patch = crate::support::resolved_channel_references(&router, patch).await;
        let (status, updated) = call(
            &router,
            "PUT",
            &format!("/prod/channels/{id}"),
            Some(patch),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateChannel: expected 200, got {status}: {updated}"
        );
        assert_collections(case, "UpdateChannel", &updated["channel"], expected);
        assert_log_level(case, "UpdateChannel", &updated["channel"], LEVEL_FOR_NO_LOGGING);

        let described = settled(&router, case, &id).await;
        assert_collections(case, "DescribeChannel after the update", &described, expected);
        assert_log_level(
            case,
            "DescribeChannel after the update",
            &described,
            LEVEL_FOR_NO_LOGGING,
        );
    }
}
