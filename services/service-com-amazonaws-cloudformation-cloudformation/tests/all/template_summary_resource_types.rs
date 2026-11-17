// SPDX-License-Identifier: CC0-1.0

//! What `GetTemplateSummary`'s `ResourceTypes` reports about a template's resources, end to
//! end over the awsQuery wire.
//!
//! THE RULE: "a list of all the template resource types that are defined in the template" is
//! the type of EACH resource the body declares — so a type several resources share is
//! reported once per resource and nothing is collapsed — reported ASCENDING BY TYPE NAME,
//! whatever order the body declares its resources in. The per-TYPE reading of the same
//! section is `ResourceIdentifierSummaries`, which carries one entry per type AN IMPORT CAN
//! IDENTIFY A RESOURCE OF, with the logical ids declared with it IN DECLARED ORDER and its own
//! identifier sets, the entries themselves DESCENDING by type name — so the two members are two
//! readings of one section and this suite asserts both of one response.
//!
//! The rows differ along what the rule generalises over: how many resources share a type,
//! whether the declared order agrees with the ascending one, how many types the body
//! declares, and the spelling of the body (JSON and YAML).

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{call, list_members, member, router, xml_blocks, xml_text, xml_texts};

/// `GetTemplateSummary` over a body the request carries.
async fn summarise(router: &MultiServiceRouter, template: &str) -> (u16, String) {
    call(
        router,
        "GetTemplateSummary",
        &[member("TemplateBody", template)],
    )
    .await
}

/// The `ResourceTypes` the response states, in wire order.
fn reported_types(xml: &str) -> Vec<String> {
    xml_blocks(xml, "ResourceTypes")
        .first()
        .map(|block| xml_texts(block, "member"))
        .unwrap_or_default()
}

/// Each `ResourceIdentifierSummary` as `(ResourceType, LogicalResourceIds)`, in wire order.
fn reported_summaries(xml: &str) -> Vec<(String, Vec<String>)> {
    list_members(xml, "ResourceIdentifierSummaries")
        .iter()
        .map(|entry| {
            let ids = xml_blocks(entry, "LogicalResourceIds")
                .first()
                .map(|block| xml_texts(block, "member"))
                .unwrap_or_default();
            (xml_text(entry, "ResourceType").unwrap_or_default(), ids)
        })
        .collect()
}

/// Each `ResourceIdentifierSummary` as `(ResourceType, ResourceIdentifiers)`, in wire order.
fn reported_identifiers(xml: &str) -> Vec<(String, Vec<String>)> {
    list_members(xml, "ResourceIdentifierSummaries")
        .iter()
        .map(|entry| {
            let identifiers = xml_blocks(entry, "ResourceIdentifiers")
                .first()
                .map(|block| xml_texts(block, "member"))
                .unwrap_or_default();
            (
                xml_text(entry, "ResourceType").unwrap_or_default(),
                identifiers,
            )
        })
        .collect()
}

/// One row: a body, the types its summary must report (ascending by type name), and the
/// identifier summaries beside them (one per importable type, descending by type name).
struct Row {
    case: &'static str,
    template: String,
    types: Vec<&'static str>,
    summaries: Vec<(&'static str, Vec<&'static str>)>,
}

