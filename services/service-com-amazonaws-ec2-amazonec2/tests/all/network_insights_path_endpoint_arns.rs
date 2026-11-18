// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the two ENDPOINT ARNS a Reachability Analyzer path reports —
//! `NetworkInsightsPath$SourceArn` and `$DestinationArn` — driven over the wire the service
//! actually speaks: `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: a stored path reports, for each end it has, the ARN of the resource
//! that end NAMES — the canonical `arn:aws:ec2:<region>:<account>:<type>/<id>` of the type
//! the id reads under when the end is named by an id of this account, and the ARN itself when
//! the end is named by one (a resource of another account or Region), with no ARN reported for
//! an end a path does not have. Every reader of the path answers the same two values, since
//! the store derives them once where the path is written.
//!
//! Each table row is a distinct input along what the rule generalises over: which kind of
//! resource each end names (eight of the nine the catalog carries, including kinds no recorded
//! call used), which id form it is named in (long, short, an ARN of a foreign account and
//! Region), and whether the path has a destination at all — so no one call is the
//! specification.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, NETWORK_INSIGHTS_ENDPOINT_TYPES,
    RESOURCE_TYPE_NETWORK_INSIGHTS_PATH,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::call;
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for, and therefore the Region an ARN this service
/// derives for an end named by an id names.
const REGION: &str = "us-west-2";

/// The largest `ResourceArn` the model publishes: `smithy_query(query="shape",
/// shape="ResourceArn")` is a string with `length 1..1283`, which `SourceArn` and
/// `DestinationArn` both target — so every ARN reported here has to fit inside it.
const MAX_RESOURCE_ARN_CHARS: usize = 1283;

/// One row of the table: the two ends a request names, and the ARN each must be reported
/// under.
#[derive(Debug, Clone)]
struct Row {
    /// What the row is about, named in every assertion it makes.
    case: &'static str,
    /// "The ID or ARN of the source" (`CreateNetworkInsightsPathRequest$Source`).
    source: &'static str,
    /// "The ID or ARN of the destination", where the row names one.
    destination: Option<&'static str>,
    /// The `ResourceType` the source's ARN names, or `None` when the row names the source by
    /// an ARN already — in which case that ARN is reported unchanged.
    source_type: Option<&'static str>,
    /// The same for the destination.
    destination_type: Option<&'static str>,
}

/// The ARN of a resource of THIS account in the Region every call here is signed for:
/// `arn:aws:ec2:<region>:<account>:<type>/<id>`, the form the AWS General Reference gives an
/// EC2 resource ARN in.
fn own_arn(resource_type: &str, id: &str) -> String {
    format!(
        "arn:aws:ec2:{REGION}:{account}:{resource_type}/{id}",
        account = engine::emulator_account_id(),
    )
}

/// The ARN the source of `row` must be reported under: the row's own value when it names one,
/// otherwise this account's ARN for the type the id reads under.
fn expected_source_arn(row: &Row) -> String {
    match row.source_type {
        None => row.source.to_string(),
        Some(resource_type) => own_arn(resource_type, row.source),
    }
}

/// The ARN the destination of `row` must be reported under, or `None` for a path with no
/// destination — there being no resource to name an ARN of.
fn expected_destination_arn(row: &Row) -> Option<String> {
    let destination = row.destination?;
    Some(match row.destination_type {
        None => destination.to_string(),
        Some(resource_type) => own_arn(resource_type, destination),
    })
}

