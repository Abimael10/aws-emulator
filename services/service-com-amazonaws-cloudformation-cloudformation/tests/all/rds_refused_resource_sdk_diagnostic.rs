// SPDX-License-Identifier: CC0-1.0

//! What a resource AMAZON RDS refused reports on its `CREATE_FAILED` event, end to end over
//! the awsQuery wire.
//!
//! RULE under test: **a resource provider reports a refused call as the exception its SDK
//! raised — the owning service's own sentence followed by that SDK's diagnostic of the call,
//! naming the service as its SDK names it and the HTTP status that service answered beside
//! the id of the call and the attempts that SDK made at it — so the failure is attributed to
//! the service that refused it, whatever type of that service failed and whichever of its
//! calls was refused. What FOLLOWS that envelope belongs to the SDK the provider was built
//! against, and for this service every recorded provider words the same attempt count. A
//! service whose reported wording this emulator has no record of has no diagnostic invented
//! for it.**
//!
//! Why it matters: a client reading a rolled-back deployment diagnoses it from the failed
//! resource's `ResourceStatusReason`. Without the attribution it cannot tell a refusal the
//! owning service answered from CloudFormation's own validation of the declaration, so a
//! template whose real fault is the thing that service named — too few Availability Zones
//! across a DB subnet group's subnets — reads as a fault in CloudFormation's handling of it,
//! and a refusal-taxonomy check that keys on the originating service cannot see it at all.
//!
//! The rows differ along what the rule generalises over: WHICH type of the owning service
//! failed (a DB subnet group and a DB instance), which call of it was refused, WHY that
//! service refused it (the Availability Zones its subnets cover, the subnets' VPCs, the
//! identifiers themselves) and the HTTP
//! STATUS that service answered (400 for a declaration it will not take, 404 for a resource
//! it does not hold — each from the fault's own `httpError` in RDS's model,
//! `smithy_query(query="operation",operation="CreateDBSubnetGroup,CreateDBInstance")`:
//! `DBSubnetGroupDoesNotCoverEnoughAZs` and `InvalidSubnet` are 400,
//! `DBSubnetGroupNotFoundFault` is 404). The control row is a type owned by a service this
//! emulator has recorded no SDK wording for, whose refusal is therefore reported exactly as
//! that service worded it.

use crate::harness::{
    assert_sdk_diagnostic_with_attempts, call_in, failed_resource_reason, member,
    quoted_and_class, xml_blocks, xml_text,
};
use crate::stack_lifecycle::{create_stack, router, wait_for_the_transition, REGION};

/// The SDK's own name for Amazon RDS, as the exception its SDK raises states it.
const RDS_SERVICE: &str = "Rds";

/// The status a resource its provider could not create records.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// One row: the stack to deploy, the resource the owning service refuses, and what its
/// reason must report.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    logical_id: &'static str,
    /// The service name and HTTP status the SDK diagnostic must state, or `None` for a row
    /// whose owning service has no recorded SDK wording — which must then carry no
    /// diagnostic at all.
    diagnostic: Option<(&'static str, u16)>,
    /// A fragment of the owning service's OWN sentence the quoted message must open with, or
    /// `None` for a row whose only subject is the diagnostic FORM: the sentence that service
    /// words for the refusal is then not what this suite is about, so it is not frozen here.
    sentence: Option<&'static str>,
    /// The `HandlerErrorCode` class the refusal falls in.
    class: &'static str,
}

/// A VPC, two subnets pinned to the SAME Availability Zone and a DB subnet group over both:
/// the group is created in Amazon RDS, which counts the zones its subnets lie in and refuses
/// a group that covers fewer than the minimum.
const SINGLE_ZONE_TEMPLATE: &str = r#"{"Resources":{
    "Vpc":{"Type":"AWS::EC2::VPC","Properties":{"CidrBlock":"10.0.0.0/16"}},
    "SubnetA":{"Type":"AWS::EC2::Subnet","Properties":{
      "VpcId":{"Ref":"Vpc"},"CidrBlock":"10.0.1.0/24",
      "AvailabilityZone":{"Fn::Select":[0,{"Fn::GetAZs":""}]}}},
    "SubnetB":{"Type":"AWS::EC2::Subnet","Properties":{
      "VpcId":{"Ref":"Vpc"},"CidrBlock":"10.0.2.0/24",
      "AvailabilityZone":{"Fn::Select":[0,{"Fn::GetAZs":""}]}}},
    "SubnetGroup":{"Type":"AWS::RDS::DBSubnetGroup","Properties":{
      "DBSubnetGroupName":"rds-diag-single-zone",
      "DBSubnetGroupDescription":"two subnets of one zone",
      "SubnetIds":[{"Ref":"SubnetA"},{"Ref":"SubnetB"}]}}}}"#;

