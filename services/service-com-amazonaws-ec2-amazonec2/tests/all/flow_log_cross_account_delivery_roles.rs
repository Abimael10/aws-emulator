// SPDX-License-Identifier: CC0-1.0

//! The two delivery-role members of `CreateFlowLogs` are a PAIR for cross-account delivery,
//! driven over the wire the service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=CreateFlowLogs`,
//! answered with XML; README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: "The ARN of the IAM role that allows Amazon EC2 to publish flow logs
//! across accounts" (`CreateFlowLogsRequest$DeliverCrossAccountRole`) and "the ARN of the IAM
//! role that allows Amazon EC2 to publish flow logs to the log destination. This parameter is
//! required if the destination type is `cloud-watch-logs`, or if the destination type is
//! `kinesis-data-firehose` and the delivery stream and the resources to monitor are in different
//! accounts" (`$DeliverLogsPermissionArn`) are the two halves of a cross-account delivery, so a
//! request that names one of them without the other is refused HTTP 400 `InvalidParameter` with
//! the sentence naming both members, and NOTHING is stored. The one destination type that takes
//! `DeliverLogsPermissionArn` on its own is `cloud-watch-logs`, where that member is mandatory
//! whether or not the delivery crosses accounts.
//!
//! The rows differ along what the rule generalises over: which destination type is in force
//! (each published member of `LogDestinationType`, and the documented default a request that
//! names none gets), which of the two members the request carries, and which it leaves out.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, FLOW_LOG_ID_PREFIX};

use super::ec2_query::{call, element, elements, error_code, error_message};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The sentence a request that gave only half a cross-account delivery is refused with — fixed
/// text naming both modelled members, with no part taken from the request.
const PAIR_REQUIRED_MESSAGE: &str = "When providing a role for cross-account delivery, both the \
                                     DeliverLogsPermissionArn and DeliverCrossAccountRole \
                                     fields must be set.";

/// EC2's class for a member combination it will not accept.
const INVALID_PARAMETER: &str = "InvalidParameter";

/// The role that allows Amazon EC2 to publish to the destination
/// (`$DeliverLogsPermissionArn`), and the one it reaches the monitored account through
/// (`$DeliverCrossAccountRole`).
const DELIVERY_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/flow-logs-delivery";
const CROSS_ACCOUNT_ROLE_ARN: &str = "arn:aws:iam::111122223333:role/flow-logs-cross-account";

/// One destination per published `LogDestinationType` member, in the ARN forms
/// `CreateFlowLogsRequest$LogDestination` documents.
const LOG_GROUP_ARN: &str = "arn:aws:logs:us-east-1:000000000000:log-group:/aws/vpc/flow-logs";
const BUCKET_ARN: &str = "arn:aws:s3:::flow-log-roles-bucket/vpc/";
const STREAM_ARN: &str = "arn:aws:firehose:us-east-1:000000000000:deliverystream:flow-logs";

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// A VPC created over the wire, by id — so a create can name a VPC the account really has.
async fn create_vpc(router: &MultiServiceRouter, cidr_block: &str) -> String {
    let (status, body) =
        call(router, REGION, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    assert_eq!(
        status, 200,
        "CreateVpc {cidr_block}: expected HTTP 200, got {status} with body {body}"
    );
    element(&body, "vpcId").unwrap_or_else(|| panic!("CreateVpc answered no vpcId: {body}"))
}

/// One row of the role-pair table: the case, the destination type the request names (or none,
/// for the member's documented default), the destination of that type, and the delivery-role
/// members it carries.
type RolePairRow<'row> = (&'row str, Option<&'row str>, &'row str, Vec<Param>);

/// The `CreateFlowLogs` one row makes, over `vpc_id`.
async fn create_flow_logs(
    router: &MultiServiceRouter,
    vpc_id: &str,
    log_destination_type: Option<&str>,
    log_destination: &str,
    roles: &[Param],
) -> (u16, String) {
    let mut params = vec![
        param("ResourceType", "VPC"),
        param("ResourceId.1", vpc_id),
        param("TrafficType", "ALL"),
        param("LogDestination", log_destination),
    ];
    if let Some(log_destination_type) = log_destination_type {
        params.push(param("LogDestinationType", log_destination_type));
    }
    params.extend_from_slice(roles);
    call(router, REGION, "CreateFlowLogs", &params).await
}

