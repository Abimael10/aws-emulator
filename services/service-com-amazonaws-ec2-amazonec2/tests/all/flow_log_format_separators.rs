// SPDX-License-Identifier: CC0-1.0

//! How a `CreateFlowLogs` `LogFormat` SEPARATES the fields it names, driven over the wire the
//! service speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! ONE rule is under test: a record format lays out ONE field per SPACE-SEPARATED token, each
//! token a single `${field-id}` marker — "specify the fields using the `${field-id}` format,
//! separated by spaces" (`CreateFlowLogsRequest$LogFormat`). So a token that is anything else
//! — two markers joined by a separator of its own, two markers with nothing between them, a
//! marker with text beside it, or a field id written with no marker at all — names a field no
//! flow log record has, and the create carrying it is refused HTTP 400 `InvalidParameter`
//! naming that text, with NOTHING stored: the monitored resource is left with exactly the flow
//! logs it already had, which is what the following `DescribeFlowLogs` reports.
//!
//! The rows span the class: the separators a caller might reach for instead of a space (a
//! comma, a semicolon, a pipe, no separator at all), where in the format the glued token sits,
//! a published field id written without its marker, and a token carrying text beside the
//! marker — against served formats that differ in how many spaces separate their fields and
//! in whether a field reads a tag of the monitored resource. One refusal row names an Amazon
//! S3 destination the account holds no bucket for, so the format is shown to be read BEFORE
//! the destination is resolved: a refused create resolves nothing and stores nothing.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message, filters};
use super::flow_logs::cloud_watch_logs_params;
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The log group every CloudWatch Logs create below publishes to.
const LOG_GROUP: &str = "/aws/vpc/flow-logs/separators";

/// One VPC of the account, created over the wire, by id.
async fn create_vpc(router: &MultiServiceRouter, cidr_block: &str) -> String {
    let (status, body) = call(router, REGION, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    assert_eq!(
        status, 200,
        "CreateVpc {cidr_block}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "vpcId").unwrap_or_else(|| panic!("CreateVpc answered no vpcId: {body}"))
}

/// One `CreateFlowLogs` on `vpc_id` carrying `log_format`, as its status and body.
async fn create_with_format(
    router: &MultiServiceRouter,
    vpc_id: &str,
    log_format: &str,
) -> (u16, String) {
    let mut params = cloud_watch_logs_params("VPC", vpc_id, LOG_GROUP);
    params.push(param("LogFormat", log_format));
    call(router, REGION, "CreateFlowLogs", &params).await
}

/// The flow log ids the account holds for `vpc_id`, in the order `DescribeFlowLogs` reports
/// them.
async fn flow_log_ids(router: &MultiServiceRouter, case: &str, vpc_id: &str) -> Vec<String> {
    let params = filters(&[("resource-id", &[vpc_id])]);
    let (status, body) = call(router, REGION, "DescribeFlowLogs", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeFlowLogs, got {status} with body {body}"
    );
    elements(&body, "flowLogId")
}

/// The sentence a refusal of this class carries, for the texts it names.
fn unknown_fields_message(unknown: &[&str]) -> String {
    let mut sorted: Vec<&str> = unknown.to_vec();
    sorted.sort_unstable();
    format!(
        "Unknown fields provided: {fields}",
        fields = sorted
            .iter()
            .map(|field| format!("'{field}'"))
            .collect::<Vec<String>>()
            .join(", "),
    )
}

/// RULE: a format whose fields are not separated by spaces names no field under the token that
/// glues them, so the create is refused and the monitored resource keeps exactly the flow logs
/// it had.
#[tokio::test]
async fn a_format_that_does_not_separate_its_fields_with_spaces_is_refused_and_stores_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(&router, "10.233.0.0/16").await;

    // One flow log the account already holds, so a refusal that created something would show
    // up as a second one.
    let (status, body) = create_with_format(&router, &vpc_id, "${srcaddr} ${dstaddr}").await;
    assert_eq!(
        status, 200,
        "seeding one flow log: expected HTTP 200, got {status} with body {body}"
    );
    let before = flow_log_ids(&router, "before the refusals", &vpc_id).await;
    assert_eq!(
        before.len(),
        1,
        "before the refusals: expected the one seeded flow log, got {before:?}"
    );

    // Each row: what it covers, the format the create asks for, and the texts the refusal must
    // name as fields the record does not have.
    let rows: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "two markers joined by a comma",
            "${srcaddr},${dstaddr}",
            vec!["srcaddr},${dstaddr"],
        ),
        (
            "two markers joined by a semicolon",
            "${srcaddr};${dstaddr}",
            vec!["srcaddr};${dstaddr"],
        ),
        (
            "two markers joined by a pipe",
            "${srcaddr}|${dstaddr}",
            vec!["srcaddr}|${dstaddr"],
        ),
        (
            "two markers with nothing between them",
            "${srcaddr}${dstaddr}",
            vec!["srcaddr}${dstaddr"],
        ),
        (
            "a glued token after a well-separated one",
            "${version} ${srcaddr},${dstaddr}",
            vec!["srcaddr},${dstaddr"],
        ),
        (
            "a glued token before a well-separated one",
            "${srcaddr},${dstaddr} ${version}",
            vec!["srcaddr},${dstaddr"],
        ),
        (
            "two glued tokens among well-separated ones",
            "${version} ${srcaddr},${dstaddr} ${action} ${start},${end}",
            vec!["srcaddr},${dstaddr", "start},${end"],
        ),
        (
            "a published field id written with no marker at all",
            "${srcaddr} dstaddr",
            vec!["dstaddr"],
        ),
        (
            "a marker carrying text beside it",
            "${srcaddr} packets=${packets}",
            vec!["packets=${packets}"],
        ),
        (
            "a whole format of three markers the caller comma-separated",
            "${interface-id},${action},${log-status}",
            vec!["interface-id},${action},${log-status"],
        ),
    ];

    for (case, log_format, unknown) in &rows {
        let (status, body) = create_with_format(&router, &vpc_id, log_format).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for the format {log_format:?}, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameter",
            "case {case:?}: expected InvalidParameter for {log_format:?}, got {body}"
        );
        let expected = unknown_fields_message(unknown);
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the message {expected:?}, got {body}"
        );
        let after = flow_log_ids(&router, case, &vpc_id).await;
        assert_eq!(
            &after, &before,
            "case {case:?}: a refused create must store nothing, but the resource now carries \
             {after:?}"
        );
    }

    assert_eq!(
        engine
            .state::<ec2::state::ComAmazonawsEc2Amazonec2State>()
            .lock()
            .flow_logs
            .len(),
        1,
        "a refused create must leave the store exactly as it was"
    );
}