/// The same type over subnet ids of the right FORM that no subnet of the account answers to,
/// which that service refuses for a different reason — and with a different error code — at
/// the same status.
const ABSENT_SUBNETS_TEMPLATE: &str = r#"{"Resources":{
    "SubnetGroup":{"Type":"AWS::RDS::DBSubnetGroup","Properties":{
      "DBSubnetGroupName":"rds-diag-absent-subnets",
      "DBSubnetGroupDescription":"a group over ids no subnet answers to",
      "SubnetIds":["subnet-0123456789abcdef0","subnet-0fedcba9876543210"]}}}}"#;

/// And the same type over values that are no subnet id at all, which that service refuses on
/// the FORM of the ids before anything about the subnets they would name.
const MALFORMED_SUBNETS_TEMPLATE: &str = r#"{"Resources":{
    "SubnetGroup":{"Type":"AWS::RDS::DBSubnetGroup","Properties":{
      "DBSubnetGroupName":"rds-diag-malformed-subnets",
      "DBSubnetGroupDescription":"a group over values that name no subnet",
      "SubnetIds":["not-a-subnet-id","also-not-a-subnet-id"]}}}}"#;

/// The same type over two subnets of two DIFFERENT VPCs: a DB subnet group is a set of
/// subnets of ONE VPC, so that list makes up no group, and the refusal names the two VPCs the
/// declared subnets straddle.
const CROSS_VPC_TEMPLATE: &str = r#"{"Resources":{
    "VpcA":{"Type":"AWS::EC2::VPC","Properties":{"CidrBlock":"10.20.0.0/16"}},
    "VpcB":{"Type":"AWS::EC2::VPC","Properties":{"CidrBlock":"10.21.0.0/16"}},
    "SubnetA":{"Type":"AWS::EC2::Subnet","Properties":{
      "VpcId":{"Ref":"VpcA"},"CidrBlock":"10.20.1.0/24",
      "AvailabilityZone":{"Fn::Select":[0,{"Fn::GetAZs":""}]}}},
    "SubnetB":{"Type":"AWS::EC2::Subnet","Properties":{
      "VpcId":{"Ref":"VpcB"},"CidrBlock":"10.21.1.0/24",
      "AvailabilityZone":{"Fn::Select":[1,{"Fn::GetAZs":""}]}}},
    "SubnetGroup":{"Type":"AWS::RDS::DBSubnetGroup","Properties":{
      "DBSubnetGroupName":"rds-diag-cross-vpc",
      "DBSubnetGroupDescription":"subnets of two VPCs",
      "SubnetIds":[{"Ref":"SubnetA"},{"Ref":"SubnetB"}]}}}}"#;

/// Another of that service's types, whose create names a DB subnet group Amazon RDS does not
/// hold: the same service, a different call, and the status it answers for a resource that is
/// not there.
const ABSENT_GROUP_TEMPLATE: &str = r#"{"Resources":{
    "Database":{"Type":"AWS::RDS::DBInstance","Properties":{
      "DBInstanceIdentifier":"rds-diag-absent-group-db",
      "Engine":"mysql","DBInstanceClass":"db.t3.micro",
      "DBSubnetGroupName":"rds-diag-no-such-group"}}}}"#;

/// The control row's template: a repository whose declared name Amazon ECR's own
/// `RepositoryName` pattern does not admit (that pattern is lower case only). ECR refuses the
/// create, and this emulator has no record of how that service's SDK words a diagnostic, so
/// the reason carries that service's sentence and nothing more.
const REFUSED_REPOSITORY_TEMPLATE: &str = r#"{"Resources":{
    "Registry":{"Type":"AWS::ECR::Repository","Properties":{
      "RepositoryName":"RdsDiagRefusedImages"}}}}"#;

/// The class a refusal of the REQUEST falls in, and the class of one about a resource that is
/// not there — both values of the model's `HandlerErrorCode` enum.
const INVALID_REQUEST: &str = "InvalidRequest";
const NOT_FOUND: &str = "NotFound";

