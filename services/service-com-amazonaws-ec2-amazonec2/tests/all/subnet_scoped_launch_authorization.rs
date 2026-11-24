// SPDX-License-Identifier: CC0-1.0

//! WHICH SUBNET a caller may launch into — the resource-level half of the permission probe
//! `DryRun` is, driven end to end over the wire EC2 speaks (`ec2Query`) with the role, its
//! policy and its session held by the IAM and STS services mounted on the SAME engine, so
//! the document under test is the one `iam:PutRolePolicy` stored and the principal a refusal
//! blames is the one `sts:AssumeRole` minted.
//!
//! THE RULE UNDER TEST: `DryRun` "checks whether you have the required permissions for the
//! action, without actually making the request… If you have the required permissions, the
//! error response is DryRunOperation. Otherwise, it is UnauthorizedOperation"
//! (`RunInstancesRequest$DryRun`), so EC2 takes the WHOLE IAM decision before it answers —
//! over the resources the request addresses AND over the condition keys the request's own
//! parameters answer, of which the subnet a launch names is one (`ec2:Subnet`, typed ARN by
//! the IAM Service Authorization Reference's "Condition keys for Amazon EC2"). A probe the
//! documents in force do not permit is refused HTTP 403 `UnauthorizedOperation` with the
//! fixed sentence that error class carries, and only a permitted one is answered HTTP 412
//! `DryRunOperation`; neither launches anything.
//!
//! The rows span what the rule generalises over: how the document scopes the launch (by the
//! subnet's resource ARN, by the `ec2:Subnet` condition key under an ARN operator and under
//! a string one, over every subnet of the account, by an explicit `Deny` of one subnet's ARN
//! and by an explicit `Deny` keyed on the condition), which subnet the launch names, and
//! whether the request carries `DryRun` at all. The condition keys a request answers for the
//! resources it names are a catalog of this crate's own (the service module
//! `request_conditions`), so both of its entries are driven: the subnet a launch names and the
//! VPC a security group create names, the second through its own operation.

use smithy_server_runtime::MultiServiceRouter;

use super::account_wide_read_authorization::{
    assert_read, ec2_as, element, emulator, refusal_message, statement, Expect, ACCOUNT_KEY_ID,
    REGION,
};
use super::denied_launch_probe_encoded_message::{assumed, role_with, sentence_and_blob};
use super::ec2_query::{element as opt_element, elements, error_code, error_message};

/// EC2's error code for a caller that may not perform the action it asked for.
const UNAUTHORIZED_OPERATION: &str = "UnauthorizedOperation";

/// EC2's error code for a `DryRun` whose caller DOES hold the action, and the sentence it
/// carries.
const DRY_RUN_OPERATION: &str = "DryRunOperation";
const DRY_RUN_MESSAGE: &str = "Request would have succeeded, but DryRun flag is set.";

/// The sentence EC2 opens an `UnauthorizedOperation` refusal with.
const NOT_AUTHORIZED_SENTENCE: &str = "You are not authorized to perform this operation.";

/// The IAM action a launch is judged by.
const LAUNCH_ACTION: &str = "ec2:RunInstances";

/// The IAM action a security group create is judged by.
const CREATE_GROUP_ACTION: &str = "ec2:CreateSecurityGroup";

/// The fixtures every row is driven against: an image a launch can name, one VPC with two
/// subnets — the subnet a scoped document admits and another it does not — and a SECOND VPC,
/// which is to the group create what the second subnet is to the launch.
struct Fixtures {
    image_id: String,
    admitted_subnet: String,
    other_subnet: String,
    admitted_vpc: String,
    other_vpc: String,
}

impl Fixtures {
    /// The ARN of the subnet a scoped document admits.
    fn admitted_subnet_arn(&self) -> String {
        subnet_arn(&self.admitted_subnet)
    }

    /// The ARN of the subnet it does not.
    fn other_subnet_arn(&self) -> String {
        subnet_arn(&self.other_subnet)
    }
}

