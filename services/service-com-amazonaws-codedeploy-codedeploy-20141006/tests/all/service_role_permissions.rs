// SPDX-License-Identifier: CC0-1.0

//! What a deployment's SERVICE ROLE lets that deployment do, end to end over the
//! wire with IAM, AWS Lambda and CloudWatch mounted beside CodeDeploy on one
//! engine.
//!
//! RULE under test: a deployment acts as its deployment group's `serviceRoleArn`,
//! so before it does any work CodeDeploy evaluates the identity policies in force
//! on that role against the calls the deployment makes on other AWS services — the
//! alias update a Lambda-platform deployment performs, and the alarm poll a
//! deployment whose effective `alarmConfiguration` is enabled performs. Creating
//! the group and creating the deployment both succeed whatever the role grants; a
//! deployment whose role denies or does not grant one of those calls then reaches
//! the terminal status `Failed` with `errorInformation.code`
//! `IAM_ROLE_PERMISSIONS`, the fixed sentence naming the role and the AWS service,
//! and NO effect — the alias stays where it was, with the routing it had. A role
//! this emulator can show nothing about leaves the deployment alone.
//!
//! The role, its policies, the function and its alias, and the alarms all belong to
//! other services, so every one of them is created through that service's own front
//! door and read back through it; nothing reaches into any store.

use serde_json::{json, Value};

use crate::codedeploy_lifecycle::{given_role, terminal_deployment, trust_policy, Emulator};

/// The service principal a CodeDeploy service role has to trust.
const SERVICE_PRINCIPAL: &str = "codedeploy.amazonaws.com";

/// The `ErrorCode` a deployment its service role did not authorize reports
/// (`smithy_query(query="shape",shape="ErrorCode")` carries `IAM_ROLE_PERMISSIONS`).
const IAM_ROLE_PERMISSIONS: &str = "IAM_ROLE_PERMISSIONS";

/// The collection a function's aliases bind, which is also where a function is
/// created: `POST /2015-03-31/functions`.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// AWS's fixed sentence for the `IAM_ROLE_PERMISSIONS` error code, spelled out here
/// as a client matching on it reads it: its variable parts are the role the
/// deployment group carries and the AWS service the deployment could not operate
/// in.
fn iam_role_permissions_message(role_arn: &str, service_name: &str) -> String {
    format!(
        "The IAM role {role_arn} does not give you permission to perform operations in the \
         following AWS service: {service_name}. Contact your AWS administrator if you need \
         help. If you are an AWS administrator, you can grant permissions to your users or \
         groups by creating IAM policies."
    )
}

/// One IAM policy statement, as a document with a single statement in it.
fn policy(effect: &str, actions: &[&str], resources: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Action": actions,
            "Resource": resources,
        }],
    })
    .to_string()
}

/// A policy document with every statement `statements` names, in that order.
fn policy_of(statements: &[Value]) -> String {
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// The ARN of one function's alias, the resource the alias update acts on:
/// `arn:aws:lambda:<region>:<account>:function:<function>:<alias>`, as a policy
/// scoped to that one alias names it. The region is the one an unsigned request
/// through this suite's front door is served in.
fn alias_arn(function: &str, alias: &str) -> String {
    format!(
        "arn:aws:lambda:us-east-1:{}:function:{function}:{alias}",
        engine::emulator_account_id()
    )
}

/// Embed `document` in the role named `name` as its inline deployment policy,
/// through IAM's own `PutRolePolicy` — so what CodeDeploy evaluates is the very
/// document IAM stored.
///
/// A put of a policy name the role already carries REPLACES that document, which is
/// how a client revokes or restores a grant on a role its deployment groups are
/// already running as.
async fn put_role_policy(emulator: &Emulator, name: &str, document: &str) {
    let (status, body) = emulator
        .iam_query(
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "deployment-permissions"),
                ("PolicyDocument", document),
            ],
        )
        .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: PutRolePolicy must succeed, got {status} with {body}"
    );
}

/// Create a role that trusts CodeDeploy and embed `document` in it as an inline
/// policy, answering the role's ARN.
async fn given_role_with_policy(emulator: &Emulator, name: &str, document: &str) -> String {
    let role = given_role(emulator, name, &trust_policy(SERVICE_PRINCIPAL)).await;
    put_role_policy(emulator, name, document).await;
    role
}

