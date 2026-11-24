// SPDX-License-Identifier: CC0-1.0

//! HOW the flow-log surface REFUSES: which error class each kind of faulty member is
//! answered under, and the sentence that goes with it — driven over the wire the service
//! speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! Four rules are under test:
//!
//! * A `LogDestination` is read AGAINST the destination type the request asked for: "The
//!   meaning of this parameter depends on the destination type. If the destination type is
//!   `cloud-watch-logs`, specify the ARN of a CloudWatch Logs log group … if the destination
//!   type is `s3`, specify the ARN of an S3 bucket … if the destination type is
//!   `kinesis-data-firehose`, specify the ARN of a Kinesis Data Firehose delivery stream"
//!   (`CreateFlowLogsRequest$LogDestination`). A destination that is not of that type is
//!   refused HTTP 400 `InvalidParameter` with `Invalid <destination type> destination:
//!   <destination>`, and nothing is stored. The rows differ in the destination type, in
//!   which other service's ARN was given for it, and in whether the ARN named a resource of
//!   its own service that the type cannot publish to.
//! * A member that destination type does not take AT ALL — a log group name for anything but
//!   CloudWatch Logs, destination options for anything but Amazon S3 — is refused the same
//!   way, with `<Member> is not supported when LogDestinationType is <destination type>`. The
//!   rows differ in the member, the destination type, and whether the type was asked for or
//!   defaulted.
//! * An unrecognised `Filter` NAME on `DescribeFlowLogs` is refused by the
//!   request-parameter layer: HTTP 400 `InvalidParameterValue` with `1 validation error
//!   detected: Value '<name>' at 'filter.<position>.member.name' has an invalid format`,
//!   where the position counts from 1 in the list the request sent. The rows differ in the
//!   position of the faulty entry, in how many entries precede it, and in how the name
//!   differs from a documented one.
//! * A `FlowLogId` outside the FORM EC2 issues flow log ids in is refused by that same layer
//!   — HTTP 400 `InvalidParameterValue` with the clause naming the whole `flowLogId`
//!   parameter — BEFORE any id is looked up, where a well-formed id the account holds no
//!   flow log for is a lookup miss (`InvalidFlowLogId.NotFound`). The rows differ in how the
//!   value leaves the id form and in where in the batch it sits, and the two classes are
//!   asserted against each other.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, FLOW_LOG_ID_PREFIX};

use super::ec2_query::{element, elements, error_code, error_message, filters, indexed};

/// The Region every call here is signed for — the flow log store holds the account's flow
/// logs, and the seeded default VPC is the one of the emulator's default Region.
const REGION: &str = "us-east-1";

/// The IAM role a CloudWatch Logs flow log publishes under: "This parameter is required if
/// the destination type is cloud-watch-logs"
/// (`CreateFlowLogsRequest$DeliverLogsPermissionArn`).
const DELIVER_LOGS_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/flow-logs-delivery";

