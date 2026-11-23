// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a request that is wrong in more than one way is answered with, across the
//! operations whose order of validation decides it — driven over the ec2Query wire the
//! service actually speaks, through the crate's shared [`super::ec2_query`] harness.
//!
//! RULE: a request is answered for the FIRST thing the service has to say about it, and that
//! order is fixed — the request's own form and the parameters it left out come before the
//! VALUE of any other parameter, and both come before any resource the request names is
//! resolved against state; and once that order has chosen a condition, the refusal carries
//! the CLASS of that condition rather than of a later one. So a caller is never told a
//! resource is missing because of a value it mistyped, nor told to correct a value when it
//! left a parameter out.
//!
//! The suites that own each operation assert what it does with a request that is wrong in
//! ONE way; this one asserts the order and the class when two faults are in the request at
//! once, which is a rule about the operations together rather than about any one of them.
//! Each table's rows differ along what the rule generalises over: which of the two faults is
//! in the request, which KIND of first fault it is (a value outside a printed form, an
//! identifier the service never issued, a parameter left out, a value outside a published
//! length), and which value the second fault carries.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn action(router: &MultiServiceRouter, name: &str, params: &[(String, String)]) -> (u16, String) {
    call(router, REGION, name, params).await
}

/// One call that must succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, name: &str, params: &[(String, String)]) -> String {
    let (status, body) = action(router, name, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {name}, got {status} with body {body}"
    );
    body
}

/// Assert that `action` refuses `params` with HTTP 400, `code` and `message`.
async fn assert_refusal(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    params: &[(String, String)],
    code: &str,
    message: &str,
) {
    let (status, body) = action(router, name, params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 from {name}, got {status} with body {body}"
    );
    let answered_code = error_code(&body);
    assert_eq!(
        answered_code, code,
        "case {case:?}: expected the error code {code:?} from {name}, got {answered_code:?} in \
         {body}"
    );
    let answered_message = error_message(&body);
    assert_eq!(
        answered_message, message,
        "case {case:?}: expected the message {message:?} from {name}, got \
         {answered_message:?}"
    );
}

// ---------------------------------------------------------------------------
// AssociateSecurityGroupVpc: the group id's form before the VPC is resolved
// ---------------------------------------------------------------------------

/// A VPC with `cidr_block`, and the id it was minted with.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr_block: &str) -> String {
    let body = ok(case, router, "CreateVpc", &[p("CidrBlock", cidr_block)]).await;
    element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId in {body}"))
}

/// A security group of `vpc_id`, and the id it was minted with.
async fn create_group(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateSecurityGroup",
        &[
            p("GroupName", "ordered"),
            p("GroupDescription", "a group this suite associates"),
            p("VpcId", vpc_id),
        ],
    )
    .await;
    element(&body, "groupId").unwrap_or_else(|| panic!("case {case:?}: no groupId in {body}"))
}