/// Create a function with two published versions and an alias on version 1.
async fn given_function_with_alias(emulator: &Emulator, function: &str, alias: &str) {
    given_function_with_versions(emulator, function, alias, 2).await;
}

/// Create a function with `versions` published versions and an alias on version 1,
/// so a suite that deploys to it more than once has a further version to shift to
/// each time.
async fn given_function_with_versions(
    emulator: &Emulator,
    function: &str,
    alias: &str,
    versions: u32,
) {
    emulator
        .lambda_ok(
            function,
            "POST",
            FUNCTIONS_URI,
            json!({
                "FunctionName": function,
                "Role": format!("arn:aws:iam::{}:role/handler", engine::emulator_account_id()),
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": {"ZipFile": ""},
            }),
            201,
        )
        .await;
    for version in 1..=versions {
        emulator
            .lambda_ok(
                function,
                "POST",
                &format!("{FUNCTIONS_URI}/{function}/versions"),
                json!({"Description": format!("snapshot {version}")}),
                201,
            )
            .await;
    }
    emulator
        .lambda_ok(
            function,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/aliases"),
            json!({"Name": alias, "FunctionVersion": "1"}),
            201,
        )
        .await;
}

/// The alias as a client reads it: the version it invokes and its routing
/// configuration.
async fn alias_of(emulator: &Emulator, function: &str, alias: &str) -> Value {
    emulator
        .lambda_ok(
            function,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/aliases/{alias}"),
            Value::Null,
            200,
        )
        .await
}

/// Create a Lambda application and one deployment group on it running as `role`
/// and carrying `extras`, answering the group's name.
async fn given_group(
    emulator: &Emulator,
    application: &str,
    role: &str,
    extras: &Value,
) -> String {
    emulator
        .ok(
            "CreateApplication",
            json!({"applicationName": application, "computePlatform": "Lambda"}),
        )
        .await;
    let group = format!("{application}-dg");
    let mut request = json!({
        "applicationName": application,
        "deploymentGroupName": group,
        "deploymentConfigName": "CodeDeployDefault.LambdaAllAtOnce",
        "serviceRoleArn": role,
    });
    for (member, value) in extras.as_object().cloned().unwrap_or_default() {
        request[member] = value;
    }
    let (status, body) = emulator.call("CreateDeploymentGroup", request).await;
    assert_eq!(
        status, 200,
        "setup {application:?}: an under-privileged role is accepted by \
         CreateDeploymentGroup, got {status} with {body:?}"
    );
    group
}

/// The AppSpec of a Lambda deployment shifting `alias` from version 1 to 2.
fn app_spec_revision(function: &str, alias: &str) -> Value {
    app_spec_shift(function, alias, "1", "2")
}

/// The AppSpec of a Lambda deployment shifting `alias` from version `from` to
/// version `to`.
fn app_spec_shift(function: &str, alias: &str, from: &str, to: &str) -> Value {
    let content = json!({
        "version": 0.0,
        "Resources": [{
            function: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Name": function,
                    "Alias": alias,
                    "CurrentVersion": from,
                    "TargetVersion": to,
                },
            },
        }],
    })
    .to_string();
    json!({"revisionType": "AppSpecContent", "appSpecContent": {"content": content}})
}