fn rows() -> Vec<Row> {
    // The bodies are written out as TEXT rather than built with `json!`, which sorts an
    // object's members as it serialises them: what these rows turn on is the order the body
    // itself states, so each row states it.
    vec![
        Row {
            case: "two types declared against the ascending order",
            template: r#"{"Resources":{
                "Store":{"Type":"AWS::S3::Bucket"},
                "Access":{"Type":"AWS::IAM::Role"}}}"#
                .to_string(),
            types: vec!["AWS::IAM::Role", "AWS::S3::Bucket"],
            summaries: vec![
                ("AWS::S3::Bucket", vec!["Store"]),
                ("AWS::IAM::Role", vec!["Access"]),
            ],
        },
        Row {
            case: "one type declared by three resources",
            template: r#"{"Resources":{
                "Third":{"Type":"AWS::SNS::Topic"},
                "First":{"Type":"AWS::SNS::Topic"},
                "Second":{"Type":"AWS::SNS::Topic"}}}"#
                .to_string(),
            types: vec!["AWS::SNS::Topic", "AWS::SNS::Topic", "AWS::SNS::Topic"],
            summaries: vec![("AWS::SNS::Topic", vec!["Third", "First", "Second"])],
        },
        Row {
            case: "a body whose declared order already is the ascending one",
            template: r#"{"Resources":{
                "Access":{"Type":"AWS::IAM::Role"},
                "Store":{"Type":"AWS::S3::Bucket"}}}"#
                .to_string(),
            types: vec!["AWS::IAM::Role", "AWS::S3::Bucket"],
            summaries: vec![
                ("AWS::S3::Bucket", vec!["Store"]),
                ("AWS::IAM::Role", vec!["Access"]),
            ],
        },
        Row {
            case: "YAML, four resources of three types, two of them sharing one",
            template: "Resources:\n  \
                 Zebra:\n    Type: AWS::SQS::Queue\n  \
                 Alpha:\n    Type: AWS::SNS::Topic\n  \
                 Mid:\n    Type: AWS::SQS::Queue\n  \
                 Store:\n    Type: AWS::S3::Bucket\n"
                .to_string(),
            types: vec![
                "AWS::S3::Bucket",
                "AWS::SNS::Topic",
                "AWS::SQS::Queue",
                "AWS::SQS::Queue",
            ],
            summaries: vec![
                ("AWS::SQS::Queue", vec!["Zebra", "Mid"]),
                ("AWS::SNS::Topic", vec!["Alpha"]),
                ("AWS::S3::Bucket", vec!["Store"]),
            ],
        },
        Row {
            case: "a single resource, where the set and the declaration agree trivially",
            template: json!({"Resources": {"Only": {"Type": "AWS::SNS::Topic"}}}).to_string(),
            types: vec!["AWS::SNS::Topic"],
            summaries: vec![("AWS::SNS::Topic", vec!["Only"])],
        },
    ]
}

#[tokio::test]
async fn a_summary_reports_the_type_of_every_declared_resource_by_type_name() {
    let router = router();
    for row in rows() {
        let case = row.case;
        let (status, body) = summarise(&router, &row.template).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the body summarised, got {status} {body}"
        );

        let reported = reported_types(&body);
        assert_eq!(
            reported, row.types,
            "case {case:?}: expected one entry per declared resource, ascending by type name, \
             {:?}, got {reported:?}",
            row.types
        );

        // The same response's per-TYPE reading: the identifier summaries carry each type
        // once, which is what shows the repetition above is this member's own rather than
        // the reader's.
        let expected: Vec<(String, Vec<String>)> = row
            .summaries
            .iter()
            .map(|(resource_type, ids)| {
                (
                    (*resource_type).to_string(),
                    ids.iter().map(|id| (*id).to_string()).collect(),
                )
            })
            .collect();
        let summaries = reported_summaries(&body);
        assert_eq!(
            summaries, expected,
            "case {case:?}: expected the identifier summaries {expected:?}, got {summaries:?}"
        );
    }
}