/// RULE for `AssociateSecurityGroupVpc`: the `GroupId` is READ before the `VpcId` is
/// resolved, so a request carrying an unreadable group id beside a VPC id the account holds
/// no VPC for is answered for the GROUP ID — and only a group id the service can read lets
/// the VPC's own refusal through.
///
/// The rows differ along the KIND of unreadable group id: a value outside the printed id
/// form (the prefix alone, a suffix of neither published length, another resource's prefix),
/// which the request boundary refuses in its own words, and a value INSIDE the printed long
/// form that the service never issued, which it refuses in its own. The last two rows are
/// the controls that show the order rather than a blanket refusal: a short-form group id the
/// account holds no group for, and a group id the account DOES hold, each beside the same
/// unknown VPC, let the VPC's refusal through.
#[tokio::test]
async fn an_unreadable_group_id_is_answered_before_an_unknown_vpc() {
    let router = emulator();
    let home = create_vpc("the home VPC", &router, "10.41.0.0/16").await;
    let held = create_group("the group the account holds", &router, &home).await;

    // A VPC id that is well formed and names no VPC of the account: the SECOND fault every
    // row below carries.
    let unknown_vpc = "vpc-0a1b2c3d4e5f60718";

    // (case, the group id, the code expected, the message expected)
    let rows: [(&str, String, &str, String); 6] = [
        (
            "the prefix with no digits",
            "sg-".to_string(),
            "InvalidGroupId.Malformed",
            "Invalid id: \"sg-\"".to_string(),
        ),
        (
            "a suffix of neither published length",
            "sg-0a1b2c".to_string(),
            "InvalidGroupId.Malformed",
            "Invalid id: \"sg-0a1b2c\"".to_string(),
        ),
        (
            "another resource's id",
            "vpc-0a1b2c3d4e5f60718".to_string(),
            "InvalidGroupId.Malformed",
            "Invalid id: \"vpc-0a1b2c3d4e5f60718\"".to_string(),
        ),
        (
            "a long id the service never issued",
            "sg-0123456789abcdef0".to_string(),
            "InvalidGroupId.Malformed",
            "The security-group ID 'sg-0123456789abcdef0' is malformed".to_string(),
        ),
        (
            "a short group id no group has",
            "sg-9f8e7d6c".to_string(),
            "InvalidVpcID.NotFound",
            format!("The vpc ID '{unknown_vpc}' does not exist"),
        ),
        (
            "a group id the account holds",
            held.clone(),
            "InvalidVpcID.NotFound",
            format!("The vpc ID '{unknown_vpc}' does not exist"),
        ),
    ];

    for (case, group_id, code, message) in rows {
        assert_refusal(
            case,
            &router,
            "AssociateSecurityGroupVpc",
            &[p("GroupId", group_id.clone()), p("VpcId", unknown_vpc)],
            code,
            &message,
        )
        .await;
    }

    // Nothing above wrote an association: the reader reports none for the group the account
    // holds.
    let body = ok(
        "after every refusal",
        &router,
        "DescribeSecurityGroupVpcAssociations",
        &[],
    )
    .await;
    assert!(
        element(&body, "groupId").is_none(),
        "a refused associate must write nothing, got {body}"
    );
}

// ---------------------------------------------------------------------------
// RegisterInstanceEventNotificationAttributes: the layer's bounds first
// ---------------------------------------------------------------------------

/// The clause the request-validation layer refuses a registered tag key outside its length
/// bounds with, for the one key `key` — built here from the template rather than from the
/// crate's own builder, so the test states the sentence independently.
fn length_violation(key: &str) -> String {
    // The layer prints a list parameter's value as its entries in brackets, each entry cut
    // to the bound every echoed value of this service is held to.
    let echoed: String = key.chars().take(128).collect();
    format!(
        "1 validation error detected: Value '[{echoed}]' at \
         'resourceTagAttribute.resourceTagKeys' failed to satisfy constraint: Member must \
         satisfy constraint: [Member must have length less than or equal to 256, Member must \
         have length greater than or equal to 1]"
    )
}