/// Create a deployment of `revision` and answer its minted id, asserting that
/// CodeDeploy ACCEPTED it: the role is never a refusal of this call.
async fn deploy(emulator: &Emulator, application: &str, group: &str, revision: Value) -> String {
    let (status, body) = emulator
        .call(
            "CreateDeployment",
            json!({
                "applicationName": application,
                "deploymentGroupName": group,
                "revision": revision,
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup {application:?}: CreateDeployment must be accepted whatever the role grants, \
         got {status} with {body:?}"
    );
    body["deploymentId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateDeployment answered no deploymentId: {body:?}"))
        .to_string()
}

/// Create a CloudWatch alarm named `name`; its state is whatever CloudWatch holds
/// for a metric nothing has published into, which no gate stops a deployment for.
async fn given_alarm(emulator: &Emulator, name: &str) {
    emulator
        .cloudwatch_ok(
            "PutMetricAlarm",
            json!({
                "AlarmName": name,
                "Namespace": "Sim/CodeDeployRole",
                "MetricName": "Errors",
                "Statistic": "Sum",
                "Period": 60,
                "EvaluationPeriods": 1,
                "Threshold": 1.0,
                "ComparisonOperator": "GreaterThanOrEqualToThreshold",
                "TreatMissingData": "notBreaching",
            }),
        )
        .await;
}

/// RULE: the identity policies in force on the group's service role decide whether
/// a Lambda deployment may move its alias; one that may not ends `Failed` with
/// `IAM_ROLE_PERMISSIONS` naming the role and `AWSLambda`, and the alias is
/// untouched.
///
/// Rows differ along what the role's policy states about the alias update: a grant
/// of every Lambda action, a grant of only that action on only that alias, a policy
/// covering every other service the deployment's role would need and no Lambda
/// action at all, an explicit `Deny` beside a broad `Allow`, and a grant of the
/// action on ANOTHER function's alias.
#[tokio::test]
async fn a_lambda_deployment_is_authorized_by_its_service_role() {
    let emulator = Emulator::with_lambda_cloudwatch_and_iam();
    let alias = "live";
    // The function each row deploys to, named up front so a row's policy can name
    // that function's alias rather than being built from the row's position.
    let broad = "role-fn-broad";
    let scoped = "role-fn-scoped";
    let other_services = "role-fn-other-services";
    let denied = "role-fn-denied";
    let other_function = "role-fn-other-function";

    // (case, the function deployed to, the inline policy document on the role,
    // whether the deployment is authorized)
    let cases: &[(&str, &str, String, bool)] = &[
        (
            "every Lambda action on everything",
            broad,
            policy("Allow", &["lambda:*"], &["*"]),
            true,
        ),
        (
            "the alias update alone, on that alias alone",
            scoped,
            policy(
                "Allow",
                &["lambda:UpdateAlias"],
                &[&alias_arn(scoped, alias)],
            ),
            true,
        ),
        (
            "every service but Lambda",
            other_services,
            policy(
                "Allow",
                &[
                    "codedeploy:*",
                    "cloudwatch:DescribeAlarms",
                    "s3:Get*",
                    "s3:List*",
                    "sns:Publish",
                    "tag:GetResources",
                ],
                &["*"],
            ),
            false,
        ),
        (
            "an explicit Deny of the alias update beside a broad Allow",
            denied,
            policy_of(&[
                json!({"Effect": "Allow", "Action": "*", "Resource": "*"}),
                json!({
                    "Effect": "Deny",
                    "Action": "lambda:UpdateAlias",
                    "Resource": "*",
                }),
            ]),
            false,
        ),
        (
            "the alias update on another function's alias",
            other_function,
            policy(
                "Allow",
                &["lambda:UpdateAlias"],
                &[&alias_arn("a-function-nothing-deploys", alias)],
            ),
            false,
        ),
    ];

    for (index, (case, function, document, authorized)) in cases.iter().enumerate() {
        let application = format!("role-app-{index}");
        given_function_with_alias(&emulator, function, alias).await;
        let role =
            given_role_with_policy(&emulator, &format!("role-{index}"), document).await;
        let group = given_group(&emulator, &application, &role, &json!({})).await;
        let deployment_id =
            deploy(&emulator, &application, &group, app_spec_revision(function, alias)).await;
        let info = terminal_deployment(&emulator, &deployment_id, case).await;
        let read = alias_of(&emulator, function, alias).await;
        if *authorized {
            assert_eq!(
                (info["status"].as_str(), read["FunctionVersion"].as_str()),
                (Some("Succeeded"), Some("2")),
                "case {case:?}: expected the deployment to succeed and the alias to move, got \
                 {info:?} and {read:?}"
            );
            continue;
        }
        assert_eq!(
            (
                info["status"].as_str(),
                info["errorInformation"]["code"].as_str(),
                info["errorInformation"]["message"].as_str(),
            ),
            (
                Some("Failed"),
                Some(IAM_ROLE_PERMISSIONS),
                Some(iam_role_permissions_message(&role, "AWSLambda").as_str()),
            ),
            "case {case:?}: expected a Failed deployment naming the role and AWSLambda, got \
             {info:?}"
        );
        // The refused deployment had no effect: the alias is where it was, with no
        // weighted routing.
        assert_eq!(
            (
                read["FunctionVersion"].as_str(),
                read["RoutingConfig"]["AdditionalVersionWeights"]
                    .as_object()
                    .map(serde_json::Map::len)
                    .unwrap_or_default(),
            ),
            (Some("1"), 0),
            "case {case:?}: expected the alias left on version 1 with no weights, got {read:?}"
        );
    }
}

/// RULE: the alarm poll a deployment with an enabled `alarmConfiguration` performs
/// needs the role's permission too, and `ignorePollAlarmFailure` — which is about
/// alarm state that could not be RETRIEVED — does not excuse a role that may not
/// retrieve it at all: both values of the member are refused identically, naming
/// `AmazonCloudWatch`.
#[tokio::test]
async fn an_enabled_alarm_configuration_needs_the_role_to_allow_the_poll() {
    let emulator = Emulator::with_lambda_cloudwatch_and_iam();
    let alias = "live";
    let alarm = "role-alarm";
    given_alarm(&emulator, alarm).await;

    // Every row's role may move the alias; they differ in what the role says about
    // the alarm poll, and in the `ignorePollAlarmFailure` the group carries.
    let allows_lambda = json!({"Effect": "Allow", "Action": "lambda:*", "Resource": "*"});
    let allows_poll =
        json!({"Effect": "Allow", "Action": "cloudwatch:DescribeAlarms", "Resource": "*"});
    let denies_poll = json!({
        "Effect": "Deny",
        "Action": ["cloudwatch:DescribeAlarms", "cloudwatch:DescribeAlarmHistory"],
        "Resource": "*",
    });

    // (case, the role's policy document, ignorePollAlarmFailure, the AWS service
    // the deployment must be refused for, if any)
    let denies = policy_of(&[allows_lambda.clone(), denies_poll]);
    let allows = policy_of(&[allows_lambda.clone(), allows_poll]);
    let silent = policy_of(&[allows_lambda]);
    let cases: &[(&str, &str, bool, Option<&str>)] = &[
        (
            "the poll denied, with ignorePollAlarmFailure false",
            &denies,
            false,
            Some("AmazonCloudWatch"),
        ),
        (
            "the poll denied, with ignorePollAlarmFailure true",
            &denies,
            true,
            Some("AmazonCloudWatch"),
        ),
        (
            "the poll never granted at all",
            &silent,
            true,
            Some("AmazonCloudWatch"),
        ),
        ("the poll granted", &allows, false, None),
        (
            "the poll granted, with ignorePollAlarmFailure true",
            &allows,
            true,
            None,
        ),
    ];

    for (index, (case, document, ignore_poll_alarm_failure, refused_for)) in
        cases.iter().enumerate()
    {
        let function = format!("alarm-fn-{index}");
        let application = format!("alarm-app-{index}");
        given_function_with_alias(&emulator, &function, alias).await;
        let role =
            given_role_with_policy(&emulator, &format!("alarm-role-{index}"), document).await;
        let group = given_group(
            &emulator,
            &application,
            &role,
            &json!({
                "alarmConfiguration": {
                    "enabled": true,
                    "ignorePollAlarmFailure": ignore_poll_alarm_failure,
                    "alarms": [{"name": alarm}],
                },
            }),
        )
        .await;
        let deployment_id =
            deploy(&emulator, &application, &group, app_spec_revision(&function, alias)).await;
        let info = terminal_deployment(&emulator, &deployment_id, case).await;
        let read = alias_of(&emulator, &function, alias).await;
        match refused_for {
            Some(service) => assert_eq!(
                (
                    info["status"].as_str(),
                    info["errorInformation"]["code"].as_str(),
                    info["errorInformation"]["message"].as_str(),
                    read["FunctionVersion"].as_str(),
                ),
                (
                    Some("Failed"),
                    Some(IAM_ROLE_PERMISSIONS),
                    Some(iam_role_permissions_message(&role, service).as_str()),
                    Some("1"),
                ),
                "case {case:?}: expected a Failed deployment naming {service} and an unmoved \
                 alias, got {info:?} and {read:?}"
            ),
            None => assert_eq!(
                (info["status"].as_str(), read["FunctionVersion"].as_str()),
                (Some("Succeeded"), Some("2")),
                "case {case:?}: expected the deployment to succeed and the alias to move, got \
                 {info:?} and {read:?}"
            ),
        }
    }
}

/// RULE: nothing is refused on evidence this emulator does not have — a role whose
/// permissions it cannot read says nothing about the deployment, which goes ahead
/// and moves the alias.
///
/// Rows differ along WHY there is no evidence: an ARN naming another account's
/// role, an ARN of this account IAM holds no role for, and a role IAM does hold
/// that carries no policy document at all.
#[tokio::test]
async fn a_role_this_emulator_knows_nothing_about_refuses_nothing() {
    let emulator = Emulator::with_lambda_cloudwatch_and_iam();
    let alias = "live";
    let unpolicied = given_role(
        &emulator,
        "no-policy-role",
        &trust_policy(SERVICE_PRINCIPAL),
    )
    .await;

    let cases: &[(&str, String)] = &[
        (
            "another account's role",
            "arn:aws:iam::210987654321:role/CodeDeployRole".to_string(),
        ),
        (
            "a role of this account IAM holds nothing for",
            format!(
                "arn:aws:iam::{}:role/NeverCreated",
                engine::emulator_account_id()
            ),
        ),
        ("a role carrying no policy at all", unpolicied.clone()),
    ];

    for (index, (case, role)) in cases.iter().enumerate() {
        let function = format!("quiet-fn-{index}");
        let application = format!("quiet-app-{index}");
        given_function_with_alias(&emulator, &function, alias).await;
        let group = given_group(&emulator, &application, role, &json!({})).await;
        let deployment_id =
            deploy(&emulator, &application, &group, app_spec_revision(&function, alias)).await;
        let info = terminal_deployment(&emulator, &deployment_id, case).await;
        let read = alias_of(&emulator, &function, alias).await;
        assert_eq!(
            (info["status"].as_str(), read["FunctionVersion"].as_str()),
            (Some("Succeeded"), Some("2")),
            "case {case:?}: expected the deployment to succeed and the alias to move, got \
             {info:?} and {read:?}"
        );
    }
}

/// RULE: the verdict is read off the documents in force on the deployment group's
/// service role AT THE INSTANT the deployment runs, so revoking a grant the
/// deployment needs refuses every deployment created after the rewrite — with
/// `Failed`, `IAM_ROLE_PERMISSIONS` and the fixed sentence naming the role and the
/// service that call reaches — and restoring the grant lets the identical
/// deployment through, each time leaving the alias exactly where that outcome says.
///
/// Rows differ along which of the deployment's calls the rewrite revokes and what
/// the group says about alarms: the alarm poll with `ignorePollAlarmFailure` false,
/// the same poll with it true (the member is about alarm state that could not be
/// RETRIEVED, so it excuses no revocation), and the alias update on a group polling
/// no alarm at all.
#[tokio::test]
async fn a_revoked_grant_refuses_later_deployments_and_a_restored_one_lets_them_through() {
    let emulator = Emulator::with_lambda_cloudwatch_and_iam();
    let alias = "live";
    let alarm = "revoked-grant-alarm";
    given_alarm(&emulator, alarm).await;

    let allows_lambda = json!({"Effect": "Allow", "Action": "lambda:*", "Resource": "*"});
    let allows_poll =
        json!({"Effect": "Allow", "Action": "cloudwatch:DescribeAlarms", "Resource": "*"});
    let denies_poll = json!({
        "Effect": "Deny",
        "Action": ["cloudwatch:DescribeAlarms", "cloudwatch:DescribeAlarmHistory"],
        "Resource": "*",
    });
    let denies_alias =
        json!({"Effect": "Deny", "Action": "lambda:UpdateAlias", "Resource": "*"});
    // Every row starts from this document, which grants every call its deployment
    // makes; the rewrite adds the row's own Deny to it and changes nothing else.
    let granted = policy_of(&[allows_lambda.clone(), allows_poll.clone()]);

    // (case, the alarm configuration's `ignorePollAlarmFailure` when the group
    // carries one at all, the statement the rewrite adds, the AWS service the
    // refusal must name)
    let cases: &[(&str, Option<bool>, Value, &str)] = &[
        (
            "the alarm poll revoked, with ignorePollAlarmFailure false",
            Some(false),
            denies_poll.clone(),
            "AmazonCloudWatch",
        ),
        (
            "the alarm poll revoked, with ignorePollAlarmFailure true",
            Some(true),
            denies_poll,
            "AmazonCloudWatch",
        ),
        (
            "the alias update revoked, on a group polling no alarm",
            None,
            denies_alias,
            "AWSLambda",
        ),
    ];

    for (index, (case, ignore_poll_alarm_failure, denial, service)) in cases.iter().enumerate() {
        let function = format!("revoked-fn-{index}");
        let application = format!("revoked-app-{index}");
        // Three versions: one shift while the grant stands, one while it is
        // revoked, and the identical shift once more when it is restored.
        given_function_with_versions(&emulator, &function, alias, 3).await;
        let role_name = format!("revoked-role-{index}");
        let role = given_role_with_policy(&emulator, &role_name, &granted).await;
        let extras = match ignore_poll_alarm_failure {
            Some(ignore) => json!({
                "alarmConfiguration": {
                    "enabled": true,
                    "ignorePollAlarmFailure": ignore,
                    "alarms": [{"name": alarm}],
                },
            }),
            None => json!({}),
        };
        let group = given_group(&emulator, &application, &role, &extras).await;

        // The positive control: with every call granted, the deployment succeeds
        // and the alias moves.
        let first = deploy(
            &emulator,
            &application,
            &group,
            app_spec_shift(&function, alias, "1", "2"),
        )
        .await;
        let info = terminal_deployment(&emulator, &first, case).await;
        let read = alias_of(&emulator, &function, alias).await;
        assert_eq!(
            (info["status"].as_str(), read["FunctionVersion"].as_str()),
            (Some("Succeeded"), Some("2")),
            "case {case:?} with every call granted: expected the deployment to succeed and the \
             alias to move to version 2, got {info:?} and {read:?}"
        );

        // The one thing that changes: the role's policy now denies the row's call.
        let revoked = policy_of(&[allows_lambda.clone(), allows_poll.clone(), denial.clone()]);
        put_role_policy(&emulator, &role_name, &revoked).await;

        let refused = deploy(
            &emulator,
            &application,
            &group,
            app_spec_shift(&function, alias, "2", "3"),
        )
        .await;
        let info = terminal_deployment(&emulator, &refused, case).await;
        let read = alias_of(&emulator, &function, alias).await;
        assert_eq!(
            (
                info["status"].as_str(),
                info["errorInformation"]["code"].as_str(),
                info["errorInformation"]["message"].as_str(),
            ),
            (
                Some("Failed"),
                Some(IAM_ROLE_PERMISSIONS),
                Some(iam_role_permissions_message(&role, service).as_str()),
            ),
            "case {case:?} once revoked: expected a Failed deployment naming the role and \
             {service}, got {info:?}"
        );
        // The refused deployment had no effect: the alias is still on the version
        // the control left it on, with no weighted routing.
        assert_eq!(
            (
                read["FunctionVersion"].as_str(),
                read["RoutingConfig"]["AdditionalVersionWeights"]
                    .as_object()
                    .map(serde_json::Map::len)
                    .unwrap_or_default(),
            ),
            (Some("2"), 0),
            "case {case:?} once revoked: expected the alias left on version 2 with no weights, \
             got {read:?}"
        );

        // Restored, the identical deployment goes through.
        put_role_policy(&emulator, &role_name, &granted).await;
        let restored = deploy(
            &emulator,
            &application,
            &group,
            app_spec_shift(&function, alias, "2", "3"),
        )
        .await;
        let info = terminal_deployment(&emulator, &restored, case).await;
        let read = alias_of(&emulator, &function, alias).await;
        assert_eq!(
            (info["status"].as_str(), read["FunctionVersion"].as_str()),
            (Some("Succeeded"), Some("3")),
            "case {case:?} once restored: expected the identical deployment to succeed and the \
             alias to move to version 3, got {info:?} and {read:?}"
        );
    }
}