/// Every ordering of one `Resources` section is summarised with the SAME `ResourceTypes`
/// list — ascending by type name — and with the same identifier summaries — descending by type
/// name — while the logical ids inside each summary follow the order that body states them in.
/// The invariant is asserted over every permutation of a four-resource section, which is the
/// dimension the rule generalises over: the ORDER the body declares its resources in.
#[tokio::test]
async fn the_types_reported_do_not_depend_on_the_order_the_body_declares_resources_in() {
    // Four declarations of three types, two of them sharing one type, each needing no
    // properties. Identifiers are each type's primary identifier, which the per-TYPE member
    // carries beside the logical ids.
    let declarations: [(&str, &str); 4] = [
        ("Store", "AWS::S3::Bucket"),
        ("Access", "AWS::IAM::Role"),
        ("Notify", "AWS::SNS::Topic"),
        ("Archive", "AWS::S3::Bucket"),
    ];
    // The one list every ordering reports: one entry per declared resource, ascending.
    let expected_types = vec![
        "AWS::IAM::Role".to_string(),
        "AWS::S3::Bucket".to_string(),
        "AWS::S3::Bucket".to_string(),
        "AWS::SNS::Topic".to_string(),
    ];
    let router = router();

    for order in permutations(declarations.len()) {
        let stated: Vec<(&str, &str)> = order.iter().map(|at| declarations[*at]).collect();
        let case = stated
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<&str>>()
            .join(",");
        // Written out as text: `json!` would state the members in its own order, and what
        // this case turns on is the order the BODY states them in.
        let body = stated
            .iter()
            .map(|(id, resource_type)| format!(r#""{id}":{{"Type":"{resource_type}"}}"#))
            .collect::<Vec<String>>()
            .join(",");
        let template = format!(r#"{{"Resources":{{{body}}}}}"#);

        let (status, response) = summarise(&router, &template).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the body summarised, got {status} {response}"
        );

        let reported = reported_types(&response);
        assert_eq!(
            reported, expected_types,
            "case {case:?}: expected {expected_types:?} whatever order the body states, got \
             {reported:?}"
        );

        // The per-TYPE member of the SAME response: one entry per type, descending by type
        // name whatever order the body states, each carrying the logical ids in declared
        // order.
        let mut expected_summaries: Vec<(String, Vec<String>)> = Vec::new();
        for (id, resource_type) in &stated {
            match expected_summaries
                .iter_mut()
                .find(|(reported_type, _)| reported_type == resource_type)
            {
                Some((_, ids)) => ids.push((*id).to_string()),
                None => {
                    expected_summaries.push(((*resource_type).to_string(), vec![(*id).to_string()]))
                }
            }
        }
        expected_summaries.sort_by(|(left, _), (right, _)| right.cmp(left));
        let summaries = reported_summaries(&response);
        assert_eq!(
            summaries, expected_summaries,
            "case {case:?}: expected the identifier summaries {expected_summaries:?}, got \
             {summaries:?}"
        );
    }
}

/// Every ordering of `count` items, as index lists.
fn permutations(count: usize) -> Vec<Vec<usize>> {
    if count == 0 {
        return vec![Vec::new()];
    }
    let mut orders = Vec::new();
    for at in 0..count {
        for mut rest in permutations(count - 1) {
            for index in &mut rest {
                if *index >= at {
                    *index += 1;
                }
            }
            let mut order = vec![at];
            order.extend(rest);
            orders.push(order);
        }
    }
    orders
}

/// A body declaring MANY resources of one type is summarised with as many `ResourceTypes`
/// entries as it declares resources: the list is as long as the `Resources` section, whatever
/// that length is. The type here is one an import can identify NO resource of, so the same
/// response carries no identifier summary at all — the two members answer different questions.
/// The rows differ along the COUNT, which is the dimension a client reads the list's length
/// for.
#[tokio::test]
async fn a_summary_reports_as_many_entries_as_the_body_declares_resources() {
    // `AWS::CloudFormation::WaitConditionHandle` is a type whose declaration needs no
    // properties, so a body of any length can be written out of it.
    const TYPE: &str = "AWS::CloudFormation::WaitConditionHandle";
    let router = router();

    for count in [1usize, 2, 7, 40] {
        let case = format!("{count} resources of one type");
        let declarations: Vec<String> = (0..count)
            .map(|index| format!(r#""Handle{index}":{{"Type":"{TYPE}"}}"#))
            .collect();
        let template = format!(r#"{{"Resources":{{{}}}}}"#, declarations.join(","));

        let (status, body) = summarise(&router, &template).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the body summarised, got {status} {body}"
        );

        let reported = reported_types(&body);
        assert_eq!(
            reported,
            vec![TYPE.to_string(); count],
            "case {case:?}: expected {count} entries of {TYPE:?}, got {reported:?}"
        );

        let summaries = reported_summaries(&body);
        assert!(
            summaries.is_empty(),
            "case {case:?}: expected no identifier summary for a type an import can identify \
             no resource of, got {summaries:?}"
        );
    }
}

/// RULE: one `ResourceIdentifierSummary` per declared type AN IMPORT CAN IDENTIFY A RESOURCE
/// OF, carrying one entry per identifier SET that type's own schema states — the properties of
/// a composite identifier in ONE entry, because a caller supplies them together, and one entry
/// per alternative a type publishes beside its primary identifier.
///
/// The rows differ along what the rule generalises over: the number of properties one identifier
/// set has, the number of sets a type publishes, whether the identifier is a name, an ARN or a
/// service-assigned id, and whether the type identifies a resource outside the stack at all.
/// Source for each expected row: the type's own resource provider schema (`primaryIdentifier`
/// and `additionalIdentifiers`), which is what the "Resources that support import operations"
/// table of the CloudFormation User Guide publishes per type.
/// One row of the identifier table: a body's declarations, and the entries the response must
/// carry for them.
struct IdentifierRow {
    case: &'static str,
    /// The `(logical id, resource type)` pairs the body declares, in that order.
    declarations: &'static [(&'static str, &'static str)],
    /// The `(ResourceType, ResourceIdentifiers)` entries the summary must report, descending
    /// by type name.
    expected: &'static [(&'static str, &'static [&'static str])],
}

#[tokio::test]
async fn a_summary_reports_the_identifier_sets_each_declared_type_states() {
    let rows: &[IdentifierRow] = &[
        IdentifierRow {
            case: "a composite identifier, whose parts are supplied together",
            declarations: &[("Tbl", "AWS::Glue::Table"), ("Db", "AWS::Glue::Database")],
            expected: &[
                ("AWS::Glue::Table", &["CatalogId,DatabaseName,Name"]),
                ("AWS::Glue::Database", &["DatabaseName"]),
            ],
        },
        IdentifierRow {
            case: "a type an import identifies by its ARN or by its name, and one by its ARN \
                   alone",
            declarations: &[
                ("Vec", "AWS::S3Vectors::VectorBucket"),
                ("Tab", "AWS::S3Tables::TableBucket"),
            ],
            expected: &[
                (
                    "AWS::S3Vectors::VectorBucket",
                    &["VectorBucketArn", "VectorBucketName"],
                ),
                ("AWS::S3Tables::TableBucket", &["TableBucketARN"]),
            ],
        },
        IdentifierRow {
            case: "a parameter, identified by the Name its own schema states",
            declarations: &[
                ("Param", "AWS::SSM::Parameter"),
                ("Log", "AWS::Logs::LogGroup"),
            ],
            expected: &[
                ("AWS::SSM::Parameter", &["Name"]),
                ("AWS::Logs::LogGroup", &["LogGroupName"]),
            ],
        },
        IdentifierRow {
            case: "a type identified by a service-assigned id, beside one identified by its \
                   name",
            declarations: &[("Topic", "AWS::SNS::Topic"), ("Store", "AWS::S3::Bucket")],
            expected: &[
                ("AWS::SNS::Topic", &["TopicArn"]),
                ("AWS::S3::Bucket", &["BucketName"]),
            ],
        },
        IdentifierRow {
            case: "a metadata record and a wait condition handle, which identify no resource \
                   at all",
            declarations: &[
                ("Meta", "AWS::CDK::Metadata"),
                ("Handle", "AWS::CloudFormation::WaitConditionHandle"),
                ("Param", "AWS::SSM::Parameter"),
            ],
            expected: &[("AWS::SSM::Parameter", &["Name"])],
        },
    ];
    let router = router();

    for IdentifierRow {
        case,
        declarations,
        expected,
    } in rows
    {
        // Written out as text: what the rows turn on is not the body's own order, but the
        // bodies state one anyway, so the reported order is the rule's and not the body's.
        let body = declarations
            .iter()
            .map(|(id, resource_type)| format!(r#""{id}":{{"Type":"{resource_type}"}}"#))
            .collect::<Vec<String>>()
            .join(",");
        let template = format!(r#"{{"Resources":{{{body}}}}}"#);

        let (status, response) = summarise(&router, &template).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the body summarised, got {status} {response}"
        );

        let expected_identifiers: Vec<(String, Vec<String>)> = expected
            .iter()
            .map(|(resource_type, identifiers)| {
                (
                    (*resource_type).to_string(),
                    identifiers.iter().map(|entry| (*entry).to_string()).collect(),
                )
            })
            .collect();
        let reported = reported_identifiers(&response);
        assert_eq!(
            reported, expected_identifiers,
            "case {case:?}: expected the identifier summaries {expected_identifiers:?}, got \
             {reported:?}"
        );
    }
}