/// A request that names one of the two cross-account delivery roles without the other is
/// refused, whichever destination type is in force and whichever of the two it left out — and
/// the account is left with no flow log at all.
#[tokio::test]
async fn half_a_cross_account_delivery_role_pair_is_refused_naming_both_members() {
    let (router, engine) = emulator();
    let vpc_id = create_vpc(&router, "10.230.0.0/16").await;

    let rows: Vec<RolePairRow> = vec![
        (
            "an Amazon S3 destination with the destination role alone",
            Some("s3"),
            BUCKET_ARN,
            vec![param("DeliverLogsPermissionArn", DELIVERY_ROLE_ARN)],
        ),
        (
            "an Amazon S3 destination with the cross-account role alone",
            Some("s3"),
            BUCKET_ARN,
            vec![param("DeliverCrossAccountRole", CROSS_ACCOUNT_ROLE_ARN)],
        ),
        (
            "a Firehose destination with the destination role alone",
            Some("kinesis-data-firehose"),
            STREAM_ARN,
            vec![param("DeliverLogsPermissionArn", DELIVERY_ROLE_ARN)],
        ),
        (
            "a Firehose destination with the cross-account role alone",
            Some("kinesis-data-firehose"),
            STREAM_ARN,
            vec![param("DeliverCrossAccountRole", CROSS_ACCOUNT_ROLE_ARN)],
        ),
        (
            "a CloudWatch Logs destination with the cross-account role alone",
            Some("cloud-watch-logs"),
            LOG_GROUP_ARN,
            vec![param("DeliverCrossAccountRole", CROSS_ACCOUNT_ROLE_ARN)],
        ),
        (
            "the default destination type with the cross-account role alone",
            None,
            LOG_GROUP_ARN,
            vec![param("DeliverCrossAccountRole", CROSS_ACCOUNT_ROLE_ARN)],
        ),
    ];

    for (case, log_destination_type, log_destination, roles) in rows {
        let (status, body) = create_flow_logs(
            &router,
            &vpc_id,
            log_destination_type,
            log_destination,
            &roles,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            PAIR_REQUIRED_MESSAGE,
            "case {case:?}: expected the message {PAIR_REQUIRED_MESSAGE:?}, got {body}"
        );
    }

    assert!(
        engine.state::<Ec2State>().lock().flow_logs.is_empty(),
        "a refused create must store nothing, but the store holds flow logs"
    );
}

/// The other side of the same rule: a request that names BOTH roles is served, and so is one
/// that names the destination role alone under the one destination type that takes it on its
/// own — so the refusal above is about the pair and not about either member.
#[tokio::test]
async fn both_roles_together_and_the_cloud_watch_logs_role_alone_are_served() {
    let (router, _engine) = emulator();

    let rows: Vec<RolePairRow> = vec![
        (
            "a CloudWatch Logs destination with the destination role alone",
            Some("cloud-watch-logs"),
            LOG_GROUP_ARN,
            vec![param("DeliverLogsPermissionArn", DELIVERY_ROLE_ARN)],
        ),
        (
            "the default destination type with the destination role alone",
            None,
            LOG_GROUP_ARN,
            vec![param("DeliverLogsPermissionArn", DELIVERY_ROLE_ARN)],
        ),
        (
            "a CloudWatch Logs destination with both roles",
            Some("cloud-watch-logs"),
            LOG_GROUP_ARN,
            vec![
                param("DeliverLogsPermissionArn", DELIVERY_ROLE_ARN),
                param("DeliverCrossAccountRole", CROSS_ACCOUNT_ROLE_ARN),
            ],
        ),
        (
            "an Amazon S3 destination with both roles",
            Some("s3"),
            BUCKET_ARN,
            vec![
                param("DeliverLogsPermissionArn", DELIVERY_ROLE_ARN),
                param("DeliverCrossAccountRole", CROSS_ACCOUNT_ROLE_ARN),
            ],
        ),
        (
            "an Amazon S3 destination with neither role",
            Some("s3"),
            BUCKET_ARN,
            Vec::new(),
        ),
        (
            "a Firehose destination with both roles",
            Some("kinesis-data-firehose"),
            STREAM_ARN,
            vec![
                param("DeliverLogsPermissionArn", DELIVERY_ROLE_ARN),
                param("DeliverCrossAccountRole", CROSS_ACCOUNT_ROLE_ARN),
            ],
        ),
        (
            "a Firehose destination with neither role",
            Some("kinesis-data-firehose"),
            STREAM_ARN,
            Vec::new(),
        ),
    ];

    for (index, (case, log_destination_type, log_destination, roles)) in
        rows.into_iter().enumerate()
    {
        // One VPC per row: one resource carries one flow log of the same configuration and
        // destination, so each row monitors a resource of its own.
        let vpc_id = create_vpc(&router, &format!("10.{}.0.0/16", 200 + index)).await;
        let (status, body) = create_flow_logs(
            &router,
            &vpc_id,
            log_destination_type,
            log_destination,
            &roles,
        )
        .await;
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