/// The ARN of the subnet `id`, as this emulator's one ARN builder renders it.
fn subnet_arn(id: &str) -> String {
    ::engine::emulator_arn("ec2", REGION, &format!("subnet/{id}"))
}

/// The ARN of the VPC `id`.
fn vpc_arn(id: &str) -> String {
    ::engine::emulator_arn("ec2", REGION, &format!("vpc/{id}"))
}

/// The `instance` resource a launch is judged on: the type with every id, the request naming
/// no instance that exists yet.
fn instance_resource() -> String {
    ::engine::emulator_arn("ec2", REGION, "instance/*")
}

/// Every subnet of the account, as a resource-scoped statement names them.
fn every_subnet() -> String {
    ::engine::emulator_arn("ec2", REGION, "subnet/*")
}

/// The estate the rows are driven against, provisioned through the account's own published
/// credential and read back over the wire — an image the account can resolve, two subnets of
/// one VPC and a second VPC.
async fn fixtures(router: &MultiServiceRouter) -> Fixtures {
    let (status, images) = ec2_as(router, ACCOUNT_KEY_ID, "DescribeImages", &[]).await;
    assert_eq!(
        status, 200,
        "the published images must be readable, got {status} {images}"
    );
    let image_id = elements(&images, "imageId")
        .into_iter()
        .next()
        .expect("a fresh account publishes at least one image");

    let admitted_vpc = vpc_of(router, "the launch network", "10.90.0.0/16").await;
    let other_vpc = vpc_of(router, "another network", "10.91.0.0/16").await;
    let admitted_subnet =
        subnet_of(router, "the admitted subnet", &admitted_vpc, "10.90.1.0/24").await;
    let other_subnet = subnet_of(router, "the other subnet", &admitted_vpc, "10.90.2.0/24").await;
    Fixtures {
        image_id,
        admitted_subnet,
        other_subnet,
        admitted_vpc,
        other_vpc,
    }
}

/// Create a VPC over `cidr_block` and answer its id.
async fn vpc_of(router: &MultiServiceRouter, case: &str, cidr_block: &str) -> String {
    let (status, created) = ec2_as(
        router,
        ACCOUNT_KEY_ID,
        "CreateVpc",
        &[("CidrBlock", cidr_block)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the VPC must be created, got {status} {created}"
    );
    element(case, &created, "vpcId")
}

/// Create a subnet of `vpc_id` over `cidr_block` and answer its id.
async fn subnet_of(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    cidr_block: &str,
) -> String {
    let (status, created) = ec2_as(
        router,
        ACCOUNT_KEY_ID,
        "CreateSubnet",
        &[("VpcId", vpc_id), ("CidrBlock", cidr_block)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the subnet must be created, got {status} {created}"
    );
    element(case, &created, "subnetId")
}

/// A one-statement document: `effect` on `action` over every `resources` entry, under
/// `condition` when the row states one (written as the `Condition` element's own JSON).
fn scoped_statement(
    effect: &str,
    action: &str,
    resources: &[String],
    condition: Option<&str>,
) -> String {
    let resources = resources
        .iter()
        .map(|resource| format!("\"{resource}\""))
        .collect::<Vec<_>>()
        .join(",");
    let condition = condition
        .map(|condition| format!(",\"Condition\":{condition}"))
        .unwrap_or_default();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"{effect}\",\
         \"Action\":\"{action}\",\"Resource\":[{resources}]{condition}}}]}}"
    )
}

/// The two statements of a document that allows everything of this service and denies one
/// thing: what a `Deny`-written scope looks like.
fn allow_everything_but(denial: &str) -> String {
    let allow = "{\"Effect\":\"Allow\",\"Action\":\"ec2:*\",\"Resource\":\"*\"}";
    format!("{{\"Version\":\"2012-10-17\",\"Statement\":[{allow},{denial}]}}")
}

/// What a row expects one probe to answer.
#[derive(Debug, Clone)]
enum Probe {
    /// HTTP 412 `DryRunOperation`: the documents in force permit the launch.
    WouldSucceed,
    /// HTTP 403 `UnauthorizedOperation` on the resource the decision was taken on, with the
    /// clause the rule that withheld the grant states.
    Refused { resource: String, reason: String },
}

/// The clause a refusal reached by the absence of any allow closes with, read from the
/// shared wording every service that reaches that decision states.
fn no_allow(action: &str) -> String {
    service_common::iam_authorization::no_identity_policy_allows(action)
}

/// The clause a refusal reached by an explicit `Deny` in an identity-based document closes
/// with, read from that same shared wording.
fn explicit_deny() -> String {
    service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
}

/// The form fields of a launch into `subnet_id`, with `DryRun` set when `dry_run`.
fn launch_params<'a>(
    image_id: &'a str,
    subnet_id: &'a str,
    dry_run: bool,
) -> Vec<(&'a str, &'a str)> {
    let mut params = vec![
        ("ImageId", image_id),
        ("MinCount", "1"),
        ("MaxCount", "1"),
        ("SubnetId", subnet_id),
    ];
    if dry_run {
        params.push(("DryRun", "true"));
    }
    params
}

