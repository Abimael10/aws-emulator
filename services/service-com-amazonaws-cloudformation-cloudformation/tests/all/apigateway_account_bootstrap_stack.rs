// SPDX-License-Identifier: CC0-1.0

//! The "API Gateway may push logs to CloudWatch" bootstrap stack, settled on a SINGLY
//! terminal status, with the role it creates really held by IAM.
//!
//! RULE: a stack create settles on one of two statuses — `CREATE_COMPLETE` where every
//! declared resource was applied by the service that owns it, or `ROLLBACK_COMPLETE` where
//! one of them failed and everything the create had applied was undone. The doubly terminal
//! `ROLLBACK_FAILED` is reported only where the ROLLBACK ITSELF could not be carried out,
//! which leaves a stack neither usable nor deletable, so no well-formed template reaches it
//! and no create whose failure the owning service merely REFUSED does either. A stack that
//! settled `CREATE_COMPLETE` is still deletable, so its teardown reaches `DELETE_COMPLETE`
//! and leaves the services it wrote to as it found them.
//!
//! What a client that deploys this bootstrap stack therefore observes, which is what the rows
//! assert: the role exists in IAM under the trust policy and the managed policy the template
//! declared, API Gateway reports it as the account's `cloudwatchRoleArn`, and the stack can be
//! deleted again — rather than a stack wedged in a failed rollback, with the account setting
//! the template was deployed in order to write never set.
//!
//! The rows differ along what the rule generalises over: whether every declared resource can
//! be applied at all (a role API Gateway can assume, against one whose trust policy admits
//! another service only, which that service refuses to log under), and the FORM of the role
//! the account is pointed at (a name CloudFormation generates at the account root, a name the
//! template states under an IAM path).
//!
//! `apigateway_account_resource.rs` asserts WHICH setting the resource writes, over the forms
//! a declaration takes. This suite asserts the STATUS the deployment settles on and that the
//! identity behind the setting is really held by IAM, across the three services the bootstrap
//! stack spans — CloudFormation over its awsQuery wire, IAM over its own, API Gateway over
//! `restJson1`, all on the one engine `stack_lifecycle.rs` builds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_vpc_link_resources::{apigateway, member_text};
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, iam, member, output_value, resource_event_statuses,
    router, settled_stack, xml_text,
};

/// The service principal the account's CloudWatch role has to trust: API Gateway writes the
/// account's logs under that role, so the assume is granted to that service.
const APIGATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// Another service's principal, for the row whose role API Gateway cannot assume: the single
/// thing that differs between it and the bootstrap role above.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The AWS-managed policy the "API Gateway may push logs to CloudWatch" bootstrap attaches to
/// the role (AWS Managed Policy Reference, filed under the `/service-role/` path).
const PUSH_TO_CLOUDWATCH_LOGS: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonAPIGatewayPushToCloudWatchLogs";

/// The statuses this suite is about: the one a create that applied everything settles on, the
/// one a create whose resource failed settles on once the undo came through, the one a
/// teardown settles on — and the doubly terminal status none of them may be.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const ROLLBACK_FAILED: &str = "ROLLBACK_FAILED";

