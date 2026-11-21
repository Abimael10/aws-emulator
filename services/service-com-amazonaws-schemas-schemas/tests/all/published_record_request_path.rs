// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for HOW a published CloudTrail document says the call reached
//! the service: the members a record carries for the VPC endpoint a request arrived over
//! and for the account that owns that endpoint.
//!
//! RULE: the document a catalogue entry publishes declares its event's record WHOLE and
//! writes the declarations in name order — every member the record states is declared, at
//! the place that order puts it, so a client that generates a binding, feeds a validator,
//! or diffs and hashes the text sees the same declarations AWS publishes. A CloudTrail
//! record states where a call CAME FROM as well as who made it: a call that arrived over a
//! VPC endpoint is recorded with the endpoint it came in through and with that endpoint's
//! owning account, so those members are declared too, after the identity element, because
//! that is where their names fall. The draft-04 export of the same document carries them,
//! rendered in name order like every other schema it translates. A document published for
//! an event whose `detail` is NOT a CloudTrail record declares them nowhere: they are
//! members of that record, not of every event.
//!
//! Every case drives the real wire path, through the harness in
//! `registry_schema_lifecycle.rs` — nothing here touches the state struct directly.

use service_com_amazonaws_schemas_schemas::state::{
    aws_managed_event_types, JsonNode, ManagedEventType, AWS_MANAGED_REGISTRY,
    CLOUD_TRAIL_EVENT_FAMILY,
};

// The harness and the document readers every sibling suite of this target shares: the
// emulator, the two read URIs, and the order-keeping document helpers.
use crate::registry_schema_lifecycle::{
    at, export_uri, member_names, ordered_document, schema_uri, Emulator,
};

/// The members a published CloudTrail document declares for the endpoint a recorded call
/// arrived over, in the order the document writes them, each with the type it is declared
/// as.
///
/// Source: the record a CloudTrail-delivered event carries (CloudTrail User Guide,
/// "CloudTrail record contents") — the VPC endpoint the request was made through and the
/// account that owns that endpoint. The emulator's own event plumbing reads
/// `vpcEndpointId` off a record too (the shared CloudTrail record support in
/// `services/common`), so a document that did not declare it would not describe the events
/// the emulator delivers.
const PUBLISHED_REQUEST_PATH_MEMBERS: &[(&str, &str)] = &[
    ("vpcEndpointAccountId", "string"),
    ("vpcEndpointId", "string"),
];

/// The sources the cases run the rule over: the one the issue cited and three it did not.
/// The `detail` of a CloudTrail-delivered event is the same record whichever source
/// published it, so each row must declare the same members.
const PUBLISHING_SOURCES: &[&str] = &["aws.events", "aws.sqs", "aws.kms", "aws.redshift"];

/// Catalogue entries whose `detail` is NOT a CloudTrail record — the negative rows of the
/// rule. Source: `crate::state::aws_managed_event_types()`.
const NON_CLOUD_TRAIL_ENTRIES: &[&str] = &[
    "aws.ec2@EC2InstanceStateChangeNotification",
    "aws.s3@ObjectCreated",
    "aws.health@AWSHealthEvent",
    "aws.events@ScheduledEvent",
];

/// The catalogue entry published under `schema_name`.
fn entry(schema_name: &str) -> ManagedEventType {
    aws_managed_event_types()
        .iter()
        .find(|event| event.schema_name() == schema_name)
        .copied()
        .unwrap_or_else(|| panic!("case {schema_name:?}: the catalogue must publish it"))
}

/// The first `properties` object in `node` whose member names are NOT in name order —
/// compared without regard to case, which is the order the published documents write a
/// schema's properties in (`accountId` before `ARN`) — named by the path to it. `None`
/// when every declaration in the tree is in that order.
fn first_unordered_properties(path: &str, node: &JsonNode) -> Option<String> {
    match node {
        JsonNode::Object(members) => {
            for (name, member) in members {
                if name == "properties" {
                    let declared = member_names(member);
                    let mut ordered = declared.clone();
                    ordered.sort_by_key(|name| (name.to_lowercase(), name.clone()));
                    if declared != ordered {
                        return Some(format!("{path}.{name}: {declared:?}"));
                    }
                }
            }
            members
                .iter()
                .find_map(|(name, member)| {
                    first_unordered_properties(&format!("{path}.{name}"), member)
                })
        }
        JsonNode::List(items) => items.iter().enumerate().find_map(|(index, item)| {
            first_unordered_properties(&format!("{path}[{index}]"), item)
        }),
        _ => None,
    }
}

