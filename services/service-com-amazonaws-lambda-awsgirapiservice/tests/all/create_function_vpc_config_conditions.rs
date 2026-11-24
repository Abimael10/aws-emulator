// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of the call that names a function's VPC CONFIGURATION, over the
//! restJson1 wire.
//!
//! RULE under test: a request whose input binds the VPC configuration a function is attached
//! to — `CreateFunction`'s optional `VpcConfig`, whose `SubnetIds` and `SecurityGroupIds`
//! lists carry the placement it asks for
//! (`smithy_query(query="shape", shape="VpcConfig")`) — is authorized with that placement
//! published as this service's own request condition keys: `lambda:SubnetIds` and
//! `lambda:SecurityGroupIds` for the ids the request names, and `lambda:VpcIds` for the VPC
//! those subnets belong to as Amazon EC2 records them. Each key is multi-valued (one value per
//! id named) and MODELLED whether or not the request asked for an attachment at all. So a
//! statement whose `Condition` is written over one of those keys decides the call on the
//! placement it asks for — including on the ABSENCE of a placement — and a grant narrowed to
//! approved security groups, subnets or VPC covers only the creates that ask for them; every
//! other caller is refused `AccessDeniedException` (HTTP 403) carrying the one sentence IAM
//! words an authorization failure with, BEFORE anything is created.
//!
//! The decision is the real one: the caller's role and its inline policy are made with IAM's
//! and STS's OWN operations over the same engine (`CreateRole`, `PutRolePolicy`, `AssumeRole`),
//! so the documents the gate reads are the ones those calls wrote, and the caller signs with
//! the access key id `AssumeRole` answered. Every VPC, subnet and security group a row names
//! was MINTED by Amazon EC2 itself on that same engine, so the VPC the gate resolves is the
//! account's own record of the subnets and not a value this suite passed off as one; and every
//! claim about what the store holds is read back through `GetFunction`, so an answer that was
//! never persisted — or a write that landed in spite of a refusal — fails here.
//!
//! The rows differ along what the rule generalises over: WHICH key the grant is conditioned on
//! (the groups, the subnets, the VPC), the CONDITION OPERATOR it is written under (IAM's two
//! set qualifiers, an unqualified equality, its `IfExists` spelling, the existence operator
//! asking for the key's absence, and no condition at all), how many ids the request names
//! under a key (two, one, none), and whether a named subnet resolves in EC2 at all.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

// The `awsQuery` form encoder, the XML element reader, the EC2 client over the router's own
// in-process callback and the network minter this target already carries: the IAM, STS and EC2
// fixtures below are written on those readings rather than on fresh copies of them.
use crate::capacity_provider_authorization::xml_element;
use crate::event_source_mapping_role_access::form_encode;
use crate::function_lifecycle::{package_bytes, zip_file, ACCOUNT, REGION};
use crate::vpc_attachment_interfaces::{ec2_client, network, Network};

/// Where `CreateFunction` is bound
/// (`smithy_query(query="operation", operation="CreateFunction")`: `POST
/// /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The access key id standing for the account's own published credential: neither STS nor IAM
/// holds a record for it, so it resolves to the account's own identity, which holds no IAM
/// policies — the position AWS account root credentials are in. The fixtures are built with it,
/// because this gate serves it.
const ACCOUNT_KEY_ID: &str = "AKIDTESTONLYACCOUNT1";

/// The name of the inline policy the caller's role carries. `iam:PutRolePolicy` REPLACES the
/// policy of that name, so one role can be re-judged row by row.
const POLICY_NAME: &str = "inline";

/// The name of the inline policy the EXECUTION role carries: attaching a function to a VPC is
/// done by Lambda AS that role, so it holds the network-interface grant that takes.
const EXECUTION_POLICY_NAME: &str = "vpc-access";

/// The session name the caller's role is assumed under.
const SESSION: &str = "probe";

/// The role the rows below sign as, and the execution role every function is created with.
const CALLER_ROLE: &str = "deployer";
const EXECUTION_ROLE: &str = "lambda-exec";