/// The ARN forms `CreateFlowLogsRequest$LogDestination` documents, one per destination type,
/// written out here rather than read from the crate so a reader that stopped accepting a
/// documented form still fails: `arn:aws:logs:region:account_id:log-group:my_group`,
/// `arn:aws:s3:::my_bucket/my_subfolder/` and
/// `arn:aws:firehose:region:account_id:deliverystream:my_stream`.
const LOG_GROUP_ARN: &str = "arn:aws:logs:us-east-1:000000000000:log-group:/aws/vpc/flow-logs";
/// An Amazon S3 destination, bucket and subfolder, as that member documents one.
const BUCKET_ARN: &str = "arn:aws:s3:::flow-log-refusals-bucket/vpc/";
/// A Kinesis Data Firehose destination, as that member documents one.
const STREAM_ARN: &str = "arn:aws:firehose:us-east-1:000000000000:deliverystream:flow-logs";

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// A VPC created over the wire, by id — so a create can name a VPC the account really has.
async fn create_vpc(router: &MultiServiceRouter, cidr_block: &str) -> String {
    let params = [param("CidrBlock", cidr_block)];
    let (status, body) = super::ec2_query::call(router, REGION, "CreateVpc", &params).await;
    assert_eq!(
        status, 200,
        "CreateVpc {cidr_block}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "vpcId").unwrap_or_else(|| panic!("CreateVpc answered no vpcId: {body}"))
}

/// A `CreateFlowLogs` on `vpc_id` publishing to `log_destination` under the destination type
/// `log_destination_type` (none at all when it is `None`, so the member's documented default
/// applies), as its HTTP status and XML body.
async fn create_to_destination(
    router: &MultiServiceRouter,
    vpc_id: &str,
    log_destination_type: Option<&str>,
    log_destination: &str,
) -> (u16, String) {
    let mut params = vec![
        param("ResourceType", "VPC"),
        param("ResourceId.1", vpc_id),
        param("TrafficType", "ALL"),
        param("LogDestination", log_destination),
    ];
    // The role a CloudWatch Logs destination requires, and the ONE destination type that takes
    // it without the cross-account role beside it: "this parameter is required if the
    // destination type is cloud-watch-logs" (`CreateFlowLogsRequest$DeliverLogsPermissionArn`),
    // while under the other types it is half of a cross-account delivery pair.
    if log_destination_type.unwrap_or("cloud-watch-logs") == "cloud-watch-logs" {
        params.push(param("DeliverLogsPermissionArn", DELIVER_LOGS_ROLE_ARN));
    }
    if let Some(log_destination_type) = log_destination_type {
        params.push(param("LogDestinationType", log_destination_type));
    }
    super::ec2_query::call(router, REGION, "CreateFlowLogs", &params).await
}

// ---------------------------------------------------------------------------
// RULE 1: a destination is read against the type the request asked for
// ---------------------------------------------------------------------------

/// One row of the member-vs-type table: the case, the destination type the request names (or
/// none, for the member's documented default), the destination of that type, the members the
/// request adds, and the member the refusal must name.
type UnsupportedMemberRow<'row> = (
    &'row str,
    Option<&'row str>,
    &'row str,
    Vec<(String, String)>,
    &'row str,
);

/// One row of the served-members table: the case, and the members the create carries beside
/// the resource it monitors.
type ServedMembersRow<'row> = (&'row str, Vec<(String, String)>);

/// A destination that is not an ARN of the destination its type publishes to is refused
/// `InvalidParameter`, with the sentence naming the TYPE and the destination the request
/// sent — and the account is left with no flow log at all.
#[tokio::test]
async fn a_destination_of_another_kind_than_its_type_is_refused_naming_the_type() {
    let (router, engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(&router, "10.240.0.0/16").await;

    // Each row: the destination type the request names (or none, for the documented
    // default), and the destination it gives under it.
    let cases: [(&str, Option<&str>, &str); 10] = [
        ("a bucket for cloud-watch-logs", Some("cloud-watch-logs"), BUCKET_ARN),
        ("a delivery stream for cloud-watch-logs", Some("cloud-watch-logs"), STREAM_ARN),
        // The default destination type, which is cloud-watch-logs: the same refusal, with
        // the type the default supplied named in the sentence.
        ("a bucket for the default destination type", None, BUCKET_ARN),
        // An ARN of CloudWatch Logs that names no log group: the type takes exactly a log
        // group, so this is a destination of the right service and the wrong kind.
        (
            "a CloudWatch Logs ARN naming no log group",
            Some("cloud-watch-logs"),
            "arn:aws:logs:us-east-1:000000000000:destination:flow-logs",
        ),
        ("a log group for s3", Some("s3"), LOG_GROUP_ARN),
        ("a delivery stream for s3", Some("s3"), STREAM_ARN),
        // An Amazon S3 ARN naming a bucket Amazon S3 could not hold: a grant to nothing.
        ("a bucket name Amazon S3 could not hold", Some("s3"), "arn:aws:s3:::Not_A_Bucket"),
        ("a log group for kinesis-data-firehose", Some("kinesis-data-firehose"), LOG_GROUP_ARN),
        ("a bucket for kinesis-data-firehose", Some("kinesis-data-firehose"), BUCKET_ARN),
        // Not an ARN at all, under the type whose other spelling is a bare name.
        ("a bare name for cloud-watch-logs", Some("cloud-watch-logs"), "/aws/vpc/flow-logs"),
    ];

    for (case, log_destination_type, log_destination) in cases {
        let (status, body) =
            create_to_destination(&router, &vpc_id, log_destination_type, log_destination).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameter",
            "case {case:?}: expected InvalidParameter, got {body}"
        );
        // The type the sentence names is the one in force: the request's, or the documented
        // default for a request that named none.
        let named_type = log_destination_type.unwrap_or("cloud-watch-logs");
        let expected = format!("Invalid {named_type} destination: {log_destination}");
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the message {expected:?}, got {body}"
        );
    }

    assert!(
        engine.state::<Ec2State>().lock().flow_logs.is_empty(),
        "a refused create must store nothing, but the store holds flow logs"
    );
}