const ROWS: &[Row] = &[
    Row {
        case: "a DB subnet group whose subnets cover one Availability Zone",
        stack: "rds-diag-single-zone",
        template: SINGLE_ZONE_TEMPLATE,
        logical_id: "SubnetGroup",
        diagnostic: Some((RDS_SERVICE, 400)),
        sentence: Some("Availability Zone (AZ) coverage requirement"),
        class: INVALID_REQUEST,
    },
    Row {
        case: "a DB subnet group over subnet ids no subnet answers to",
        stack: "rds-diag-absent-subnets",
        template: ABSENT_SUBNETS_TEMPLATE,
        logical_id: "SubnetGroup",
        diagnostic: Some((RDS_SERVICE, 400)),
        sentence: Some("are invalid."),
        class: INVALID_REQUEST,
    },
    Row {
        case: "a DB subnet group over values that are no subnet id at all",
        stack: "rds-diag-malformed-subnets",
        template: MALFORMED_SUBNETS_TEMPLATE,
        logical_id: "SubnetGroup",
        diagnostic: Some((RDS_SERVICE, 400)),
        // Only the diagnostic: how the owning service words a refusal of the id's FORM is
        // that service's own concern, and not something this suite should pin.
        sentence: None,
        class: INVALID_REQUEST,
    },
    Row {
        case: "a DB subnet group over subnets of two different VPCs",
        stack: "rds-diag-cross-vpc",
        template: CROSS_VPC_TEMPLATE,
        logical_id: "SubnetGroup",
        diagnostic: Some((RDS_SERVICE, 400)),
        sentence: Some("Subnet has different VPC Id: "),
        class: INVALID_REQUEST,
    },
    Row {
        case: "a DB instance naming a DB subnet group the service does not hold",
        stack: "rds-diag-absent-group",
        template: ABSENT_GROUP_TEMPLATE,
        logical_id: "Database",
        diagnostic: Some((RDS_SERVICE, 404)),
        sentence: Some("rds-diag-no-such-group"),
        class: NOT_FOUND,
    },
    Row {
        case: "a repository whose declared name the owning service will not take",
        stack: "rds-diag-unrecorded-wording",
        template: REFUSED_REPOSITORY_TEMPLATE,
        logical_id: "Registry",
        diagnostic: None,
        // That service's own sentence names the MEMBER the declaration got wrong, which is
        // what reaches the reason when no diagnostic is appended to it.
        sentence: Some("at 'repositoryName' failed to satisfy constraint"),
        class: INVALID_REQUEST,
    },
];

#[tokio::test]
async fn a_refused_resource_is_attributed_to_the_service_that_refused_it() {
    for row in ROWS {
        let router = router();
        let (status_code, created) = create_stack(&router, row.stack, row.template, &[]).await;
        assert_eq!(
            status_code, 200,
            "case {:?}: CreateStack must answer 200, got {status_code} {created}",
            row.case
        );
        // The create's own terminal status, and the rollback's: both are on the stack's clock.
        wait_for_the_transition().await;
        wait_for_the_transition().await;

        let reason = failed_resource_reason(
            row.case,
            &router,
            REGION,
            row.stack,
            row.logical_id,
            CREATE_FAILED,
        )
        .await;
        let (quoted, class) = quoted_and_class(row.case, &reason);
        if let Some(sentence) = row.sentence {
            assert!(
                quoted.contains(sentence),
                "case {:?}: expected the owning service's own sentence carrying {sentence:?}, \
                 got {quoted:?}",
                row.case,
            );
        }
        assert_eq!(
            class, row.class,
            "case {:?}: expected the {} class, got {class:?} in {reason:?}",
            row.case, row.class
        );
        match row.diagnostic {
            Some((service, status)) => {
                // Amazon RDS's SDK words the ATTEMPT-counting shape, so the quoted exception
                // carries the envelope and the attempts it made at the call.
                assert_sdk_diagnostic_with_attempts(row.case, &quoted, service, status);
            }
            None => assert!(
                !quoted.contains("(Service:"),
                "case {:?}: expected no diagnostic invented for a service none is recorded \
                 for, got {quoted:?}",
                row.case
            ),
        }
    }
}