/// The service principal a function's execution role is assumed by.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The condition keys a call that names a VPC configuration publishes values for, as the IAM
/// Service Authorization Reference spells this service's own keys (`lambda:<MemberName>`), with
/// the VPC named after the id EC2 resolves for the configured subnets. Written out here rather
/// than taken from the crate, so the assertions are independent of the constants the
/// implementation carries.
const VPC_IDS_KEY: &str = "lambda:VpcIds";
const SUBNET_IDS_KEY: &str = "lambda:SubnetIds";
const SECURITY_GROUP_IDS_KEY: &str = "lambda:SecurityGroupIds";

/// A form-valid subnet id no account ever minted: the id form a `SubnetIds` member takes,
/// naming nothing — so the configuration names a subnet while no VPC resolves for it.
const UNKNOWN_SUBNET: &str = "subnet-0000000000bad0bad";

/// The wire name and status of the refusal, which no operation of this service models: the
/// service answers the AWS common `AccessDeniedException` at HTTP 403.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The action every row's grant is written for.
const ACTION: &str = "lambda:CreateFunction";

/// The ARN the account holds the role `role` under.
fn role_arn(role: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{role}")
}

/// The principal ARN the session assumed from `role` speaks for — what a refusal must name.
fn session_arn(role: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{SESSION}")
}

/// The ARN of the function `name`, as the model's `FunctionArn` pattern spells it — the
/// resource a call addressed at that function is judged against.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// What a dispatched Lambda request answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
    error_type: Option<String>,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn error_code(&self) -> String {
        self.error_type
            .clone()
            .or_else(|| {
                self.json()
                    .get("__type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| self.text())
    }

    /// The `Message` an error body carries — this service spells the member with a capital, as
    /// every error structure it models does.
    fn message(&self) -> String {
        let document = self.json();
        document
            .get("Message")
            .or_else(|| document.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Lambda beside the IAM, STS and Amazon EC2 services over ONE shared engine, which is what
/// `core/router` assembles, plus an EC2 client over the router's own in-process callback.
fn router_with_identity_and_ec2() -> (MultiServiceRouter, aws_sdk_ec2::Client) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Pinned off so the suite does not depend on how the emulator process was started; these
    // rows assert the authorization decision, not the emulator's SigV4 check.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let callback = EmbeddedEmulator::from_router(router.clone());
    let ec2 = ec2_client(&callback);
    (router, ec2)
}

/// One Lambda request signed with `access_key_id`.
async fn lambda(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/lambda/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        error_type: parts
            .headers
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body,
    }
}

/// One `awsQuery` call to IAM or STS, signed with the account's own credential.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.amazonaws.com/"))
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Put `document` on `role` under the policy name `policy`, through `iam:PutRolePolicy`, which
/// REPLACES the policy of that name — so the next request judged on that role sees exactly the
/// document this call wrote.
async fn given_role_policy(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    policy: &str,
    document: &Value,
) {
    let (status, body) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", role.to_string()),
            ("PolicyName", policy.to_string()),
            ("PolicyDocument", document.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy must succeed, got {status} {body}"
    );
}

/// Create the execution role every function below runs as: it admits the Lambda service
/// principal, and it carries the network-interface grant attaching a function to a VPC takes —
/// the calls Lambda makes AS that role, which a role short of them would have the create
/// refused for.
async fn given_execution_role(router: &MultiServiceRouter) {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": LAMBDA_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole {EXECUTION_ROLE:?} must succeed, got {status} {body}"
    );
    given_role_policy(
        router,
        "the execution role's network grant",
        EXECUTION_ROLE,
        EXECUTION_POLICY_NAME,
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": [
                    "ec2:CreateNetworkInterface",
                    "ec2:DescribeNetworkInterfaces",
                    "ec2:DeleteNetworkInterface",
                ],
                "Resource": "*",
            }],
        }),
    )
    .await;
}