/// Assert that a `DryRun` launch into `subnet_id` by `akid` answered what `expected`
/// requires, and that it launched nothing either way.
async fn assert_probe(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    principal: &str,
    image_id: &str,
    subnet_id: &str,
    expected: &Probe,
) {
    let (status, body) = ec2_as(
        router,
        akid,
        "RunInstances",
        &launch_params(image_id, subnet_id, true),
    )
    .await;
    match expected {
        Probe::WouldSucceed => {
            let message = error_message(&body);
            assert_eq!(
                (status, error_code(&body).as_str(), message.as_str()),
                (412, DRY_RUN_OPERATION, DRY_RUN_MESSAGE),
                "case {case:?}: expected the dry-run verdict, got {status} {body}"
            );
        }
        Probe::Refused { resource, reason } => {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (403, UNAUTHORIZED_OPERATION),
                "case {case:?}: expected the unauthorized refusal, got {status} {body}"
            );
            // A refusal of a launch hands the caller an encoded authorization failure
            // message, so the sentence under test is what precedes the blob.
            let (sentence, _) = sentence_and_blob(case, &error_message(&body));
            assert_eq!(
                sentence,
                format!(
                    "{NOT_AUTHORIZED_SENTENCE} User: {principal} is not authorized to \
                     perform: {LAUNCH_ACTION} on resource: {resource} {reason}."
                ),
                "case {case:?}: expected IAM's refusal wording, got {body}"
            );
        }
    }
    let (status, instances) = ec2_as(router, ACCOUNT_KEY_ID, "DescribeInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read back must be served, got {status} {instances}"
    );
    assert_eq!(
        opt_element(&instances, "instanceId"),
        None,
        "case {case:?}: a dry run must launch nothing, got {instances}"
    );
}

/// One row: the document in force on the role, and what a probe into each of the two subnets
/// must answer.
struct Row {
    case: &'static str,
    role: &'static str,
    document: String,
    into_admitted: Probe,
    into_other: Probe,
}