/// The record a published document declares for `event_name`, with its member order kept.
async fn described_record(emulator: &Emulator, case: &str, event_name: &str) -> JsonNode {
    let described = emulator
        .call("GET", &schema_uri(AWS_MANAGED_REGISTRY, case), None)
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected 200, got {} {:?}",
        described.status, described.body
    );
    let content = described.str_at("Content");
    let document = ordered_document(case, &content);
    at(
        case,
        &document,
        &["components", "schemas", event_name, "properties"],
    )
    .clone()
}

/// RULE (the describe path): a published CloudTrail document declares where a recorded
/// call came from — the VPC endpoint it arrived over and that endpoint's owning account —
/// each with the type the record carries, and declares them where the document's name
/// order puts them: after the identity element, as the record's last members.
#[tokio::test]
async fn a_published_record_declares_the_endpoint_a_call_arrived_over() {
    let emulator = Emulator::new();

    // Rows: every member of the CloudTrail-delivered family (which KIND of record the
    // event is) published by four different sources — the record is the same in each.
    for source in PUBLISHING_SOURCES {
        for (_, event_name) in CLOUD_TRAIL_EVENT_FAMILY {
            let case = format!("{source}@{event_name}");
            let record = described_record(&emulator, &case, event_name).await;
            let declared = member_names(&record);

            for (member, declared_type) in PUBLISHED_REQUEST_PATH_MEMBERS {
                let node = at(&case, &record, &[member]);
                assert_eq!(
                    node.member("type"),
                    Some(&JsonNode::text(declared_type)),
                    "case {case:?}: {member} is declared {declared_type:?}, got {node:?}"
                );
                assert_eq!(
                    member_names(node),
                    vec!["type"],
                    "case {case:?}: {member} is constrained by its type alone, got {node:?}"
                );
            }

            let expected_tail: Vec<String> = PUBLISHED_REQUEST_PATH_MEMBERS
                .iter()
                .map(|(member, _)| (*member).to_string())
                .collect();
            let tail: Vec<String> = declared
                .iter()
                .skip(declared.len().saturating_sub(expected_tail.len()))
                .cloned()
                .collect();
            assert_eq!(
                tail, expected_tail,
                "case {case:?}: the record's name order ends with the endpoint it came in \
                 through, got {declared:?}"
            );
            let identity_at = declared
                .iter()
                .position(|member| member == "userIdentity")
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the record declares userIdentity, got {declared:?}")
                });
            assert_eq!(
                identity_at,
                declared.len() - expected_tail.len() - 1,
                "case {case:?}: the endpoint members follow the identity, got {declared:?}"
            );
        }
    }
}

/// RULE (an invariant of the published text): a published CloudTrail document declares
/// every schema's properties in name order, at every depth — so the record, its nested
/// structures and the session block all read as the published text does, and a member
/// declared out of that order (or missing from it) is a divergence a client's diff sees.
#[tokio::test]
async fn a_published_cloud_trail_document_declares_every_schema_in_name_order() {
    let emulator = Emulator::new();

    for source in PUBLISHING_SOURCES {
        for (_, event_name) in CLOUD_TRAIL_EVENT_FAMILY {
            let case = format!("{source}@{event_name}");

            let described = emulator
                .call("GET", &schema_uri(AWS_MANAGED_REGISTRY, &case), None)
                .await;
            assert_eq!(
                described.status, 200,
                "case {case:?}: expected 200, got {} {:?}",
                described.status, described.body
            );
            let content = described.str_at("Content");
            let document = ordered_document(&case, &content);
            assert_eq!(
                first_unordered_properties("$", &document),
                None,
                "case {case:?}: every declaration is written in name order, got {content}"
            );

            let exported = emulator
                .call(
                    "GET",
                    &export_uri(AWS_MANAGED_REGISTRY, &case, "JSONSchemaDraft4"),
                    None,
                )
                .await;
            assert_eq!(
                exported.status, 200,
                "case {case:?}: expected 200 from the export, got {} {:?}",
                exported.status, exported.body
            );
            let export_content = exported.str_at("Content");
            let converted = ordered_document(&case, &export_content);
            assert_eq!(
                first_unordered_properties("$", &converted),
                None,
                "case {case:?}: the export writes every declaration in name order, \
                 got {export_content}"
            );
        }
    }
}