#[tokio::test]
async fn the_refusal_names_the_zone_coverage_the_declared_subnets_add_up_to() {
    // The variable part of that service's sentence comes from the state of the subnets the
    // declaration named, not from the request: two subnets of one zone cover ONE zone, which
    // the refusal names.
    let case = "two subnets of one Availability Zone";
    let router = router();
    let (status_code, created) =
        create_stack(&router, "rds-diag-zone-named", SINGLE_ZONE_TEMPLATE, &[]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateStack must answer 200, got {status_code} {created}"
    );
    wait_for_the_transition().await;
    wait_for_the_transition().await;

    let reason = failed_resource_reason(
        case,
        &router,
        REGION,
        "rds-diag-zone-named",
        "SubnetGroup",
        CREATE_FAILED,
    )
    .await;
    let (quoted, _) = quoted_and_class(case, &reason);
    const COVERAGE: &str = "Current AZ coverage: ";
    let covered = quoted
        .split_once(COVERAGE)
        .and_then(|(_, rest)| rest.split_once('.'))
        .map(|(covered, _)| covered)
        .unwrap_or_else(|| panic!("case {case:?}: expected {COVERAGE:?}, got {quoted:?}"));
    assert!(
        covered.starts_with(REGION) && covered.len() > REGION.len(),
        "case {case:?}: expected a zone of the Region the subnets were created in, got \
         {covered:?}"
    );
    assert!(
        !covered.contains(','),
        "case {case:?}: expected the ONE zone both subnets lie in, got {covered:?}"
    );
}

/// The `PhysicalResourceId` the stack recorded on `logical_id`'s own event in
/// `resource_status` — how a resource of a ROLLED-BACK stack is still identified, which is
/// what lets the refusal below be compared against the VPCs the stack really created.
async fn event_physical_id(
    case: &str,
    router: &smithy_server_runtime::MultiServiceRouter,
    stack: &str,
    logical_id: &str,
    resource_status: &str,
) -> String {
    let (status_code, body) = call_in(
        router,
        REGION,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeStackEvents({stack}) must answer 200, got {status_code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .find(|entry| {
            xml_text(entry, "LogicalResourceId").as_deref() == Some(logical_id)
                && xml_text(entry, "ResourceStatus").as_deref() == Some(resource_status)
        })
        .and_then(|entry| xml_text(entry, "PhysicalResourceId"))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected {stack} to have recorded a {resource_status} \
                 PhysicalResourceId for {logical_id}, got {body}"
            )
        })
}

#[tokio::test]
async fn the_cross_vpc_refusal_names_the_two_vpcs_the_declared_subnets_lie_in() {
    // The variable parts of that service's sentence come from the state of the subnets the
    // declaration named: the VPC each one lies in, which is the service that owns subnets'
    // own fact. The two VPCs are named in the order the declared subnets introduce them.
    let case = "one subnet of each of two VPCs";
    let stack = "rds-diag-cross-vpc-named";
    let router = router();
    let (status_code, created) = create_stack(&router, stack, CROSS_VPC_TEMPLATE, &[]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateStack must answer 200, got {status_code} {created}"
    );
    wait_for_the_transition().await;
    wait_for_the_transition().await;

    let reason =
        failed_resource_reason(case, &router, REGION, stack, "SubnetGroup", CREATE_FAILED).await;
    let (quoted, _) = quoted_and_class(case, &reason);
    const OPENS: &str = "Subnet has different VPC Id: ";
    const THAN: &str = " than ";
    let (named_first, named_second) = quoted
        .strip_prefix(OPENS)
        .and_then(|rest| rest.split_once(THAN))
        .map(|(named_first, rest)| {
            let named_second = rest
                .split_once(" (Service: ")
                .map(|(named_second, _)| named_second)
                .unwrap_or(rest);
            (named_first.to_string(), named_second.to_string())
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {OPENS:?} and {THAN:?}, got {quoted:?}")
        });

    // Each named VPC is one the stack created, and they are the two the declared subnets
    // really lie in — in the order the declaration's own subnets introduce them.
    let first_vpc = event_physical_id(case, &router, stack, "VpcA", "CREATE_COMPLETE").await;
    let second_vpc = event_physical_id(case, &router, stack, "VpcB", "CREATE_COMPLETE").await;
    assert_ne!(
        first_vpc, second_vpc,
        "case {case:?}: the two declared VPCs must be two VPCs, got {first_vpc:?}"
    );
    assert_eq!(
        (named_first.as_str(), named_second.as_str()),
        (first_vpc.as_str(), second_vpc.as_str()),
        "case {case:?}: expected the VPCs of the declared subnets in the order those subnets \
         name them, got {quoted:?}"
    );
}
