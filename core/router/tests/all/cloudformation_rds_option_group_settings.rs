// SPDX-License-Identifier: CC0-1.0

//! What an `AWS::RDS::OptionGroup`'s declared `OptionConfigurations` leave behind in Amazon
//! RDS: the OPTION SETTINGS, with the values the template gave them.
//!
//! The rule under test: a property a stack declares is carried out in the owning service's own
//! vocabulary, so every leaf of a declared structure reaches that service under the name its
//! model gives it — and for an awsQuery list that name is the element the list's own member
//! fixes (`OptionConfiguration.1.OptionSettings.OptionSetting.1.Name`, not the Smithy default
//! `member`). A list written under a segment the called operation does not read is a list the
//! service never sees: the stack still reports `CREATE_COMPLETE`, and the resource is
//! configured differently from what the template declared. `DescribeOptionGroups` is where a
//! client reads an option's settings back, so what it reports for a setting the template set
//! is the value the template set, while a setting the template left alone keeps the value its
//! option publishes as the default.
//!
//! The cases span what the rule generalises over: four engines and four options, one declared
//! setting and several, settings of `STRING` and of `INTEGER` type, a collection setting whose
//! value is itself comma-separated, a value the stack itself produced (an `AWS::IAM::Role`'s
//! ARN, reached by `Fn::GetAtt` — nothing in this file can spell it in advance), two option
//! configurations in one declaration, and an option configuration carrying the other two lists
//! of its own (`VpcSecurityGroupMemberships`, `DBSecurityGroupMemberships`) whose elements are
//! likewise named by their list members rather than by `member`.
//!
//! Three crates have to agree for this to hold — CloudFormation deploys the group, Amazon RDS
//! owns it and answers the read, IAM answers for the role whose ARN one setting carries — so
//! the suite lives here rather than in any one of them.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, blocks_of, element, elements, form_encode, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The statuses a stack settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one poll waits before the next, and how many are made: the stack settles on
/// another service's own clock, so the suite waits for the CONDITION rather than for a span,
/// with a budget far longer than the spans themselves so a loaded host does not decide the
/// outcome.
const POLL: Duration = Duration::from_millis(50);
/// See [`POLL`].
const POLLS: usize = 2_400;

/// The sentinel an expectation uses for "the ARN the stack's own role got", which no line of
/// this file can spell in advance: it is replaced by the ARN the stack published before the
/// comparison.
const DEPLOYED_ROLE_ARN: &str = "<the stack's role ARN>";

/// The emulator as it is ASSEMBLED, on one engine: the CloudFormation provisioner's own calls
/// are dispatched back into this same router, so the group it creates is the one this read
/// resolves.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One Amazon RDS call over its awsQuery wire.
async fn rds(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "rds", "2014-10-31", action, members).await
}

/// Deploy `body` as the stack `stack` and answer the status it settled in, with the stack's own
/// answer.
async fn deploy(router: &MultiServiceRouter, stack: &str, body: &str) -> (String, String) {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", body),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateStack {stack:?}: expected HTTP 200, got {status} with body {answered}"
    );
    let mut last = stack_state(router, stack).await;
    for _ in 0..POLLS {
        if !last.0.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = stack_state(router, stack).await;
    }
    last
}

/// The `StackStatus` the stack reports now, with the answer it was read from.
async fn stack_state(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {body}"
    );
    (opt_element(&body, "StackStatus").unwrap_or_default(), body)
}

/// The value the stack published under the output `key`, read out of its `Outputs` block so no
/// other repeated `member` of the answer is mistaken for one.
fn output(answered: &str, key: &str) -> Option<String> {
    let outputs = block_of(answered, "Outputs")?;
    blocks_of(outputs, "member")
        .into_iter()
        .find(|member| opt_element(member, "OutputKey").as_deref() == Some(key))
        .and_then(|member| opt_element(member, "OutputValue"))
}