/// RULE: a `DryRun` launch is answered by the IAM decision about it — taken over the
/// resources the request addresses and the condition keys its own parameters answer, the
/// subnet it names included — so a document that admits one subnet answers `DryRunOperation`
/// for a launch into that subnet and `UnauthorizedOperation` for one into any other,
/// whichever way the document is written.
#[tokio::test]
async fn a_launch_probe_is_decided_by_the_subnet_the_policy_admits() {
    let router = emulator();
    let fixtures = fixtures(&router).await;
    let admitted = fixtures.admitted_subnet_arn();
    let other = fixtures.other_subnet_arn();
    let rows = vec![
        Row {
            case: "a grant scoped to one subnet by its resource ARN",
            role: "subnet-arn-scoped-role",
            document: scoped_statement(
                "Allow",
                LAUNCH_ACTION,
                &[instance_resource(), admitted.clone()],
                None,
            ),
            into_admitted: Probe::WouldSucceed,
            // The grant covers no other subnet, and the subnet is the resource the request
            // addressed, so that is the resource the refusal names.
            into_other: Probe::Refused {
                resource: other.clone(),
                reason: no_allow(LAUNCH_ACTION),
            },
        },
        Row {
            case: "a grant scoped to one subnet by the ec2:Subnet condition key",
            role: "subnet-condition-scoped-role",
            document: scoped_statement(
                "Allow",
                LAUNCH_ACTION,
                &[instance_resource(), every_subnet()],
                Some(&format!(
                    "{{\"ArnEquals\":{{\"ec2:Subnet\":\"{admitted}\"}}}}"
                )),
            ),
            into_admitted: Probe::WouldSucceed,
            // A launch is judged on the resource it CREATES first, and that is where a
            // clause the request does not satisfy withholds the grant.
            into_other: Probe::Refused {
                resource: instance_resource(),
                reason: no_allow(LAUNCH_ACTION),
            },
        },
        Row {
            case: "the same condition under IAM's string operator",
            role: "subnet-string-condition-role",
            document: scoped_statement(
                "Allow",
                LAUNCH_ACTION,
                &[instance_resource(), every_subnet()],
                Some(&format!(
                    "{{\"StringEquals\":{{\"ec2:Subnet\":\"{admitted}\"}}}}"
                )),
            ),
            into_admitted: Probe::WouldSucceed,
            into_other: Probe::Refused {
                resource: instance_resource(),
                reason: no_allow(LAUNCH_ACTION),
            },
        },
        Row {
            case: "a grant over every subnet of the account",
            role: "every-subnet-role",
            document: scoped_statement(
                "Allow",
                LAUNCH_ACTION,
                &[instance_resource(), every_subnet()],
                None,
            ),
            into_admitted: Probe::WouldSucceed,
            into_other: Probe::WouldSucceed,
        },
        Row {
            case: "an explicit deny of the launch on one subnet's ARN",
            role: "subnet-arn-denied-role",
            document: allow_everything_but(&format!(
                "{{\"Effect\":\"Deny\",\"Action\":\"{LAUNCH_ACTION}\",\"Resource\":\"{other}\"}}"
            )),
            into_admitted: Probe::WouldSucceed,
            into_other: Probe::Refused {
                resource: other.clone(),
                reason: explicit_deny(),
            },
        },
        Row {
            case: "an explicit deny keyed on the ec2:Subnet condition",
            role: "subnet-condition-denied-role",
            document: allow_everything_but(&format!(
                "{{\"Effect\":\"Deny\",\"Action\":\"{LAUNCH_ACTION}\",\"Resource\":\"*\",\
                 \"Condition\":{{\"ArnEquals\":{{\"ec2:Subnet\":\"{other}\"}}}}}}"
            )),
            // The clause is DECIDED against the subnet this launch names, so the deny is
            // about the other subnet alone rather than about every launch.
            into_admitted: Probe::WouldSucceed,
            into_other: Probe::Refused {
                resource: instance_resource(),
                reason: explicit_deny(),
            },
        },
    ];

    for row in &rows {
        role_with(&router, row.case, row.role, &row.document).await;
        let (akid, principal, _) =
            assumed(&router, row.case, row.role, "scoped-launcher", None).await;
        assert_probe(
            &router,
            row.case,
            &akid,
            &principal,
            &fixtures.image_id,
            &fixtures.admitted_subnet,
            &row.into_admitted,
        )
        .await;
        assert_probe(
            &router,
            row.case,
            &akid,
            &principal,
            &fixtures.image_id,
            &fixtures.other_subnet,
            &row.into_other,
        )
        .await;
    }
}