/// RULE for `RegisterInstanceEventNotificationAttributes`: a tag key outside the LENGTH
/// RANGE the request-validation layer holds that member to never reaches the operation, so it
/// is answered with that layer's own class and clause; a key inside that range is then held
/// to the service's own, narrower key bound, with the service's own class and sentence.
///
/// The rows span the member's length domain: each published bound of the layer's range, one
/// past each of them, and the service's own bound and one past it — so the two layers are
/// told apart by where the key's length sits rather than by which operation was called.
#[tokio::test]
async fn a_tag_key_outside_the_published_lengths_is_the_validation_layers_refusal() {
    let router = emulator();
    // The key length EC2's own tagging rules admit, as the crate states it.
    let service_bound = ec2::state::MAX_TAG_KEY_CHARS;

    // (case, the key, the expected outcome)
    enum Expected {
        /// Accepted: the key is inside every bound.
        Registered,
        /// Outside the validation layer's length range.
        LengthViolation,
        /// Inside that range and outside the service's own key bound.
        ServiceBound,
    }
    let rows: Vec<(String, String, Expected)> = vec![
        (
            "no characters at all".to_string(),
            String::new(),
            Expected::LengthViolation,
        ),
        (
            "the shortest key the layer admits".to_string(),
            "a".to_string(),
            Expected::Registered,
        ),
        (
            format!("the longest key the service admits ({service_bound})"),
            "k".repeat(service_bound),
            Expected::Registered,
        ),
        (
            "one character past the service's bound".to_string(),
            "k".repeat(service_bound + 1),
            Expected::ServiceBound,
        ),
        (
            "the longest key the layer admits (256)".to_string(),
            "k".repeat(256),
            Expected::ServiceBound,
        ),
        (
            "one character past the layer's bound (257)".to_string(),
            "k".repeat(257),
            Expected::LengthViolation,
        ),
    ];

    for (case, key, expected) in rows {
        let params = indexed("InstanceTagAttribute.InstanceTagKey", &[key.as_str()]);
        match expected {
            Expected::Registered => {
                let body = ok(
                    &case,
                    &router,
                    "RegisterInstanceEventNotificationAttributes",
                    &params,
                )
                .await;
                assert!(
                    body.contains(&key),
                    "case {case:?}: an admitted key is registered, got {body}"
                );
            }
            Expected::LengthViolation => {
                assert_refusal(
                    &case,
                    &router,
                    "RegisterInstanceEventNotificationAttributes",
                    &params,
                    ec2::state::INVALID_REQUEST,
                    &length_violation(&key),
                )
                .await;
            }
            Expected::ServiceBound => {
                assert_refusal(
                    &case,
                    &router,
                    "RegisterInstanceEventNotificationAttributes",
                    &params,
                    ec2::state::INVALID_PARAMETER_VALUE,
                    &format!("A tag key accepts at most {service_bound} characters"),
                )
                .await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ProvisionByoipCidr: the parameter left out before the range's value
// ---------------------------------------------------------------------------

/// What one refusal carries: the EC2 error code and the sentence worded with it.
type Refusal = (&'static str, String);

/// One row of the provision table: the case, the range it names, and what the provision
/// answers once the authorization document is given — `None` where it is served.
type ProvisionRow = (&'static str, &'static str, Option<Refusal>);

/// One row of the Fleet table: the case, the allocation strategy it names (`None` where it
/// names none), and what the create answers once the matching criteria are given — `None`
/// where it is served.
type FleetRow = (&'static str, Option<&'static str>, Option<Refusal>);

/// The form fields of one `ProvisionByoipCidr` call for `cidr`, carrying the authorization
/// document when `authorized`.
fn provision_params(cidr: &str, authorized: bool) -> Params {
    let mut params = vec![p("Cidr", cidr)];
    if authorized {
        params.push(p(
            "CidrAuthorizationContext.Message",
            "1|aws|000000000000|81.2.69.0/24|20991231|SHA256|RSAPSS",
        ));
        params.push(p(
            "CidrAuthorizationContext.Signature",
            "signed-authorization-message",
        ));
    }
    params
}

/// RULE for `ProvisionByoipCidr`: the authorization document the provision requires is a
/// PARAMETER, so a request that left it out is told so before the range it did send is
/// judged — and with the document in hand the range's own value decides the answer.
///
/// The rows span the ways the range can be at fault — a range outside public space, one more
/// specific than the family admits, a value that is no address range at all, and a range
/// there is nothing wrong with — each asked without the document and with it, so the first
/// column shows the order and the second shows that the order did not swallow the range's
/// own refusal.
#[tokio::test]
async fn the_authorization_document_is_required_before_the_range_is_judged() {
    let router = emulator();

    // (case, the range, the code and message it is answered WITH the document)
    let rows: [ProvisionRow; 4] = [
        (
            "a range outside public space",
            "203.0.113.0/24",
            Some((
                "InvalidParameterValue",
                "Value (203.0.113.0/24) for parameter cidr is invalid. Only a public address \
                 range can be provisioned."
                    .to_string(),
            )),
        ),
        (
            "a range more specific than the family admits",
            "81.2.69.0/25",
            Some((
                "InvalidParameterValue",
                "Value (81.2.69.0/25) for parameter cidr is invalid. The most specific prefix \
                 that can be provisioned is /24."
                    .to_string(),
            )),
        ),
        (
            "a value that is no address range at all",
            "not-a-cidr",
            Some((
                "InvalidParameterValue",
                "The CIDR (not-a-cidr) is not valid.".to_string(),
            )),
        ),
        // A range there is nothing wrong with: without the document it is still refused for
        // the parameter it left out, and with it the provision is served.
        ("a public range", "81.2.69.0/24", None),
    ];

    for (case, cidr, with_document) in rows {
        assert_refusal(
            &format!("{case}, with no authorization document"),
            &router,
            "ProvisionByoipCidr",
            &provision_params(cidr, false),
            ec2::state::MISSING_PARAMETER,
            "You must provide a CidrAuthorizationContext parameter.",
        )
        .await;
        match with_document {
            Some((code, message)) => {
                assert_refusal(
                    &format!("{case}, with the authorization document"),
                    &router,
                    "ProvisionByoipCidr",
                    &provision_params(cidr, true),
                    code,
                    &message,
                )
                .await;
            }
            None => {
                let body = ok(
                    &format!("{case}, with the authorization document"),
                    &router,
                    "ProvisionByoipCidr",
                    &provision_params(cidr, true),
                )
                .await;
                assert_eq!(
                    element(&body, "cidr").as_deref(),
                    Some(cidr),
                    "case {case:?}: the range is brought as the request named it, got {body}"
                );
            }
        }
    }

    // A document missing either half is as absent as none at all, reported with the half it
    // left out.
    for member in ["Message", "Signature"] {
        let case = format!("an authorization document with no {member}");
        let mut params = provision_params("81.2.69.0/24", true);
        for parameter in params.iter_mut() {
            if parameter.0 == format!("CidrAuthorizationContext.{member}") {
                parameter.1 = String::new();
            }
        }
        let lower = format!(
            "{}{}",
            member[..1].to_lowercase(),
            &member[1..]
        );
        assert_refusal(
            &case,
            &router,
            "ProvisionByoipCidr",
            &params,
            ec2::state::MISSING_PARAMETER,
            &format!("The request must contain the parameter cidrAuthorizationContext.{lower}"),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// CreateCapacityReservationFleet: the parameter left out before a value
// ---------------------------------------------------------------------------

/// The form fields of one `CreateCapacityReservationFleet` call: one instance type
/// specification, plus the allocation strategy and the matching criteria the case names.
fn fleet_params(
    zone: &str,
    allocation_strategy: Option<&str>,
    instance_match_criteria: Option<&str>,
) -> Params {
    let mut params = vec![
        p("TotalTargetCapacity", "2"),
        p("InstanceTypeSpecification.1.InstanceType", "m5.large"),
        p("InstanceTypeSpecification.1.InstancePlatform", "Linux/UNIX"),
        p("InstanceTypeSpecification.1.AvailabilityZone", zone),
    ];
    if let Some(strategy) = allocation_strategy {
        params.push(p("AllocationStrategy", strategy));
    }
    if let Some(criteria) = instance_match_criteria {
        params.push(p("InstanceMatchCriteria", criteria));
    }
    params
}

/// RULE for `CreateCapacityReservationFleet`: the matching criteria every Capacity
/// Reservation of the Fleet inherits is a parameter of the create, so a request that left it
/// out is told so before the VALUE of any other parameter is judged — and once it is given,
/// the other parameter's value decides the answer.
///
/// The rows span the allocation strategy's value domain: the one value it publishes, a value
/// outside it, and the request that names no strategy at all — each asked with the matching
/// criteria and without it.
#[tokio::test]
async fn the_missing_matching_criteria_is_answered_before_the_allocation_strategy() {
    let router = emulator();
    let zones = ok("zones", &router, "DescribeAvailabilityZones", &[]).await;
    let zone = element(&zones, "zoneName")
        .unwrap_or_else(|| panic!("the Region must report Availability Zones, got {zones}"));
    // The one value each of the two members publishes, read off the modelled catalogs rather
    // than written down here.
    let criteria = ec2::ops::types::FLEET_INSTANCE_MATCH_CRITERIA_VALUES[0];
    let strategy = ec2::state::FLEET_ALLOCATION_STRATEGY_PRIORITIZED;

    // (case, the allocation strategy, what the create answers once the criteria are given)
    let rows: [FleetRow; 3] = [
        ("no allocation strategy at all", None, None),
        ("the strategy the member publishes", Some(strategy), None),
        (
            "a strategy outside the published value",
            Some("not-a-strategy"),
            Some((
                "InvalidParameterValue",
                format!(
                    "Invalid value 'not-a-strategy' for allocationStrategy. Valid values: \
                     {strategy}"
                ),
            )),
        ),
    ];

    for (case, allocation_strategy, with_criteria) in rows {
        assert_refusal(
            &format!("{case}, with no matching criteria"),
            &router,
            "CreateCapacityReservationFleet",
            &fleet_params(&zone, allocation_strategy, None),
            ec2::state::MISSING_PARAMETER,
            "The following parameter is missing: instanceMatchCriteria",
        )
        .await;
        let params = fleet_params(&zone, allocation_strategy, Some(criteria));
        match with_criteria {
            Some((code, message)) => {
                assert_refusal(
                    &format!("{case}, with the matching criteria"),
                    &router,
                    "CreateCapacityReservationFleet",
                    &params,
                    code,
                    &message,
                )
                .await;
            }
            None => {
                let body = ok(
                    &format!("{case}, with the matching criteria"),
                    &router,
                    "CreateCapacityReservationFleet",
                    &params,
                )
                .await;
                assert!(
                    element(&body, "capacityReservationFleetId").is_some(),
                    "case {case:?}: the create must report a Fleet id, got {body}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RequestSpotFleet: the fleet's own role before what it would launch
// ---------------------------------------------------------------------------

/// One `SpotFleetRequestConfig.<member>` parameter.
fn c(member: &str, value: impl Into<String>) -> (String, String) {
    p(&format!("SpotFleetRequestConfig.{member}"), value)
}

/// RULE for `RequestSpotFleet`: the role the fleet would act under is read before what it
/// would launch, and a member of the configuration the service cannot act on is refused in
/// the configuration's own class — naming the member rather than quoting the value.
///
/// Every row names an AMI the account cannot resolve beside the role, so a row answered for
/// the role shows the order. The rows differ along the way the role is not one the fleet
/// could act under: a value that is no ARN, an ARN of another service, an ARN carrying a
/// Region (an IAM ARN is partition-global), an ARN whose resource is an instance profile
/// rather than a role, and an ARN with no resource name at all. The control row carries a
/// role ARN the service can read, so the launch specification's own refusal comes through.
#[tokio::test]
async fn a_configuration_member_the_service_cannot_act_on_names_itself() {
    let router = emulator();
    let unknown_image = "ami-0d1bc08881a62440e";

    // (case, the fleet role)
    let unusable: [(&str, &str); 5] = [
        ("a value that is no ARN", "aws-ec2-spot-fleet-tagging-role"),
        (
            "an ARN of another service",
            "arn:aws:ec2:us-west-2:111122223333:role/spot-fleet",
        ),
        (
            "an IAM ARN carrying a Region",
            "arn:aws:iam:us-west-2:111122223333:role/spot-fleet",
        ),
        (
            "an instance profile rather than a role",
            "arn:aws:iam::111122223333:instance-profile/spot-fleet",
        ),
        (
            "an ARN with no resource name",
            "arn:aws:iam::111122223333:role/",
        ),
    ];

    for (case, role) in unusable {
        assert_refusal(
            case,
            &router,
            "RequestSpotFleet",
            &[
                c("IamFleetRole", role),
                c("TargetCapacity", "1"),
                c("LaunchSpecifications.1.ImageId", unknown_image),
            ],
            "InvalidSpotFleetRequestConfig",
            "Parameter: SpotFleetRequestConfig.IamFleetRole is invalid.",
        )
        .await;
    }

    // The control: a role ARN the service can read lets the launch specification's own
    // refusal through, so the order above is an order and not a blanket refusal.
    assert_refusal(
        "a readable role ARN beside an AMI the account cannot resolve",
        &router,
        "RequestSpotFleet",
        &[
            c(
                "IamFleetRole",
                "arn:aws:iam::111122223333:role/aws-ec2-spot-fleet-tagging-role",
            ),
            c("TargetCapacity", "1"),
            c("LaunchSpecifications.1.ImageId", unknown_image),
        ],
        "InvalidAMIID.NotFound",
        &format!("The image id '[{unknown_image}]' does not exist"),
    )
    .await;
}