/// Every `ResourceStatusReason` the stack has recorded, so a deployment that did not settle
/// where the case says reports WHY.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// One case: the engine and major version the group is for, the `OptionConfigurations` the
/// template declares (as the JSON text of that property), and what `DescribeOptionGroups` then
/// has to report.
struct Case {
    case: &'static str,
    stack: &'static str,
    engine: &'static str,
    major_version: &'static str,
    /// `true` where the declaration reaches for the stack's own `AWS::IAM::Role` ARN, which is
    /// then also published as an output so the expectation can name it.
    declares_a_role: bool,
    /// The declared `OptionConfigurations`, as JSON.
    configurations: &'static str,
    /// `(option, setting, value)` the group has to report, with [`DEPLOYED_ROLE_ARN`] standing
    /// for the ARN the stack's role got.
    settings: &'static [(&'static str, &'static str, &'static str)],
    /// `(option, member, value)` of the other two membership lists an option configuration can
    /// carry, each reported under the element its own list member names.
    memberships: &'static [(&'static str, &'static str, &'static str)],
}

const CASES: &[Case] = &[
    Case {
        case: "the SQL Server native backup option, its one setting set to the stack's role ARN",
        stack: "xsvc-og-sqlserver-backup",
        engine: "sqlserver-ex",
        major_version: "16.00",
        declares_a_role: true,
        configurations: r#"[{
            "OptionName": "SQLSERVER_BACKUP_RESTORE",
            "OptionSettings": [{
                "Name": "IAM_ROLE_ARN",
                "Value": { "Fn::GetAtt": ["S3IntegrationRole", "Arn"] }
            }]
        }]"#,
        settings: &[(
            "SQLSERVER_BACKUP_RESTORE",
            "IAM_ROLE_ARN",
            DEPLOYED_ROLE_ARN,
        )],
        memberships: &[],
    },
    Case {
        case: "the memcached option, three settings of two data types beside a declared port",
        stack: "xsvc-og-memcached",
        engine: "mysql",
        major_version: "8.0",
        declares_a_role: false,
        configurations: r#"[{
            "OptionName": "MEMCACHED",
            "Port": 11299,
            "OptionSettings": [
                { "Name": "DAEMON_MEMCACHED_R_BATCH_SIZE", "Value": "8" },
                { "Name": "INNODB_API_TRX_LEVEL", "Value": "3" },
                { "Name": "DAEMON_MEMCACHED_OPTION", "Value": "-vv" }
            ]
        }]"#,
        settings: &[
            ("MEMCACHED", "DAEMON_MEMCACHED_R_BATCH_SIZE", "8"),
            ("MEMCACHED", "INNODB_API_TRX_LEVEL", "3"),
            ("MEMCACHED", "DAEMON_MEMCACHED_OPTION", "-vv"),
            // The setting the declaration left alone keeps its published default, so the
            // values that round-trip are the declared ones and not every setting at once.
            ("MEMCACHED", "DAEMON_MEMCACHED_W_BATCH_SIZE", "1"),
        ],
        memberships: &[],
    },
    Case {
        case: "the MariaDB audit plugin, a collection setting whose value is comma-separated",
        stack: "xsvc-og-mariadb-audit",
        engine: "mariadb",
        major_version: "10.11",
        declares_a_role: false,
        configurations: r#"[{
            "OptionName": "MARIADB_AUDIT_PLUGIN",
            "OptionSettings": [
                { "Name": "SERVER_AUDIT_EVENTS", "Value": "CONNECT,QUERY_DDL,QUERY_DML" },
                { "Name": "SERVER_AUDIT_INCL_USERS", "Value": "reporter,auditor" },
                { "Name": "SERVER_AUDIT_FILE_ROTATIONS", "Value": "17" }
            ],
            "VpcSecurityGroupMemberships": ["sg-0a1b2c3d4e5f60711", "sg-1b2c3d4e"]
        }]"#,
        settings: &[
            (
                "MARIADB_AUDIT_PLUGIN",
                "SERVER_AUDIT_EVENTS",
                "CONNECT,QUERY_DDL,QUERY_DML",
            ),
            (
                "MARIADB_AUDIT_PLUGIN",
                "SERVER_AUDIT_INCL_USERS",
                "reporter,auditor",
            ),
            ("MARIADB_AUDIT_PLUGIN", "SERVER_AUDIT_FILE_ROTATIONS", "17"),
            // Published as not modifiable, so it reports its default whatever is declared.
            ("MARIADB_AUDIT_PLUGIN", "SERVER_AUDIT", "FORCE_PLUS_PERMANENT"),
        ],
        memberships: &[
            (
                "MARIADB_AUDIT_PLUGIN",
                "VpcSecurityGroupId",
                "sg-0a1b2c3d4e5f60711",
            ),
            ("MARIADB_AUDIT_PLUGIN", "VpcSecurityGroupId", "sg-1b2c3d4e"),
        ],
    },
    Case {
        case: "two option configurations in one declaration, one naming a DB security group",
        stack: "xsvc-og-sqlserver-audit",
        engine: "sqlserver-ee",
        major_version: "15.00",
        declares_a_role: true,
        configurations: r#"[
            {
                "OptionName": "SQLSERVER_AUDIT",
                "OptionSettings": [
                    { "Name": "IAM_ROLE_ARN", "Value": { "Fn::GetAtt": ["S3IntegrationRole", "Arn"] } },
                    { "Name": "S3_BUCKET_ARN", "Value": "arn:aws:s3:::xsvc-og-audit-logs" },
                    { "Name": "RETENTION_TIME", "Value": "840" },
                    { "Name": "ENABLE_COMPRESSION", "Value": "false" }
                ],
                "DBSecurityGroupMemberships": ["xsvc-og-audit-group"]
            },
            {
                "OptionName": "SQLSERVER_BACKUP_RESTORE",
                "OptionSettings": [
                    { "Name": "IAM_ROLE_ARN", "Value": { "Fn::GetAtt": ["S3IntegrationRole", "Arn"] } }
                ]
            }
        ]"#,
        settings: &[
            ("SQLSERVER_AUDIT", "IAM_ROLE_ARN", DEPLOYED_ROLE_ARN),
            (
                "SQLSERVER_AUDIT",
                "S3_BUCKET_ARN",
                "arn:aws:s3:::xsvc-og-audit-logs",
            ),
            ("SQLSERVER_AUDIT", "RETENTION_TIME", "840"),
            ("SQLSERVER_AUDIT", "ENABLE_COMPRESSION", "false"),
            (
                "SQLSERVER_BACKUP_RESTORE",
                "IAM_ROLE_ARN",
                DEPLOYED_ROLE_ARN,
            ),
        ],
        memberships: &[(
            "SQLSERVER_AUDIT",
            "DBSecurityGroupName",
            "xsvc-og-audit-group",
        )],
    },
];