/// RULE: the decision is the CALLER's, not the flag's — a launch that does NOT carry `DryRun`
/// is decided by the same documents, so the scoped caller launches into the subnet its policy
/// admits (and the instance is there to read back) and is refused on any other, with nothing
/// created for the refused one.
#[tokio::test]
async fn the_same_decision_governs_a_launch_that_is_not_a_dry_run() {
    let router = emulator();
    let fixtures = fixtures(&router).await;
    let case = "a real launch by a subnet-scoped caller";
    let role = "subnet-scoped-launcher-role";
    role_with(
        &router,
        case,
        role,
        &scoped_statement(
            "Allow",
            LAUNCH_ACTION,
            &[instance_resource(), fixtures.admitted_subnet_arn()],
            None,
        ),
    )
    .await;
    let (akid, principal, _) = assumed(&router, case, role, "real-launcher", None).await;

    // The subnet the document does NOT admit: refused, and nothing is created.
    let (status, refused) = ec2_as(
        &router,
        &akid,
        "RunInstances",
        &launch_params(&fixtures.image_id, &fixtures.other_subnet, false),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (403, UNAUTHORIZED_OPERATION),
        "case {case:?}: a launch outside the admitted subnet must be refused, got {status} \
         {refused}"
    );
    let (sentence, _) = sentence_and_blob(case, &error_message(&refused));
    assert_eq!(
        sentence,
        format!(
            "{NOT_AUTHORIZED_SENTENCE} User: {principal} is not authorized to perform: \
             {LAUNCH_ACTION} on resource: {} {}.",
            fixtures.other_subnet_arn(),
            no_allow(LAUNCH_ACTION)
        ),
        "case {case:?}: expected IAM's refusal wording, got {refused}"
    );
    let (status, instances) = ec2_as(&router, ACCOUNT_KEY_ID, "DescribeInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read back must be served, got {status} {instances}"
    );
    assert_eq!(
        opt_element(&instances, "instanceId"),
        None,
        "case {case:?}: a refused launch must create nothing, got {instances}"
    );

    // The subnet it does admit: served, and the instance it minted is what the account holds.
    let (status, launched) = ec2_as(
        &router,
        &akid,
        "RunInstances",
        &launch_params(&fixtures.image_id, &fixtures.admitted_subnet, false),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the admitted launch must be served, got {status} {launched}"
    );
    let instance_id = element(case, &launched, "instanceId");
    let (status, instances) = ec2_as(
        &router,
        ACCOUNT_KEY_ID,
        "DescribeInstances",
        &[("InstanceId.1", &instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read back must be served, got {status} {instances}"
    );
    let reported = elements(&instances, "subnetId");
    assert!(
        !reported.is_empty() && reported.iter().all(|id| *id == fixtures.admitted_subnet),
        "case {case:?}: expected the launched instance in the admitted subnet {:?}, got \
         {reported:?}",
        fixtures.admitted_subnet
    );
}

/// RULE (the other entry of the same catalog): the VPC a request names answers `ec2:Vpc` the
/// way the subnet it names answers `ec2:Subnet`, so a create scoped by that key is decided —
/// and its probe reports the two sides of the decision through the same two codes.
#[tokio::test]
async fn a_group_create_probe_is_decided_by_the_vpc_the_policy_admits() {
    let router = emulator();
    let fixtures = fixtures(&router).await;
    let case = "a create scoped by the ec2:Vpc condition key";
    let role = "vpc-condition-scoped-role";
    role_with(
        &router,
        case,
        role,
        &scoped_statement(
            "Allow",
            CREATE_GROUP_ACTION,
            &["*".to_string()],
            Some(&format!(
                "{{\"ArnEquals\":{{\"ec2:Vpc\":\"{}\"}}}}",
                vpc_arn(&fixtures.admitted_vpc)
            )),
        ),
    )
    .await;
    let (akid, principal, _) = assumed(&router, case, role, "scoped-group-maker", None).await;

    for (vpc_id, admitted) in [
        (&fixtures.admitted_vpc, true),
        (&fixtures.other_vpc, false),
    ] {
        let (status, body) = ec2_as(
            &router,
            &akid,
            "CreateSecurityGroup",
            &[
                ("GroupName", "probed-group"),
                ("GroupDescription", "a probed group"),
                ("VpcId", vpc_id),
                ("DryRun", "true"),
            ],
        )
        .await;
        if admitted {
            assert_eq!(
                (status, error_code(&body).as_str(), error_message(&body).as_str()),
                (412, DRY_RUN_OPERATION, DRY_RUN_MESSAGE),
                "case {case:?} / the admitted VPC: expected the dry-run verdict, got {status} \
                 {body}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&body).as_str()),
            (403, UNAUTHORIZED_OPERATION),
            "case {case:?} / another VPC: expected the unauthorized refusal, got {status} {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "{NOT_AUTHORIZED_SENTENCE} User: {principal} is not authorized to perform: \
                 {CREATE_GROUP_ACTION} on resource: {} {}.",
                vpc_arn(&fixtures.other_vpc),
                no_allow(CREATE_GROUP_ACTION)
            ),
            "case {case:?} / another VPC: expected IAM's refusal wording, got {body}"
        );
    }

    // Nothing was created either way: a probe is an authorization question.
    let (status, groups) = ec2_as(&router, ACCOUNT_KEY_ID, "DescribeSecurityGroups", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read back must be served, got {status} {groups}"
    );
    assert!(
        !groups.contains("probed-group"),
        "case {case:?}: a probed create must create nothing, got {groups}"
    );
}

/// RULE: an action that supports no resource-level permissions carries none of those keys —
/// "the Amazon EC2 API actions that begin with `Describe` do not support resource-level
/// permissions" — so a grant of such an action conditioned on the VPC the request names is
/// decided by nothing the request carries and grants it nowhere, while the same grant without
/// the clause serves it.
#[tokio::test]
async fn a_read_answers_no_condition_key_about_the_resources_it_names() {
    let router = emulator();
    let fixtures = fixtures(&router).await;

    let case = "a read whose grant is conditioned on the VPC it names";
    let role = "conditioned-reader-role";
    role_with(
        &router,
        case,
        role,
        &scoped_statement(
            "Allow",
            "ec2:DescribeVpcs",
            &["*".to_string()],
            Some(&format!(
                "{{\"ArnEquals\":{{\"ec2:Vpc\":\"{}\"}}}}",
                vpc_arn(&fixtures.admitted_vpc)
            )),
        ),
    )
    .await;
    let (akid, principal, _) = assumed(&router, case, role, "conditioned-reader", None).await;
    let (status, refused) = ec2_as(
        &router,
        &akid,
        "DescribeVpcs",
        &[("VpcId.1", &fixtures.admitted_vpc)],
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (403, UNAUTHORIZED_OPERATION),
        "case {case:?}: the read must be refused, got {status} {refused}"
    );
    assert!(
        !refused.contains(&fixtures.admitted_vpc),
        "case {case:?}: a refused read discloses no VPC of the account, got {refused}"
    );
    assert_eq!(
        error_message(&refused),
        refusal_message(&principal, "ec2:DescribeVpcs", Expect::NoAllow),
        "case {case:?}: expected IAM's refusal wording, got {refused}"
    );

    // The control: the same grant without the clause serves the very same read.
    let case = "the same read under an unconditioned grant";
    let role = "plain-reader-role";
    role_with(
        &router,
        case,
        role,
        &statement("Allow", "ec2:DescribeVpcs", "*"),
    )
    .await;
    let (akid, principal, _) = assumed(&router, case, role, "plain-reader", None).await;
    assert_read(
        &router,
        case,
        &akid,
        &principal,
        "DescribeVpcs",
        Expect::Served,
    )
    .await;
}