/// Create the role `role` with a trust policy the account itself satisfies and assume it
/// through `sts:AssumeRole`: the answer is the access key id of the session, which is the
/// credential every row signs its own call with.
async fn given_assumed_role(router: &MultiServiceRouter, role: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole {role:?} must succeed, got {status} {body}"
    );
    let (status, body) = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            ("RoleArn", role_arn(role)),
            ("RoleSessionName", SESSION.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole on {role:?} must succeed, got {status} {body}"
    );
    xml_element(&body, "AccessKeyId")
        .unwrap_or_else(|| panic!("setup: AssumeRole must answer an AccessKeyId, got {body}"))
}

/// A SECOND security group in the VPC `vpc_id`, minted by Amazon EC2 itself: the group an
/// approved-group grant must NOT cover.
async fn given_security_group(ec2: &aws_sdk_ec2::Client, vpc_id: &str, name: &str) -> String {
    let created = ec2
        .create_security_group()
        .vpc_id(vpc_id)
        .group_name(name)
        .description("a group a VPC-attached function may be confined away from")
        .send()
        .await
        .unwrap_or_else(|error| panic!("setup: EC2 must create the group {name:?}: {error:?}"));
    created
        .group_id()
        .unwrap_or_else(|| panic!("setup: CreateSecurityGroup {name:?} must report the group"))
        .to_string()
}

/// The `VpcConfig` member a row's request carries: the subnets and the groups it names.
fn vpc_config(subnet_ids: &[&str], security_group_ids: &[&str]) -> Value {
    json!({ "SubnetIds": subnet_ids, "SecurityGroupIds": security_group_ids })
}

/// The `CreateFunction` body for `name`, carrying the `VpcConfig` the row asks for and no such
/// member at all when it asks for none.
fn create_body(name: &str, attachment: Option<&Value>) -> Value {
    let mut body = json!({
        "FunctionName": name,
        "Role": role_arn(EXECUTION_ROLE),
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Code": { "ZipFile": zip_file(&package_bytes(64)) },
    });
    if let Some(attachment) = attachment {
        body["VpcConfig"] = attachment.clone();
    }
    body
}

/// An identity-based document allowing [`ACTION`] on every resource under the `Condition` block
/// `condition` (no block at all for `Value::Null`).
fn grant(condition: Value) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Action": ACTION,
        "Resource": "*",
    });
    if !condition.is_null() {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// The sentence IAM refuses a call no readable identity-based document allows with, as
/// `service_common::iam_authorization` words it: the principal that signed, the action, the
/// resource and IAM's default-deny clause.
fn default_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// `GetFunction` with the account's own credential: the stored function, or `None` when this
/// account holds none under that name.
async fn stored_function(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let read = lambda(
        router,
        ACCOUNT_KEY_ID,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}"),
        &Value::Null,
    )
    .await;
    if read.status == 404 {
        return None;
    }
    assert_eq!(
        read.status,
        200,
        "GetFunction {name:?} as the account must be served, got {} {}",
        read.status,
        read.text()
    );
    Some(read.json())
}

/// One row: what the grant is conditioned on, the placement the create asks for, and whether
/// the grant covers it.
struct Row {
    case: &'static str,
    condition: Value,
    attachment: Option<Value>,
    served: bool,
}

