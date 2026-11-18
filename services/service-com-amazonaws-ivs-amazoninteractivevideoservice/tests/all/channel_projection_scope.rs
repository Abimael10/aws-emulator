// SPDX-License-Identifier: CC0-1.0

//! WHERE the channel wire projection reaches, driven over the wire through
//! `MultiServiceRouter::dispatch` exactly as a signed `aws ivs` call arrives.
//!
//! The rule under test: a channel object reports each of its optional configuration ARNs
//! as a PRESENT key holding a string — the ARN of the resource configured for it, or the
//! empty string when none is — and a structure that is NOT a channel reports no such
//! member at all. The two halves are one rule: the failover-slate ARN is a member of a
//! channel, so every channel object a list projects carries it whatever that channel
//! carries otherwise, and nothing else of a response does — not the response envelope a
//! list answers in, not the summaries of another resource, and not a channel's own tag
//! map.
//!
//! `channel_configuration_arns.rs` pins the first half across the projections of one
//! channel and `list_channels_failover_projection.rs` across the shape of a list; this
//! suite pins the summaries of ONE list whose channels differ in what they have
//! configured — which is where a summary carrying its own ARNs is reached beside one
//! carrying none — and the BOUNDARY, the structures the member must not appear on.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::channel_lifecycle::{text, Ivs};

/// The member the recorded IVS responses carry on every projected channel and that this
/// model revision declares on neither channel shape
/// (`smithy_query(query="shape",shape="Channel,ChannelSummary")`), so it is served by the
/// channel wire projection rather than by a generated field.
const FAILOVER_SLATE: &str = "failoverSlateConfigurationArn";

/// Every configuration ARN a channel summary reports: the three `ChannelSummary`
/// declares, each documented "Default: \"\""
/// (`smithy_query(query="shape",shape="ChannelSummary")`), and the projected one, which
/// reads the same way.
const CONFIGURATION_ARNS: &[&str] = &[
    "recordingConfigurationArn",
    "playbackRestrictionPolicyArn",
    "adConfigurationArn",
    FAILOVER_SLATE,
];

/// The value a configuration ARN reads as while nothing is configured for it.
const UNCONFIGURED: &str = "";

/// A playback-restriction-policy ARN of the form the modelled
/// `ChannelPlaybackRestrictionPolicyArn` pattern
/// `^^$|^arn:aws:ivs:[a-z0-9-]+:[0-9]+:playback-restriction-policy/[a-zA-Z0-9-]+$$`
/// admits, written out rather than minted because `CreatePlaybackRestrictionPolicy` is
/// still scaffolded in this emulator: what the rows below vary is which ARN a channel
/// carries, and a channel create stores the ARN the request names.
pub(crate) const POLICY_ARN: &str =
    "arn:aws:ivs:us-east-1:000000000000:playback-restriction-policy/listed";

/// Mint a recording configuration and answer its ARN, so a row attaches a resource this
/// emulator really holds.
pub(crate) async fn recording_configuration(ivs: &Ivs, name: &str) -> String {
    let created = ivs
        .ok(
            "CreateRecordingConfiguration",
            json!({ "name": name,
                    "destinationConfiguration": { "s3": { "bucketName": "ivs-recordings" } } }),
        )
        .await;
    text(&created["recordingConfiguration"], "arn")
}

/// Mint an ad configuration and answer its ARN.
pub(crate) async fn ad_configuration(ivs: &Ivs, name: &str) -> String {
    let created = ivs
        .ok(
            "CreateAdConfiguration",
            json!({ "name": name,
                    "mediaTailorPlaybackConfigurations": [
                        { "playbackConfigurationArn":
                          "arn:aws:mediatailor:us-east-1:000000000000:playbackConfiguration/ads" }
                    ] }),
        )
        .await;
    text(&created["adConfiguration"], "arn")
}

/// Every object under `value`, at any depth, that carries `key`.
fn objects_carrying(value: &Value, key: &str) -> Vec<Value> {
    let mut found = Vec::new();
    collect_carriers(value, key, &mut found);
    found
}

fn collect_carriers(value: &Value, key: &str, found: &mut Vec<Value>) {
    match value {
        Value::Object(members) => {
            if members.contains_key(key) {
                found.push(value.clone());
            }
            for nested in members.values() {
                collect_carriers(nested, key, found);
            }
        }
        Value::Array(elements) => {
            for element in elements {
                collect_carriers(element, key, found);
            }
        }
        _ => {}
    }
}