/// The other side of the same rule: each destination type SERVES the ARN its own
/// documentation gives for it, so the refusal above is about the mismatch and not about the
/// form of an ARN.
#[tokio::test]
async fn each_destination_type_serves_the_arn_its_documentation_gives_it() {
    let (router, _engine) = super::ec2_query::emulator();

    // Each row: the destination type, and the destination of that type.
    let cases: [(&str, Option<&str>, &str); 5] = [
        ("cloud-watch-logs by log group ARN", Some("cloud-watch-logs"), LOG_GROUP_ARN),
        ("the default destination type by log group ARN", None, LOG_GROUP_ARN),
        ("s3 by bucket ARN with a subfolder", Some("s3"), BUCKET_ARN),
        ("s3 by bucket ARN alone", Some("s3"), "arn:aws:s3:::flow-log-refusals-plain"),
        ("kinesis-data-firehose by delivery stream ARN", Some("kinesis-data-firehose"), STREAM_ARN),
    ];

    for (index, (case, log_destination_type, log_destination)) in cases.into_iter().enumerate() {
        // One VPC per row: one resource carries one flow log of the same configuration and
        // destination, so each row monitors a resource of its own.
        let vpc_id = create_vpc(&router, &format!("10.{}.0.0/16", 241 + index)).await;
        let (status, body) =
            create_to_destination(&router, &vpc_id, log_destination_type, log_destination).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let ids = elements(&body, "item");
        assert_eq!(
            ids.len(),
            1,
            "case {case:?}: expected exactly one flow log id, got {ids:?} in {body}"
        );
        assert!(
            ids[0].starts_with(&format!("{FLOW_LOG_ID_PREFIX}-")),
            "case {case:?}: expected a {FLOW_LOG_ID_PREFIX}- prefixed id, got {ids:?}"
        );
    }
}