/// RULE: `CreateFunction` publishes the placement its own `VpcConfig` asks for — the security
/// groups and subnets it names, and the VPC EC2 resolves for those subnets — as this service's
/// `lambda:SecurityGroupIds`, `lambda:SubnetIds` and `lambda:VpcIds` request condition keys, so
/// a grant conditioned on any of them serves only the creates whose placement satisfies the
/// condition and refuses every other one before anything is created.
#[tokio::test]
async fn a_create_is_decided_by_the_vpc_configuration_it_names() {
    let (router, ec2) = router_with_identity_and_ec2();
    given_execution_role(&router).await;
    let caller_key = given_assumed_role(&router, CALLER_ROLE).await;

    // Two networks EC2 minted: the approved one, whose two subnets carry the approved group and
    // one more group beside it, and another VPC entirely.
    let approved: Network = network(&ec2, "the approved network", 10, 2).await;
    let elsewhere: Network = network(&ec2, "another network", 11, 1).await;
    let other_group = given_security_group(&ec2, &approved.vpc_id, "fn-access-unapproved").await;
    let approved_group = approved.group_id.clone();
    let approved_subnets: Vec<&str> = approved
        .subnet_ids
        .iter()
        .map(String::as_str)
        .collect();
    let first_subnet = approved_subnets[0];
    let foreign_subnet = elsewhere.subnet_ids[0].as_str();

    let rows: Vec<Row> = vec![
        Row {
            case: "every group must be the approved one, and it is",
            condition: json!({
                "ForAllValues:StringEquals": { SECURITY_GROUP_IDS_KEY: [&approved_group] }
            }),
            attachment: Some(vpc_config(&approved_subnets, &[&approved_group])),
            served: true,
        },
        Row {
            // The recorded control: a create naming a group the grant does not approve.
            case: "every group must be the approved one, and another one is named",
            condition: json!({
                "ForAllValues:StringEquals": { SECURITY_GROUP_IDS_KEY: [&approved_group] }
            }),
            attachment: Some(vpc_config(&approved_subnets, &[&other_group])),
            served: false,
        },
        Row {
            case: "every group must be the approved one, and the approved one is named beside \
                   another",
            condition: json!({
                "ForAllValues:StringEquals": { SECURITY_GROUP_IDS_KEY: [&approved_group] }
            }),
            attachment: Some(vpc_config(&approved_subnets, &[&approved_group, &other_group])),
            served: false,
        },
        Row {
            // IAM's own empty-set rule: `ForAllValues:` "returns true if there are no request
            // values", which is decidable only because the key is MODELLED by this call.
            case: "every group must be the approved one, and the create asks for no attachment",
            condition: json!({
                "ForAllValues:StringEquals": { SECURITY_GROUP_IDS_KEY: [&approved_group] }
            }),
            attachment: None,
            served: true,
        },
        Row {
            case: "one group must be the approved one, which is named beside another",
            condition: json!({
                "ForAnyValue:StringEquals": { SECURITY_GROUP_IDS_KEY: [&approved_group] }
            }),
            attachment: Some(vpc_config(&approved_subnets, &[&other_group, &approved_group])),
            served: true,
        },
        Row {
            case: "one group must be the approved one, and only another is named",
            condition: json!({
                "ForAnyValue:StringEquals": { SECURITY_GROUP_IDS_KEY: [&approved_group] }
            }),
            attachment: Some(vpc_config(&approved_subnets, &[&other_group])),
            served: false,
        },
        Row {
            case: "an unqualified equality over the approved group, that one group named",
            condition: json!({ "StringEquals": { SECURITY_GROUP_IDS_KEY: &approved_group } }),
            attachment: Some(vpc_config(&approved_subnets, &[&approved_group])),
            served: true,
        },
        Row {
            case: "an unqualified equality over the approved group, another group named",
            condition: json!({ "StringEquals": { SECURITY_GROUP_IDS_KEY: &approved_group } }),
            attachment: Some(vpc_config(&approved_subnets, &[&other_group])),
            served: false,
        },
        Row {
            case: "an equality over the approved VPC, a subnet of it named",
            condition: json!({ "StringEquals": { VPC_IDS_KEY: &approved.vpc_id } }),
            attachment: Some(vpc_config(&[first_subnet], &[&approved_group])),
            served: true,
        },
        Row {
            case: "an equality over the approved VPC, a subnet of another VPC named",
            condition: json!({ "StringEquals": { VPC_IDS_KEY: &approved.vpc_id } }),
            attachment: Some(vpc_config(&[foreign_subnet], &[&approved_group])),
            served: false,
        },
        Row {
            // Nothing is published for a key on evidence this emulator does not have: a subnet
            // the account's inventory holds no record of resolves to no VPC, so the grant
            // narrowed to one cannot cover the create.
            case: "an equality over the approved VPC, a subnet EC2 holds no record of named",
            condition: json!({ "StringEquals": { VPC_IDS_KEY: &approved.vpc_id } }),
            attachment: Some(vpc_config(&[UNKNOWN_SUBNET], &[&approved_group])),
            served: false,
        },
        Row {
            case: "every subnet must be one of the approved ones, and both are",
            condition: json!({
                "ForAllValues:StringEquals": { SUBNET_IDS_KEY: &approved.subnet_ids }
            }),
            attachment: Some(vpc_config(&approved_subnets, &[&approved_group])),
            served: true,
        },
        Row {
            case: "every subnet must be one of the approved ones, and one outside them is named",
            condition: json!({
                "ForAllValues:StringEquals": { SUBNET_IDS_KEY: &approved.subnet_ids }
            }),
            attachment: Some(vpc_config(&[first_subnet, foreign_subnet], &[&approved_group])),
            served: false,
        },
        Row {
            case: "the existence operator asking for no attachment, and none is asked for",
            condition: json!({ "Null": { SECURITY_GROUP_IDS_KEY: "true" } }),
            attachment: None,
            served: true,
        },
        Row {
            case: "the existence operator asking for no attachment, and a group is named",
            condition: json!({ "Null": { SECURITY_GROUP_IDS_KEY: "true" } }),
            attachment: Some(vpc_config(&approved_subnets, &[&approved_group])),
            served: false,
        },
        Row {
            case: "the IfExists spelling of the group equality, no attachment asked for",
            condition: json!({
                "StringEqualsIfExists": { SECURITY_GROUP_IDS_KEY: &approved_group }
            }),
            attachment: None,
            served: true,
        },
        Row {
            case: "the IfExists spelling of the group equality, another group named",
            condition: json!({
                "StringEqualsIfExists": { SECURITY_GROUP_IDS_KEY: &approved_group }
            }),
            attachment: Some(vpc_config(&approved_subnets, &[&other_group])),
            served: false,
        },
        Row {
            case: "no condition at all, another group named",
            condition: Value::Null,
            attachment: Some(vpc_config(&approved_subnets, &[&other_group])),
            served: true,
        },
        Row {
            case: "no condition at all, no attachment asked for",
            condition: Value::Null,
            attachment: None,
            served: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        given_role_policy(
            &router,
            row.case,
            CALLER_ROLE,
            POLICY_NAME,
            &grant(row.condition.clone()),
        )
        .await;
        let name = format!("created-{index:02}");
        let answered = lambda(
            &router,
            &caller_key,
            "POST",
            FUNCTIONS_URI,
            &create_body(&name, row.attachment.as_ref()),
        )
        .await;

        if row.served {
            assert_eq!(
                answered.status,
                201,
                "case {:?}: the grant covers this create, so it must be served, got {} {}",
                row.case,
                answered.status,
                answered.text()
            );
            let stored = stored_function(&router, &name).await.unwrap_or_else(|| {
                panic!(
                    "case {:?}: a served create must leave the function readable",
                    row.case
                )
            });
            assert_eq!(
                stored["Configuration"]["FunctionArn"].as_str(),
                Some(function_arn(&name).as_str()),
                "case {:?}: expected the stored function {}, got {stored}",
                row.case,
                function_arn(&name)
            );
            // The placement the create asked for is the one the stored function carries, read
            // back through the configuration: an authorization gate that let the call through
            // must not have changed what it asks for.
            let reported = stored["Configuration"]["VpcConfig"].clone();
            match &row.attachment {
                None => assert!(
                    reported.get("SecurityGroupIds").is_none()
                        || reported["SecurityGroupIds"] == json!([]),
                    "case {:?}: a create asking for no attachment must store none, got \
                     {reported}",
                    row.case
                ),
                Some(attachment) => {
                    assert_eq!(
                        reported["SecurityGroupIds"], attachment["SecurityGroupIds"],
                        "case {:?}: expected the groups the create named, got {reported}",
                        row.case
                    );
                    assert_eq!(
                        reported["SubnetIds"], attachment["SubnetIds"],
                        "case {:?}: expected the subnets the create named, got {reported}",
                        row.case
                    );
                }
            }
            continue;
        }

        assert_eq!(
            answered.status, ACCESS_DENIED_STATUS,
            "case {:?}: the grant does not cover this create, so it must be refused, got {} {}",
            row.case,
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.error_code(),
            ACCESS_DENIED,
            "case {:?}: expected {ACCESS_DENIED}, got {}",
            row.case,
            answered.text()
        );
        assert_eq!(
            answered.message(),
            default_deny_message(&session_arn(CALLER_ROLE), ACTION, &function_arn(&name)),
            "case {:?}: expected IAM's default-deny sentence, got {:?}",
            row.case,
            answered.message()
        );
        assert!(
            stored_function(&router, &name).await.is_none(),
            "case {:?}: a refused create must leave nothing a later caller can read",
            row.case
        );
    }
}

/// RULE: an explicit `Deny` written over the same keys refuses the create whatever grant sits
/// beside it — the other direction of the same publication, and the shape a network-isolation
/// control is usually written in: allow the action broadly, deny every placement outside the
/// approved groups.
#[tokio::test]
async fn a_deny_over_the_published_keys_refuses_an_unapproved_placement() {
    let (router, ec2) = router_with_identity_and_ec2();
    given_execution_role(&router).await;
    let caller_key = given_assumed_role(&router, CALLER_ROLE).await;

    let approved: Network = network(&ec2, "the approved network", 12, 1).await;
    let other_group = given_security_group(&ec2, &approved.vpc_id, "fn-access-denied").await;
    let approved_group = approved.group_id.clone();
    let subnet = approved.subnet_ids[0].as_str();

    // A broad grant of the action, with every placement outside the approved group denied.
    given_role_policy(
        &router,
        "the deny beside the grant",
        CALLER_ROLE,
        POLICY_NAME,
        &json!({
            "Version": "2012-10-17",
            "Statement": [
                { "Effect": "Allow", "Action": ACTION, "Resource": "*" },
                {
                    "Effect": "Deny",
                    "Action": ACTION,
                    "Resource": "*",
                    "Condition": {
                        "ForAnyValue:StringNotEquals": {
                            SECURITY_GROUP_IDS_KEY: [&approved_group]
                        }
                    },
                },
            ],
        }),
    )
    .await;

    // (case, the groups the create names, whether it is served)
    let rows: Vec<(&str, Vec<&str>, bool)> = vec![
        ("the approved group alone", vec![approved_group.as_str()], true),
        ("another group", vec![other_group.as_str()], false),
        (
            "the approved group beside another",
            vec![approved_group.as_str(), other_group.as_str()],
            false,
        ),
        // Asked again, so a served create is not a one-off and the refusals above are decided
        // on the placement rather than on the order the rows ran in.
        ("the approved group alone, again", vec![approved_group.as_str()], true),
    ];

    for (index, (case, groups, served)) in rows.iter().enumerate() {
        let name = format!("denied-{index:02}");
        let answered = lambda(
            &router,
            &caller_key,
            "POST",
            FUNCTIONS_URI,
            &create_body(&name, Some(&vpc_config(&[subnet], groups))),
        )
        .await;

        if *served {
            assert_eq!(
                answered.status,
                201,
                "case {case:?}: no deny covers this create, so it must be served, got {} {}",
                answered.status,
                answered.text()
            );
            assert!(
                stored_function(&router, &name).await.is_some(),
                "case {case:?}: a served create must leave the function readable"
            );
            continue;
        }

        assert_eq!(
            answered.status, ACCESS_DENIED_STATUS,
            "case {case:?}: the deny covers this create, so it must be refused, got {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.error_code(),
            ACCESS_DENIED,
            "case {case:?}: expected {ACCESS_DENIED}, got {}",
            answered.text()
        );
        assert!(
            answered
                .message()
                .starts_with(&format!("User: {} is not authorized", session_arn(CALLER_ROLE))),
            "case {case:?}: expected IAM's refusal naming the session, got {:?}",
            answered.message()
        );
        assert!(
            stored_function(&router, &name).await.is_none(),
            "case {case:?}: a refused create must leave nothing a later caller can read"
        );
    }
}