/// RULE (the export path): the draft-04 rendering of that document carries the endpoint
/// members the source document declares, each with its type, rendered in name order like
/// every other schema it translates.
#[tokio::test]
async fn the_draft4_export_carries_the_endpoint_a_call_arrived_over() {
    let emulator = Emulator::new();

    for source in PUBLISHING_SOURCES {
        for (_, event_name) in CLOUD_TRAIL_EVENT_FAMILY {
            let case = format!("{source}@{event_name}");
            let exported = emulator
                .call(
                    "GET",
                    &export_uri(AWS_MANAGED_REGISTRY, &case, "JSONSchemaDraft4"),
                    None,
                )
                .await;
            assert_eq!(
                exported.status, 200,
                "case {case:?}: expected 200 from the export, got {} {:?}",
                exported.status, exported.body
            );
            let content = exported.str_at("Content");
            let converted = ordered_document(&case, &content);
            let record = at(
                &case,
                &converted,
                &["definitions", event_name, "properties"],
            );
            let declared = member_names(record);
            for (member, declared_type) in PUBLISHED_REQUEST_PATH_MEMBERS {
                let node = at(&case, record, &[member]);
                assert_eq!(
                    node.member("type"),
                    Some(&JsonNode::text(declared_type)),
                    "case {case:?}: the export declares {member} {declared_type:?}, \
                     got {node:?}"
                );
            }
            let expected_tail: Vec<String> = PUBLISHED_REQUEST_PATH_MEMBERS
                .iter()
                .map(|(member, _)| (*member).to_string())
                .collect();
            let tail: Vec<String> = declared
                .iter()
                .skip(declared.len().saturating_sub(expected_tail.len()))
                .cloned()
                .collect();
            assert_eq!(
                tail, expected_tail,
                "case {case:?}: the export's name order ends with them too, got {declared:?}"
            );
        }
    }
}

/// RULE (the negative side): the endpoint members belong to the CloudTrail record, so a
/// catalogue entry whose `detail` is a service's OWN event declares none of them, on
/// either read path.
#[tokio::test]
async fn an_event_that_is_no_cloud_trail_record_declares_no_endpoint_members() {
    let emulator = Emulator::new();

    for schema_name in NON_CLOUD_TRAIL_ENTRIES {
        let event = entry(schema_name);
        let record = described_record(&emulator, schema_name, event.event_name).await;
        let declared = member_names(&record);
        for (member, _) in PUBLISHED_REQUEST_PATH_MEMBERS {
            assert!(
                !declared.iter().any(|name| name == member),
                "case {schema_name:?}: {member} is a CloudTrail record member, \
                 got {declared:?}"
            );
        }

        let exported = emulator
            .call(
                "GET",
                &export_uri(AWS_MANAGED_REGISTRY, schema_name, "JSONSchemaDraft4"),
                None,
            )
            .await;
        assert_eq!(
            exported.status, 200,
            "case {schema_name:?}: expected 200 from the export, got {} {:?}",
            exported.status, exported.body
        );
        let content = exported.str_at("Content");
        for (member, _) in PUBLISHED_REQUEST_PATH_MEMBERS {
            assert!(
                !content.contains(member),
                "case {schema_name:?}: the export declares no {member}, got {content}"
            );
        }
    }
}