/// The same rule for a member the destination type does not take AT ALL: `LogGroupName` is
/// "valid only if the destination type is cloud-watch-logs"
/// (`CreateFlowLogsRequest$LogGroupName`) and the destination options describe the objects an
/// Amazon S3 delivery writes (`DestinationOptionsRequest`), so every other destination type is
/// refused them — HTTP 400 `InvalidParameter`, naming the member and the destination type in
/// force — and nothing is stored. The rows differ in the member, in the destination type, and
/// in whether the type came from the request or from the member's documented default.
#[tokio::test]
async fn a_member_the_destination_type_does_not_take_is_refused_naming_both() {
    let (router, engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(&router, "10.248.0.0/16").await;

    // Each row: the destination type the request names (or none, for the documented default),
    // the destination of that type, the members it adds, and the member the refusal must name.
    let cases: [UnsupportedMemberRow; 6] = [
        (
            "a log group name for an Amazon S3 destination",
            Some("s3"),
            BUCKET_ARN,
            vec![param("LogGroupName", "/aws/vpc/flow-logs")],
            "LogGroupName",
        ),
        (
            "a log group name for a Firehose destination",
            Some("kinesis-data-firehose"),
            STREAM_ARN,
            vec![param("LogGroupName", "/aws/vpc/flow-logs")],
            "LogGroupName",
        ),
        (
            "destination options for a CloudWatch Logs destination",
            Some("cloud-watch-logs"),
            LOG_GROUP_ARN,
            vec![param("DestinationOptions.FileFormat", "plain-text")],
            "DestinationOptions",
        ),
        (
            "destination options for the default destination type",
            None,
            LOG_GROUP_ARN,
            vec![param("DestinationOptions.HiveCompatiblePartitions", "true")],
            "DestinationOptions",
        ),
        (
            "destination options for a Firehose destination",
            Some("kinesis-data-firehose"),
            STREAM_ARN,
            vec![param("DestinationOptions.PerHourPartition", "true")],
            "DestinationOptions",
        ),
        (
            "every destination option at once, for a CloudWatch Logs destination",
            Some("cloud-watch-logs"),
            LOG_GROUP_ARN,
            vec![
                param("DestinationOptions.FileFormat", "parquet"),
                param("DestinationOptions.HiveCompatiblePartitions", "true"),
                param("DestinationOptions.PerHourPartition", "true"),
            ],
            "DestinationOptions",
        ),
    ];

    for (case, log_destination_type, log_destination, extra, member) in cases {
        let mut params = vec![
            param("ResourceType", "VPC"),
            param("ResourceId.1", &vpc_id),
            param("TrafficType", "ALL"),
            param("LogDestination", log_destination),
            param("DeliverLogsPermissionArn", DELIVER_LOGS_ROLE_ARN),
        ];
        if let Some(log_destination_type) = log_destination_type {
            params.push(param("LogDestinationType", log_destination_type));
        }
        params.extend(extra);

        let (status, body) = super::ec2_query::call(&router, REGION, "CreateFlowLogs", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameter",
            "case {case:?}: expected InvalidParameter, got {body}"
        );
        let named_type = log_destination_type.unwrap_or("cloud-watch-logs");
        let expected =
            format!("{member} is not supported when LogDestinationType is {named_type}");
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the message {expected:?}, got {body}"
        );
    }

    assert!(
        engine.state::<Ec2State>().lock().flow_logs.is_empty(),
        "a refused create must store nothing, but the store holds flow logs"
    );
}