/// One row of the mixed list below: the case name, the `CreateChannel` request that
/// creates the channel, and the configuration ARNs that channel must report as
/// configured — every other one reads as [`UNCONFIGURED`].
struct Row {
    case: &'static str,
    request: Value,
    configured: Vec<(&'static str, String)>,
}

/// Every summary of ONE list reports its OWN configuration ARNs, and the projected member
/// beside them, whatever the other channels of that list have configured.
#[tokio::test]
async fn every_summary_of_a_mixed_list_reports_its_own_configuration_arns() {
    let ivs = Ivs::new();
    let recording = recording_configuration(&ivs, "recording").await;
    let ad = ad_configuration(&ivs, "ads").await;

    // Rows differ along which configuration ARNs the channel carries — none, each one
    // alone, all three together — and, on the last row, whether the channel carries a TAG
    // whose key is the projected member's own name, which is a map entry and not a member
    // of the channel.
    let rows: Vec<Row> = vec![
        Row {
            case: "nothing configured",
            request: json!({ "name": "bare" }),
            configured: vec![],
        },
        Row {
            case: "a recording configuration only",
            request: json!({ "name": "recorded",
                             "recordingConfigurationArn": recording.clone() }),
            configured: vec![("recordingConfigurationArn", recording.clone())],
        },
        Row {
            case: "a playback restriction policy only",
            request: json!({ "name": "restricted", "playbackRestrictionPolicyArn": POLICY_ARN }),
            configured: vec![("playbackRestrictionPolicyArn", POLICY_ARN.to_string())],
        },
        Row {
            case: "an ad configuration only",
            request: json!({ "name": "monetised", "adConfigurationArn": ad.clone() }),
            configured: vec![("adConfigurationArn", ad.clone())],
        },
        Row {
            case: "all three configurable ARNs together",
            request: json!({ "name": "everything", "recordingConfigurationArn": recording.clone(),
                             "playbackRestrictionPolicyArn": POLICY_ARN,
                             "adConfigurationArn": ad.clone() }),
            configured: vec![
                ("recordingConfigurationArn", recording.clone()),
                ("playbackRestrictionPolicyArn", POLICY_ARN.to_string()),
                ("adConfigurationArn", ad.clone()),
            ],
        },
        Row {
            case: "a tag whose key is the projected member's name",
            request: json!({ "name": "tagged", "tags": { FAILOVER_SLATE: "a tag value" } }),
            configured: vec![],
        },
    ];

    let expected_count = rows.len();
    let mut expected: BTreeMap<String, Row> = BTreeMap::new();
    for row in rows {
        let arn = ivs.create_arn(row.request.clone()).await;
        expected.insert(arn, row);
    }

    let listed = ivs.ok("ListChannels", json!({})).await;
    let summaries = listed["channels"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        summaries.len(),
        expected_count,
        "case a mixed list: expected {expected_count} summaries, got {listed}"
    );

    for summary in &summaries {
        let arn = text(summary, "arn");
        let row = expected
            .get(&arn)
            .unwrap_or_else(|| panic!("case a mixed list: unexpected summary {summary}"));
        let case = row.case;
        for member in CONFIGURATION_ARNS {
            let want = row
                .configured
                .iter()
                .find(|(name, _)| name == member)
                .map(|(_, arn)| arn.clone())
                .unwrap_or_else(|| UNCONFIGURED.to_string());
            let actual = summary.get(*member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual,
                Value::String(want.clone()),
                "case {case:?}: the summary must report {member} as {want:?}, got {actual:?} \
                 in {summary}"
            );
        }
    }

    // The tag map of the last row is an entry of a map, not a member of the channel: the
    // projection writes the channel's own member and leaves the map exactly as stored.
    let tagged = summaries
        .iter()
        .find(|summary| text(summary, "name") == "tagged")
        .cloned()
        .unwrap_or(Value::Null);
    let tag = tagged
        .get("tags")
        .and_then(|tags| tags.get(FAILOVER_SLATE))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        tag,
        Value::String("a tag value".to_string()),
        "case a tag whose key is the projected member's name: the tag must be reported as \
         stored, got {tag:?} in {tagged}"
    );
}

/// In a list answer, the channel summaries are the ONLY objects carrying the projected
/// member: not the envelope the summaries are listed in, and not anything nested in one.
#[tokio::test]
async fn only_the_channel_summaries_of_a_list_carry_the_projected_member() {
    // Rows differ along how many channels the store holds and how many of them one
    // answer projects: an unpaged list of one, an unpaged list of several, and pages
    // smaller than the set, at the published lower bound of the modelled
    // `MaxChannelResults` (`range(min: 1, max: 100)`) and above it.
    let cases: Vec<(&str, usize, Option<usize>)> = vec![
        ("one channel, unpaged", 1, None),
        ("four channels, unpaged", 4, None),
        ("four channels, one per page", 4, Some(1)),
        ("four channels, three per page", 4, Some(3)),
    ];

    for (case, count, page_size) in cases {
        let ivs = Ivs::new();
        for index in 0..count {
            // The channels differ in type and flags, so a row is not a set of channels
            // that differ only by name.
            let mut request = match index % 3 {
                0 => json!({ "type": "BASIC" }),
                1 => json!({ "type": "STANDARD", "authorized": true }),
                _ => json!({ "type": "ADVANCED_SD", "latencyMode": "LOW" }),
            };
            request["name"] = json!(format!("chan-{index}"));
            ivs.create_arn(request).await;
        }

        let mut request = json!({});
        if let Some(page_size) = page_size {
            request["maxResults"] = json!(page_size);
        }
        let answer = ivs.ok("ListChannels", request).await;
        let summaries = answer["channels"].as_array().cloned().unwrap_or_default();
        let projected = page_size.unwrap_or(count).min(count);
        assert_eq!(
            summaries.len(),
            projected,
            "case {case:?}: expected {projected} summaries, got {answer}"
        );

        let carriers = objects_carrying(&answer, FAILOVER_SLATE);
        assert_eq!(
            carriers.len(),
            summaries.len(),
            "case {case:?}: exactly the {} channel summaries may carry {FAILOVER_SLATE}, \
             {} objects do in {answer}",
            summaries.len(),
            carriers.len()
        );
        for carrier in &carriers {
            assert!(
                summaries.contains(carrier),
                "case {case:?}: only a channel summary may carry {FAILOVER_SLATE}, found it \
                 on {carrier} in {answer}"
            );
        }
    }
}