/// The `ResourceStatus` the resource whose create the owning service refused carries.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// A trust policy granting `sts:AssumeRole` to one service principal, the form `CreateRole`'s
/// own modelled example carries.
fn trust_policy(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The bootstrap template: the role whose logs API Gateway pushes, and the account-level
/// setting naming it. `name` and `path` state what the row varies about the role.
fn bootstrap_template(principal: &str, name: Option<&str>, path: Option<&str>) -> String {
    let mut role = json!({
        "AssumeRolePolicyDocument": trust_policy(principal),
        "ManagedPolicyArns": [PUSH_TO_CLOUDWATCH_LOGS],
    });
    if let Some(properties) = role.as_object_mut() {
        if let Some(name) = name {
            properties.insert("RoleName".to_string(), json!(name));
        }
        if let Some(path) = path {
            properties.insert("Path".to_string(), json!(path));
        }
    }
    json!({
        "Resources": {
            "CwRole": { "Type": "AWS::IAM::Role", "Properties": role },
            "Account": {
                "Type": "AWS::ApiGateway::Account",
                "Properties": { "CloudWatchRoleArn": { "Fn::GetAtt": ["CwRole", "Arn"] } },
            },
        },
        "Outputs": {
            "RoleName": { "Value": { "Ref": "CwRole" } },
            "RoleArn": { "Value": { "Fn::GetAtt": ["CwRole", "Arn"] } },
        },
    })
    .to_string()
}

/// One row: what the bootstrap stack declares, and whether every resource of it can be
/// applied.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The service principal the role's trust policy admits.
    principal: &'static str,
    /// The name the template states for the role, or `None` for one CloudFormation generates.
    role_name: Option<&'static str>,
    /// The IAM path the template files the role under, or `None` for the account root.
    path: Option<&'static str>,
    /// Whether the account-level setting the template declares can be applied at all.
    applicable: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the bootstrap stack, with a role whose name CloudFormation generates",
        stack: "apigwbootstrap-generated",
        principal: APIGATEWAY_PRINCIPAL,
        role_name: None,
        path: None,
        applicable: true,
    },
    Row {
        case: "the bootstrap stack, with a named role under the service-role path",
        stack: "apigwbootstrap-named",
        principal: APIGATEWAY_PRINCIPAL,
        role_name: Some("apigwbootstrap-named-logs"),
        path: Some("/service-role/"),
        applicable: true,
    },
    Row {
        case: "a role whose trust policy admits another service only",
        stack: "apigwbootstrap-untrusted",
        principal: LAMBDA_PRINCIPAL,
        role_name: Some("apigwbootstrap-untrusted-logs"),
        path: None,
        applicable: false,
    },
];

/// `CreateStack` under the IAM acknowledgement, then the body of the read that reported the
/// stack settled.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (status, body) = create_stack(router, stack, template, &acknowledging_iam(&[])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({stack}) must answer 200, got {status} {body}"
    );
    settled_stack(router, case, stack).await
}

/// The `StackStatus` one read reports.
fn stack_status(described: &str) -> String {
    xml_text(described, "StackStatus").unwrap_or_default()
}

/// The value the stack reports for one declared output, or a panic naming the case.
fn required_output(case: &str, described: &str, key: &str) -> String {
    output_value(described, key).unwrap_or_else(|| {
        panic!("case {case:?}: expected the stack to publish the output {key:?}, got {described}")
    })
}

/// `GetAccount` (`GET /account`): the role API Gateway reports in force for the account, or
/// `None` where it reports none.
async fn account_role(router: &MultiServiceRouter, case: &str) -> Option<String> {
    let (status, document) = apigateway(router, "GET", "/account").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetAccount to answer 200, got {status} with {document}"
    );
    member_text(&document, "cloudwatchRoleArn")
}

/// `GetRole`: the HTTP status IAM answers for one role name, and the body it answers with.
async fn get_role(router: &MultiServiceRouter, role_name: &str) -> (u16, String) {
    iam(router, "GetRole", &[("RoleName", role_name)]).await
}