/// RULE: the format is read BEFORE the destination the records would go to is resolved, so a
/// create carrying both a glued token and an Amazon S3 destination the account holds no bucket
/// for is refused for the format and stores nothing — the destination is never reached.
#[tokio::test]
async fn a_glued_token_is_refused_before_the_destination_is_resolved() {
    let (router, engine) = super::ec2_query::emulator();
    let case = "a glued token beside an absent bucket";
    let vpc_id = create_vpc(&router, "10.234.0.0/16").await;
    let params: Vec<Param> = vec![
        param("ResourceType", "VPC"),
        param("ResourceId.1", &vpc_id),
        param("TrafficType", "ALL"),
        param("LogDestinationType", "s3"),
        param("LogDestination", "arn:aws:s3:::flow-log-separators-absent/bad/"),
        param("LogFormat", "${srcaddr},${dstaddr}"),
    ];
    let (status, body) = call(&router, REGION, "CreateFlowLogs", &params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameter",
        "case {case:?}: expected InvalidParameter, got {body}"
    );
    assert_eq!(
        error_message(&body),
        unknown_fields_message(&["srcaddr},${dstaddr"]),
        "case {case:?}: expected the format refusal rather than the destination's, got {body}"
    );
    let held = flow_log_ids(&router, case, &vpc_id).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected the VPC to carry no flow log at all, got {held:?}"
    );
    assert!(
        engine
            .state::<ec2::state::ComAmazonawsEc2Amazonec2State>()
            .lock()
            .flow_logs
            .is_empty(),
        "case {case:?}: a refused create must store nothing"
    );
}

/// RULE: a format whose every token IS one marker of a published field is served, however many
/// spaces separate the tokens — and the flow log reports the format back verbatim.
#[tokio::test]
async fn a_format_whose_tokens_are_each_one_marker_is_served() {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = create_vpc(&router, "10.235.0.0/16").await;

    // Each row: what it covers, and the format the create asks for.
    let rows: [(&str, &str); 5] = [
        ("one field", "${srcaddr}"),
        ("fields separated by one space", "${srcaddr} ${dstaddr}"),
        (
            "fields separated by several spaces",
            "${srcaddr}   ${dstaddr}    ${action}",
        ),
        (
            "a field that reads a tag of the monitored resource",
            "${srcaddr} ${resource-tag:Name}",
        ),
        (
            "every field of the documented default layout",
            "${version} ${account-id} ${interface-id} ${srcaddr} ${dstaddr} ${srcport} \
             ${dstport} ${protocol} ${packets} ${bytes} ${start} ${end} ${action} \
             ${log-status}",
        ),
    ];

    for (case, log_format) in rows {
        let (status, body) = create_with_format(&router, &vpc_id, log_format).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for the format {log_format:?}, got {status} with \
             body {body}"
        );
        let created = elements(&body, "item");
        assert_eq!(
            created.len(),
            1,
            "case {case:?}: expected exactly one flow log id, got {created:?} in {body}"
        );
    }

    let stored = flow_log_ids(&router, "every served format", &vpc_id).await;
    assert_eq!(
        stored.len(),
        rows.len(),
        "every served format: expected {expected} flow logs, got {stored:?}",
        expected = rows.len()
    );
}