/// One read of this service that answers a resource other than a channel: the case name,
/// the operation and its request, the output member the model declares the resource under
/// (`smithy_query(query="operation",operation=...)`), and the ARN that member must report
/// when the read has one to report.
struct Read {
    case: &'static str,
    operation: &'static str,
    request: Value,
    member: &'static str,
    reports: Option<String>,
}

/// A response with no channel object in it carries the projected member nowhere: the
/// member belongs to a channel, so another resource's summary does not gain it.
///
/// Each row also asserts what the read DOES report, so the absence above is an absence in
/// an answer that really carries its resource rather than in an empty one.
#[tokio::test]
async fn a_response_with_no_channel_carries_the_projected_member_nowhere() {
    let ivs = Ivs::new();
    let created = ivs
        .create(json!({ "name": "owner", "tags": { "team": "video" } }))
        .await;
    let channel = text(&created["channel"], "arn");
    let stream_key = text(&created["streamKey"], "arn");
    let recording = recording_configuration(&ivs, "recording").await;
    let ad = ad_configuration(&ivs, "ads").await;

    // One row per read of this service that answers a resource OTHER than a channel —
    // the stream keys a channel owns, a recording configuration, an ad configuration and
    // the playback key pairs — so the rows span the other shapes this service projects,
    // single, batched and listed.
    let reads: Vec<Read> = vec![
        Read {
            case: "the stream keys of a channel",
            operation: "ListStreamKeys",
            request: json!({ "channelArn": channel }),
            member: "streamKeys",
            reports: Some(stream_key.clone()),
        },
        Read {
            case: "one stream key",
            operation: "GetStreamKey",
            request: json!({ "arn": stream_key.clone() }),
            member: "streamKey",
            reports: Some(stream_key.clone()),
        },
        Read {
            case: "a batch of stream keys",
            operation: "BatchGetStreamKey",
            request: json!({ "arns": [stream_key.clone()] }),
            member: "streamKeys",
            reports: Some(stream_key.clone()),
        },
        Read {
            case: "the recording configurations",
            operation: "ListRecordingConfigurations",
            request: json!({}),
            member: "recordingConfigurations",
            reports: Some(recording.clone()),
        },
        Read {
            case: "one recording configuration",
            operation: "GetRecordingConfiguration",
            request: json!({ "arn": recording.clone() }),
            member: "recordingConfiguration",
            reports: Some(recording.clone()),
        },
        Read {
            case: "the ad configurations",
            operation: "ListAdConfigurations",
            request: json!({}),
            member: "adConfigurations",
            reports: Some(ad.clone()),
        },
        Read {
            case: "one ad configuration",
            operation: "GetAdConfiguration",
            request: json!({ "arn": ad.clone() }),
            member: "adConfiguration",
            reports: Some(ad.clone()),
        },
        Read {
            // Nothing imported a playback key pair, so this row reports no ARN: what it
            // asserts is that the modelled `@required` list is answered at all.
            case: "the playback key pairs",
            operation: "ListPlaybackKeyPairs",
            request: json!({}),
            member: "keyPairs",
            reports: None,
        },
    ];

    for read in reads {
        let Read {
            case,
            operation,
            request,
            member,
            reports,
        } = read;
        let answer = ivs.ok(operation, request).await;
        let reported = answer.get(member).cloned().unwrap_or(Value::Null);
        assert!(
            !reported.is_null(),
            "case {case:?}: {operation} must answer its {member} member, got {answer}"
        );
        if let Some(arn) = &reports {
            let carried = objects_carrying(&reported, "arn")
                .iter()
                .map(|object| text(object, "arn"))
                .any(|carried| &carried == arn);
            assert!(
                carried,
                "case {case:?}: {operation} must report {arn} under {member}, got {answer}"
            );
        }

        let carriers = objects_carrying(&answer, FAILOVER_SLATE);
        assert!(
            carriers.is_empty(),
            "case {case:?}: {operation} serves no channel object, so nothing in its answer \
             may carry {FAILOVER_SLATE}, got {carriers:?} in {answer}"
        );
    }
}