/// The table the rule is asserted over: one row per endpoint form, so no row is the
/// specification.
///
/// The resource types are the catalog this crate screens an end against and derives its ARN
/// from ([`NETWORK_INSIGHTS_ENDPOINT_TYPES`], asserted below to be the one this table reads);
/// the rows cover the instance, interface, internet-gateway, VPN-gateway, VPC-endpoint,
/// endpoint-service, peering-connection, transit-gateway and attachment kinds, in the long and
/// the short id form EC2 has issued ids in, and as an ARN of another account and Region.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an instance to an internet gateway",
            source: "i-0123456789abcdef0",
            destination: Some("igw-0123456789abcdef0"),
            source_type: Some("instance"),
            destination_type: Some("internet-gateway"),
        },
        Row {
            case: "an interface pair, the kind the recorded call used",
            source: "eni-0123456789abcdef0",
            destination: Some("eni-0fedcba9876543210"),
            source_type: Some("network-interface"),
            destination_type: Some("network-interface"),
        },
        Row {
            case: "a short-form interface id, the other form EC2 has issued ids in",
            source: "eni-01234567",
            destination: Some("i-0fedcba9876543210"),
            source_type: Some("network-interface"),
            destination_type: Some("instance"),
        },
        Row {
            case: "a VPN gateway to a VPC endpoint",
            source: "vgw-0123456789abcdef0",
            destination: Some("vpce-0123456789abcdef0"),
            source_type: Some("vpn-gateway"),
            destination_type: Some("vpc-endpoint"),
        },
        Row {
            case: "an endpoint service to a peering connection",
            source: "vpce-svc-0123456789abcdef0",
            destination: Some("pcx-0123456789abcdef0"),
            source_type: Some("vpc-endpoint-service"),
            destination_type: Some("vpc-peering-connection"),
        },
        Row {
            case: "a transit gateway to one of its attachments",
            source: "tgw-0123456789abcdef0",
            destination: Some("tgw-attach-0123456789abcdef0"),
            source_type: Some("transit-gateway"),
            destination_type: Some("transit-gateway-attachment"),
        },
        Row {
            case: "a cross-account ARN as the source, which is reported unchanged",
            source: "arn:aws:ec2:eu-west-1:210987654321:network-interface/eni-0123456789abcdef0",
            destination: Some("i-0123456789abcdef0"),
            source_type: None,
            destination_type: Some("instance"),
        },
        Row {
            case: "a cross-account ARN at both ends",
            source: "arn:aws:ec2:eu-west-1:210987654321:instance/i-0123456789abcdef0",
            destination: Some(
                "arn:aws:ec2:ap-southeast-2:210987654321:transit-gateway/tgw-0123456789abcdef0",
            ),
            source_type: None,
            destination_type: None,
        },
        Row {
            case: "a source alone, so the path has no destination to name an ARN of",
            source: "i-0123456789abcdef0",
            destination: None,
            source_type: Some("instance"),
            destination_type: None,
        },
    ]
}

/// The form fields of one row's create, with `client_token` as its `ClientToken`.
fn create_params(row: &Row, client_token: &str) -> Vec<Param> {
    let mut params = vec![
        param("ClientToken", client_token),
        param("Source", row.source),
        param("Protocol", "tcp"),
    ];
    if let Some(destination) = row.destination {
        params.push(param("Destination", destination));
    }
    params
}

/// The two ARNs one reported path carries, beside the id it is keyed by.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedArns {
    path_id: String,
    source_arn: Option<String>,
    destination_arn: Option<String>,
}

/// Every path a response reports, in document order: the elements carrying a
/// `networkInsightsPathId` child, which is `<networkInsightsPath>` in a create's answer and
/// each `<item>` of a describe's `<networkInsightsPathSet>`.
fn reported(case: &str, body: &str) -> Vec<ReportedArns> {
    let doc = document(case, body);
    doc.descendants()
        .filter(|node| node.is_element() && child_text(*node, "networkInsightsPathId").is_some())
        .map(|node| ReportedArns {
            path_id: child_text(node, "networkInsightsPathId").unwrap_or_default(),
            source_arn: child_text(node, "sourceArn"),
            destination_arn: child_text(node, "destinationArn"),
        })
        .collect()
}

/// The one path a body reports.
fn only(case: &str, body: &str) -> ReportedArns {
    let paths = reported(case, body);
    assert_eq!(
        paths.len(),
        1,
        "case {case:?}: expected exactly one path in the answer, got {paths:?} from {body}"
    );
    paths.into_iter().next().expect("one path")
}