/// The template one case deploys: the option group, the `AWS::IAM::Role` a case that reaches
/// for one declares, and the outputs the assertions read.
fn template(case: &Case) -> String {
    let configurations: Value = serde_json::from_str(case.configurations)
        .unwrap_or_else(|error| panic!("case {:?}: its declaration is JSON: {error}", case.case));
    let mut resources = serde_json::Map::new();
    let mut outputs = serde_json::Map::new();
    if case.declares_a_role {
        resources.insert(
            "S3IntegrationRole".to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": { "Service": "rds.amazonaws.com" },
                        }],
                    },
                },
            }),
        );
        outputs.insert(
            "RoleArn".to_string(),
            json!({ "Value": { "Fn::GetAtt": ["S3IntegrationRole", "Arn"] } }),
        );
    }
    resources.insert(
        "Options".to_string(),
        json!({
            "Type": "AWS::RDS::OptionGroup",
            "Properties": {
                "EngineName": case.engine,
                "MajorEngineVersion": case.major_version,
                "OptionGroupDescription": "the stack's own option group",
                "OptionConfigurations": configurations,
            },
        }),
    );
    outputs.insert(
        "OptionGroupName".to_string(),
        json!({ "Value": { "Ref": "Options" } }),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an option group whose options carry settings",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// The `<Option>` block of `answered` whose `OptionName` is `option`.
fn option_block<'a>(answered: &'a str, option: &str) -> Option<&'a str> {
    blocks_of(answered, "Option")
        .into_iter()
        .find(|block| opt_element(block, "OptionName").as_deref() == Some(option))
}