/// The other side of that rule: each member is SERVED by the destination type that takes it —
/// a log group name for CloudWatch Logs, destination options for Amazon S3.
#[tokio::test]
async fn each_member_is_served_by_the_destination_type_that_takes_it() {
    let (router, _engine) = super::ec2_query::emulator();

    // Each row: the case, and the members the create carries beside the resource.
    let cases: [ServedMembersRow; 2] = [
        (
            "a log group name for CloudWatch Logs",
            vec![
                param("LogDestinationType", "cloud-watch-logs"),
                param("LogGroupName", "/aws/vpc/served-group"),
                param("DeliverLogsPermissionArn", DELIVER_LOGS_ROLE_ARN),
            ],
        ),
        (
            "destination options for Amazon S3",
            vec![
                param("LogDestinationType", "s3"),
                param("LogDestination", BUCKET_ARN),
                param("DestinationOptions.FileFormat", "parquet"),
                param("DestinationOptions.HiveCompatiblePartitions", "true"),
            ],
        ),
    ];

    for (index, (case, members)) in cases.into_iter().enumerate() {
        // One VPC per row: one resource carries one flow log of the same configuration.
        let vpc_id = create_vpc(&router, &format!("10.{}.0.0/16", 246 + index)).await;
        let mut params = vec![
            param("ResourceType", "VPC"),
            param("ResourceId.1", &vpc_id),
            param("TrafficType", "ALL"),
        ];
        params.extend(members);
        let (status, body) = super::ec2_query::call(&router, REGION, "CreateFlowLogs", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            elements(&body, "item").len(),
            1,
            "case {case:?}: expected exactly one flow log id, got {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 2: an unrecognised filter name names the entry at fault
// ---------------------------------------------------------------------------

/// One row of the filter-position table: the case, the filter entries the request sends
/// (each a name and its values, as [`super::ec2_query::filters`] takes them), the 1-based
/// position of the entry at fault, and the name that entry carries.
type FilterPositionRow<'row> = (&'row str, Vec<(&'row str, &'row [&'row str])>, usize, &'row str);

/// A `Filter` entry whose name this operation does not document is refused
/// `InvalidParameterValue`, with the parameter layer's clause naming the entry's POSITION in
/// the request.
#[tokio::test]
async fn an_unrecognised_filter_name_is_refused_naming_its_position() {
    let (router, _engine) = super::ec2_query::emulator();

    // The rows differ in where the faulty entry sits and in how its name misses.
    let cases: [FilterPositionRow; 6] = [
        ("the only entry", vec![("resource-type", &["VPC"][..])], 1, "resource-type"),
        (
            "the second of two entries",
            vec![("traffic-type", &["ALL"][..]), ("destination-type", &["s3"][..])],
            2,
            "destination-type",
        ),
        (
            "the third of three entries",
            vec![
                ("traffic-type", &["ALL"][..]),
                ("resource-id", &["vpc-00000000000000001"][..]),
                ("log-destination", &["arn:aws:s3:::b"][..]),
            ],
            3,
            "log-destination",
        ),
        (
            "the first of two, the second documented",
            vec![("flow-log-status", &["ACTIVE"][..]), ("traffic-type", &["ALL"][..])],
            1,
            "flow-log-status",
        ),
        // A documented name in another case: "Filter names are case-sensitive"
        // (`Filter$Name`), so this names no filter.
        ("a documented name in upper case", vec![("Traffic-Type", &["ALL"][..])], 1, "Traffic-Type"),
        // A tag filter with no key after the prefix selects on no tag key at all.
        ("a tag filter with no key", vec![("tag:", &["anything"][..])], 1, "tag:"),
    ];

    for (case, entries, position, name) in cases {
        let params = filters(&entries);
        let (status, body) =
            super::ec2_query::call(&router, REGION, "DescribeFlowLogs", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {case:?}: expected InvalidParameterValue, got {body}"
        );
        let expected = format!(
            "1 validation error detected: Value '{name}' at 'filter.{position}.member.name' \
             has an invalid format"
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the message {expected:?}, got {body}"
        );
    }
}

/// The other side of that rule: every name the member documents is SERVED, so the refusal
/// above is about the name and not about filtering itself.
#[tokio::test]
async fn every_documented_filter_name_is_served() {
    let (router, _engine) = super::ec2_query::emulator();

    // The names `DescribeFlowLogsRequest$Filter` documents, written out here rather than
    // read from the crate so a reader that dropped one still fails
    // (`smithy_query(query="doc",shape="DescribeFlowLogsRequest",member="Filter")`): the
    // six attribute filters, the tag family and `tag-key`.
    let documented: [(&str, &[&str]); 8] = [
        ("deliver-log-status", &["SUCCESS"][..]),
        ("log-destination-type", &["s3"][..]),
        ("flow-log-id", &["fl-0123456789abcdef0"][..]),
        ("log-group-name", &["/aws/vpc/flow-logs"][..]),
        ("resource-id", &["vpc-00000000000000001"][..]),
        ("traffic-type", &["ALL"][..]),
        ("tag:Name", &["anything"][..]),
        ("tag-key", &["Name"][..]),
    ];

    for (name, values) in documented {
        let params = filters(&[(name, values)]);
        let (status, body) =
            super::ec2_query::call(&router, REGION, "DescribeFlowLogs", &params).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200, got {status} with body {body}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 3: an unreadable id is refused before any lookup
// ---------------------------------------------------------------------------

/// A `DeleteFlowLogs` naming a value outside the flow log id form is refused
/// `InvalidParameterValue` with the parameter layer's clause over the WHOLE id list, before
/// any id is resolved — so nothing is deleted and the refusal does not depend on what the
/// account holds.
#[tokio::test]
async fn a_value_outside_the_id_form_is_refused_as_an_unreadable_parameter() {
    let (router, engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(&router, "10.252.0.0/16").await;
    let stored = {
        let params = vec![
            param("ResourceType", "VPC"),
            param("ResourceId.1", &vpc_id),
            param("TrafficType", "ALL"),
            param("LogGroupName", "/aws/vpc/id-forms"),
            param("DeliverLogsPermissionArn", DELIVER_LOGS_ROLE_ARN),
        ];
        let (status, body) = super::ec2_query::call(&router, REGION, "CreateFlowLogs", &params).await;
        assert_eq!(
            status, 200,
            "seeding one flow log: expected HTTP 200, got {status} with body {body}"
        );
        elements(&body, "item")
            .pop()
            .unwrap_or_else(|| panic!("CreateFlowLogs answered no id: {body}"))
    };

    // Each row: the ids the request names. Every row leaves the id form in a different way
    // — a suffix that is not hexadecimal, a length between the two issued forms, an empty
    // value, another kind's prefix, no prefix at all, upper-case digits — and the rows place
    // the faulty value alone, after a stored id, and before one.
    let cases: [(&str, Vec<&str>); 8] = [
        ("a word for a suffix", vec!["fl-bogus"]),
        ("a suffix of the wrong length", vec!["fl-0123456789abcde"]),
        ("upper-case hexadecimal digits", vec!["fl-0123456789ABCDEF0"]),
        ("the prefix alone", vec!["fl-"]),
        ("no prefix at all", vec!["0123456789abcdef0"]),
        ("another resource kind's id", vec!["vpc-0123456789abcdef0"]),
        ("an empty value", vec![""]),
        ("a stored id, then an unreadable one", vec![stored.as_str(), "fl-bogus"]),
    ];

    for (case, named) in cases {
        let params = indexed("FlowLogId", &named);
        let (status, body) =
            super::ec2_query::call(&router, REGION, "DeleteFlowLogs", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValue",
            "case {case:?}: expected InvalidParameterValue, got {body}"
        );
        let expected = format!(
            "1 validation error detected: Value '[{}]' at 'flowLogId' has an invalid format",
            named.join(", "),
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the message {expected:?}, got {body}"
        );
    }

    // Nothing was deleted: the seeded flow log is still there.
    assert_eq!(
        engine
            .state::<Ec2State>()
            .lock()
            .flow_logs
            .keys()
            .cloned()
            .collect::<Vec<String>>(),
        vec![stored.clone()],
        "a refused delete must remove nothing"
    );
}

/// The boundary between the two classes: a WELL-FORMED id the account holds no flow log for
/// is a lookup miss (`InvalidFlowLogId.NotFound`), where a value outside the form never
/// reaches the lookup. The rows differ in the id form that is well formed — both forms EC2
/// has issued ids in — and in the value that is not.
#[tokio::test]
async fn a_well_formed_absent_id_is_a_lookup_miss_and_an_unreadable_one_is_not() {
    let (router, _engine) = super::ec2_query::emulator();

    // (case, the id the delete names, the code it must be refused with)
    let cases: [(&str, &str, &str); 5] = [
        ("the long form, absent", "fl-0123456789abcdef0", "InvalidFlowLogId.NotFound"),
        ("the short form, absent", "fl-01234567", "InvalidFlowLogId.NotFound"),
        ("the null id, absent", "fl-00000000000000000", "InvalidFlowLogId.NotFound"),
        ("a word for a suffix", "fl-bogus", "InvalidParameterValue"),
        ("a suffix of the wrong length", "fl-0123456789abcde", "InvalidParameterValue"),
    ];

    for (case, flow_log_id, expected_code) in cases {
        let params = indexed("FlowLogId", &[flow_log_id]);
        let (status, body) =
            super::ec2_query::call(&router, REGION, "DeleteFlowLogs", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
    }
}