/// One call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The create answers both endpoint ARNs, and the describe of that path answers the same two
/// — the store derives them once, so no reader of a path can report one end differently.
#[tokio::test]
async fn a_created_path_reports_the_arn_of_each_end_it_names() {
    let (router, _engine) = super::ec2_query::emulator();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let token = format!("endpoint-arn-token-{index}");
        let body = ok(
            case,
            &router,
            "CreateNetworkInsightsPath",
            &create_params(row, &token),
        )
        .await;
        let created = only(case, &body);
        let expected_source = expected_source_arn(row);
        let expected_destination = expected_destination_arn(row);

        assert_eq!(
            created.source_arn.as_deref(),
            Some(expected_source.as_str()),
            "case {case:?}: expected sourceArn {expected_source:?}, got {created:?}"
        );
        assert_eq!(
            created.destination_arn, expected_destination,
            "case {case:?}: expected destinationArn {expected_destination:?}, got {created:?}"
        );
        for arn in [created.source_arn.as_deref(), created.destination_arn.as_deref()]
            .into_iter()
            .flatten()
        {
            assert!(
                (1..=MAX_RESOURCE_ARN_CHARS).contains(&arn.chars().count()),
                "case {case:?}: an endpoint ARN must fit the modelled ResourceArn length of \
                 1..{MAX_RESOURCE_ARN_CHARS}, got {} characters in {arn:?}",
                arn.chars().count()
            );
        }

        // The same path, read back through the operation that describes one: the two ARNs are
        // the stored path's, not something the create answered and forgot.
        let body = ok(
            case,
            &router,
            "DescribeNetworkInsightsPaths",
            &[param("NetworkInsightsPathId.1", &created.path_id)],
        )
        .await;
        let described = only(case, &body);
        assert_eq!(
            described, created,
            "case {case:?}: the describe must report the path the create answered, got \
             {described:?}"
        );
    }
}

/// Every path the account holds carries its two ARNs when the whole set is reported, not only
/// a path asked for by id — the projection reports what the store holds.
#[tokio::test]
async fn every_path_of_the_account_carries_its_endpoint_arns() {
    let case = "the whole set of paths";
    let (router, engine) = super::ec2_query::emulator();
    let rows = rows();

    let mut expected: Vec<ReportedArns> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let token = format!("every-path-token-{index}");
        let body = ok(
            row.case,
            &router,
            "CreateNetworkInsightsPath",
            &create_params(row, &token),
        )
        .await;
        let created = only(row.case, &body);
        expected.push(ReportedArns {
            path_id: created.path_id,
            source_arn: Some(expected_source_arn(row)),
            destination_arn: expected_destination_arn(row),
        });
    }
    assert_eq!(
        engine
            .state::<Ec2State>()
            .lock()
            .network_insights_paths
            .len(),
        rows.len(),
        "case {case:?}: every row must have stored its own path"
    );

    let body = ok(case, &router, "DescribeNetworkInsightsPaths", &[]).await;
    let mut described = reported(case, &body);
    described.sort_by(|left, right| left.path_id.cmp(&right.path_id));
    expected.sort_by(|left, right| left.path_id.cmp(&right.path_id));
    assert_eq!(
        described, expected,
        "case {case:?}: expected every stored path with both of its endpoint ARNs, got \
         {described:?}"
    );
}

/// The types the table names are the ones the crate's catalog carries, so a row cannot assert
/// an ARN under a type the service would not screen an end against.
#[test]
fn the_table_names_the_types_the_catalog_carries() {
    let case = "the endpoint type catalog";
    for row in rows() {
        for named in [row.source_type, row.destination_type].into_iter().flatten() {
            assert!(
                NETWORK_INSIGHTS_ENDPOINT_TYPES
                    .iter()
                    .any(|(resource_type, _)| *resource_type == named),
                "case {case:?}: {named:?} must be one of {NETWORK_INSIGHTS_ENDPOINT_TYPES:?}, \
                 named by row {:?}",
                row.case
            );
        }
    }
    // The path's own type is NOT an endpoint type: a path is not a resource a path can be
    // analyzed between.
    assert!(
        !NETWORK_INSIGHTS_ENDPOINT_TYPES
            .iter()
            .any(|(resource_type, _)| *resource_type == RESOURCE_TYPE_NETWORK_INSIGHTS_PATH),
        "case {case:?}: the catalog must not carry {RESOURCE_TYPE_NETWORK_INSIGHTS_PATH:?}"
    );
}