/// Rule: every option setting a stack's `OptionConfigurations` declares reaches Amazon RDS, so
/// `DescribeOptionGroups` reports each with the value the template gave it — and a setting the
/// template did not name keeps the value its option publishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cfn_option_groups_declared_settings_carry_their_values() {
    for case in CASES {
        let name = case.case;
        let router = build_router();
        let (settled, answered) = deploy(&router, case.stack, &template(case)).await;
        assert_eq!(
            settled,
            CREATE_COMPLETE,
            "case {name:?}: expected the stack to settle in {CREATE_COMPLETE}, got {settled:?} \
             with resource failures {:?}",
            failure_reasons(&router, case.stack).await
        );
        let group = output(&answered, "OptionGroupName").unwrap_or_else(|| {
            panic!("case {name:?}: expected the stack to publish its option group's name")
        });
        let role_arn = output(&answered, "RoleArn");
        if case.declares_a_role {
            let arn = role_arn.as_deref().unwrap_or_default();
            assert!(
                !arn.is_empty(),
                "case {name:?}: expected the stack to publish its role's ARN, got {role_arn:?}"
            );
        }

        // Amazon RDS's OWN read of the group: the service that holds an option group is where
        // a client reads its settings back.
        let (status, body) = rds(
            &router,
            "DescribeOptionGroups",
            &[("OptionGroupName", &group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected Amazon RDS to hold the option group {group:?}, got {status} \
             with {body}"
        );

        for (option, setting, declared) in case.settings {
            let expected = if *declared == DEPLOYED_ROLE_ARN {
                role_arn.clone().unwrap_or_else(|| {
                    panic!("case {name:?}: the expectation needs the stack's role ARN")
                })
            } else {
                (*declared).to_string()
            };
            let block = option_block(&body, option).unwrap_or_else(|| {
                panic!("case {name:?}: expected the group to carry the option {option:?}: {body}")
            });
            let reported = blocks_of(block, "OptionSetting")
                .into_iter()
                .find(|entry| opt_element(entry, "Name").as_deref() == Some(*setting))
                .map(|entry| opt_element(entry, "Value").unwrap_or_default())
                .unwrap_or_else(|| {
                    panic!(
                        "case {name:?}: expected the option {option:?} to report the setting \
                         {setting:?}: {block}"
                    )
                });
            assert_eq!(
                reported, expected,
                "case {name:?}: expected {option}'s {setting} to be {expected:?}, got \
                 {reported:?}"
            );
        }

        for (option, member, expected) in case.memberships {
            let block = option_block(&body, option).unwrap_or_else(|| {
                panic!("case {name:?}: expected the group to carry the option {option:?}: {body}")
            });
            let reported = elements(block, member);
            assert!(
                reported.iter().any(|value| value == expected),
                "case {name:?}: expected {option} to report {member} {expected:?}, got \
                 {reported:?}"
            );
        }
    }
}

/// Rule, the control on the other side: a declaration carrying an option with NO settings of
/// its own leaves every setting that option publishes at its published default, so what the
/// test above reads back really is the declaration's own doing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_option_declared_without_settings_reports_the_published_defaults() {
    const STACK: &str = "xsvc-og-defaults";
    const CASE: &str = "the memcached option declared with no setting at all";
    let router = build_router();
    let case = Case {
        case: CASE,
        stack: STACK,
        engine: "mysql",
        major_version: "8.0",
        declares_a_role: false,
        configurations: r#"[{ "OptionName": "MEMCACHED" }]"#,
        settings: &[],
        memberships: &[],
    };
    let (settled, answered) = deploy(&router, STACK, &template(&case)).await;
    assert_eq!(
        settled,
        CREATE_COMPLETE,
        "case {CASE:?}: expected the stack to settle in {CREATE_COMPLETE}, got {settled:?} with \
         resource failures {:?}",
        failure_reasons(&router, STACK).await
    );
    let group = output(&answered, "OptionGroupName")
        .unwrap_or_else(|| panic!("case {CASE:?}: expected the stack to publish the group's name"));

    let (status, body) = rds(
        &router,
        "DescribeOptionGroups",
        &[("OptionGroupName", &group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {CASE:?}: expected Amazon RDS to hold the option group {group:?}, got {status} with \
         {body}"
    );
    let block = option_block(&body, "MEMCACHED")
        .unwrap_or_else(|| panic!("case {CASE:?}: expected the group to carry MEMCACHED: {body}"));
    for entry in blocks_of(block, "OptionSetting") {
        let setting = element(entry, "Name");
        let value = opt_element(entry, "Value").unwrap_or_default();
        let default = opt_element(entry, "DefaultValue").unwrap_or_default();
        assert_eq!(
            value, default,
            "case {CASE:?}: expected the unconfigured setting {setting:?} to report its default \
             {default:?}, got {value:?}"
        );
    }
}