/// Every row: a bootstrap stack whose resources can all be applied settles `CREATE_COMPLETE`
/// with the role really held by IAM and in force in API Gateway, and is still deletable; one
/// whose account setting the owning service refuses settles `ROLLBACK_COMPLETE` with the
/// account carrying no role. Neither settles on the doubly terminal rollback status.
#[tokio::test]
async fn the_bootstrap_stack_settles_on_a_singly_terminal_status() {
    for row in ROWS {
        let case = row.case;
        // A router per row: the account settings are a SINGLETON of the account and Region,
        // so each row deploys against an account nothing has set a role on.
        let router = router();
        let template = bootstrap_template(row.principal, row.role_name, row.path);
        let described = deploy(&router, case, row.stack, &template).await;
        let settled = stack_status(&described);
        assert_ne!(
            settled, ROLLBACK_FAILED,
            "case {case:?}: a refused resource must not wedge the stack in {ROLLBACK_FAILED}, \
             got {described}"
        );
        assert_eq!(
            settled,
            if row.applicable {
                CREATE_COMPLETE
            } else {
                ROLLBACK_COMPLETE
            },
            "case {case:?}: expected the create to settle on the status its resources owe, got \
             {described}"
        );

        if !row.applicable {
            // The resource the owning service refused is the one the stack reports failed,
            // and nothing of it was left behind: the account logs under no role.
            let statuses = resource_event_statuses(&router, case, row.stack, "Account").await;
            assert!(
                statuses.iter().any(|status| status == CREATE_FAILED),
                "case {case:?}: expected a {CREATE_FAILED} event for the account, got {statuses:?}"
            );
            let reported = account_role(&router, case).await;
            assert_eq!(
                reported, None,
                "case {case:?}: expected the rolled-back stack to leave no account role, got \
                 {reported:?}"
            );
            continue;
        }

        let role_arn = required_output(case, &described, "RoleArn");
        let role_name = required_output(case, &described, "RoleName");

        // The role is really IAM's, under what the template declared: a setting pointing at a
        // role nothing holds would be a success backed by no identity.
        let (status, body) = get_role(&router, &role_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected IAM to hold the role {role_name:?}, got {status} {body}"
        );
        assert_eq!(
            xml_text(&body, "Arn").as_deref(),
            Some(role_arn.as_str()),
            "case {case:?}: expected IAM to report the ARN the stack published {role_arn:?}, got \
             {body}"
        );
        assert_eq!(
            xml_text(&body, "Path").as_deref(),
            Some(row.path.unwrap_or("/")),
            "case {case:?}: expected the role to be filed under the declared path, got {body}"
        );
        let trust = xml_text(&body, "AssumeRolePolicyDocument").unwrap_or_default();
        assert!(
            trust.contains(APIGATEWAY_PRINCIPAL),
            "case {case:?}: expected the role's trust policy to admit {APIGATEWAY_PRINCIPAL}, got \
             {trust:?}"
        );
        let (status, attached) = iam(
            &router,
            "ListAttachedRolePolicies",
            &[("RoleName", role_name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListAttachedRolePolicies to answer 200, got {status} \
             {attached}"
        );
        assert!(
            attached.contains(PUSH_TO_CLOUDWATCH_LOGS),
            "case {case:?}: expected the declared managed policy to be attached, got {attached}"
        );

        // And the account really logs under it, which is what the template was deployed for.
        let reported = account_role(&router, case).await;
        assert_eq!(
            reported.as_deref(),
            Some(role_arn.as_str()),
            "case {case:?}: expected GetAccount to report the declared role {role_arn:?}, got \
             {reported:?}"
        );

        // A completed create leaves a stack that can still be taken away: it reaches
        // DELETE_COMPLETE, the account is as the stack found it, and IAM holds no role.
        let stack_id = xml_text(&described, "StackId").unwrap_or_default();
        let (status, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteStack({}) answered {status}: {body}",
            row.stack
        );
        let torn_down = settled_stack(&router, case, &stack_id).await;
        assert_eq!(
            stack_status(&torn_down),
            DELETE_COMPLETE,
            "case {case:?}: expected the teardown to reach {DELETE_COMPLETE}, got {torn_down}"
        );
        let reported = account_role(&router, case).await;
        assert_eq!(
            reported, None,
            "case {case:?}: expected the deleted stack to leave no account role, got {reported:?}"
        );
        // The teardown is settled, so the role it deleted is already gone: a resource this
        // stack reports deleted is one IAM no longer holds.
        let (status, body) = get_role(&router, &role_name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected IAM to hold no role under {role_name:?} once the stack is \
             gone, got {status} {body}"
        );
    }
}
