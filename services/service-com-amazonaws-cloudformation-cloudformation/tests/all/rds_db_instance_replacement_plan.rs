// SPDX-License-Identifier: CC0-1.0

//! What a change set says a change to an `AWS::RDS::DBInstance` property would do to the DB
//! instance, end to end over the awsQuery wire.
//!
//! **The rule every row holds the service to:** a property of a resource type whose owning
//! service has no call that changes it on the resource that exists — for a DB instance, a
//! member of `CreateDBInstance` that is a member of no `ModifyDBInstance`
//! (`smithy_query(query="operation",operation="CreateDBInstance,ModifyDBInstance")` on the
//! Amazon RDS model) — cannot take a new value while the instance goes on existing, so a
//! change set reports that change with `RequiresRecreation` `Always` and the resource's
//! `Replacement` `True`; a property that modification DOES take is a change to the instance
//! the stack already has, which is `Never` and `False`.
//!
//! The rows span that classification rather than the one recorded property: the database the
//! instance hosts, the master user it is reached as, whether its storage is encrypted, the
//! character set it stores text in — four create-only properties, only one of which the
//! recorded trace changes — and, the other side of the rule, three properties
//! `ModifyDBInstance` takes (the storage it is given, the backups it keeps and the class it
//! runs on), plus a row that changes one of each at once, where the plan's `Replacement` is
//! decided by the change with the most impact.
//!
//! Every read goes through the real request path (form-encoded body in, XML envelope out),
//! and each row's stack really creates its DB instance in Amazon RDS, mounted beside
//! CloudFormation on the one engine.

use service_com_amazonaws_cloudformation_cloudformation::state::change_action;
use service_com_amazonaws_cloudformation_cloudformation::state::change_plan::{
    change_source, evaluation, replacement, requires_recreation,
};

use crate::change_set_lifecycle::settle_the_store;
use crate::stack_lifecycle::{
    call, create_stack, member, output_value, rds, router_with_engine, settled_stack,
    wait_for_the_transition, xml_blocks, xml_text,
};

/// The logical id of the DB instance every row plans a change to.
const INSTANCE: &str = "Database";

/// The revision every row starts from: one mysql DB instance declaring the create-only
/// properties the rows edit beside the ones `ModifyDBInstance` takes.
///
/// The instance is really created in Amazon RDS by the deployment, so the declaration states
/// what that create needs and nothing a row does not read.
const BASE_TEMPLATE: &str = r#"{"Resources":{
     "Database":{"Type":"AWS::RDS::DBInstance","Properties":{
        "Engine":"mysql",
        "DBInstanceClass":"db.t3.micro",
        "DBName":"appa",
        "MasterUsername":"appadmin",
        "MasterUserPassword":"an-example-master-password",
        "CharacterSetName":"utf8",
        "StorageEncrypted":false,
        "AllocatedStorage":"20",
        "BackupRetentionPeriod":1}}}}"#;

/// [`BASE_TEMPLATE`] with each `(original, edited)` spliced in, which is how a row states
/// "the same revision, with this edit".
fn edited(edits: &[(&str, &str)]) -> String {
    let mut template = BASE_TEMPLATE.to_string();
    for (original, edit) in edits {
        assert!(
            template.contains(original),
            "the base revision must carry {original:?} for a row to edit it"
        );
        template = template.replace(original, edit);
    }
    template
}

/// One `ResourceChangeDetail` as the response carries it, flattened for comparison.
#[derive(Debug, PartialEq, Eq)]
struct Detail {
    name: Option<String>,
    requires_recreation: Option<String>,
}

/// The expected form of one detail, as a row spells it.
fn detail(name: &str, requires_recreation: &str) -> Detail {
    Detail {
        name: Some(name.to_string()),
        requires_recreation: Some(requires_recreation.to_string()),
    }
}

/// Every `Properties` detail of the one `ResourceChange` the response carries, in response
/// order, beside that change's `Action` and `Replacement`.
fn reported_change(xml: &str) -> (Option<String>, Option<String>, Vec<Detail>) {
    let changes = xml_blocks(xml, "ResourceChange");
    assert_eq!(
        changes.len(),
        1,
        "expected exactly one planned change, got {xml}"
    );
    let change = &changes[0];
    let details = xml_blocks(change, "Details")
        .first()
        .map(|details| {
            xml_blocks(details, "member")
                .iter()
                .map(|entry| {
                    // Every row edits a `Properties` property and states its new value
                    // outright, so the attribute and the evaluation are the same on each
                    // detail and are asserted here rather than spelled on every row.
                    assert_eq!(
                        xml_text(entry, "Attribute").as_deref(),
                        Some("Properties"),
                        "expected a Properties detail, got {entry}"
                    );
                    assert_eq!(
                        xml_text(entry, "Evaluation").as_deref(),
                        Some(evaluation::STATIC),
                        "expected a determined new value, got {entry}"
                    );
                    assert_eq!(
                        xml_text(entry, "ChangeSource").as_deref(),
                        Some(change_source::DIRECT_MODIFICATION),
                        "expected a direct modification, got {entry}"
                    );
                    Detail {
                        name: xml_text(entry, "Name"),
                        requires_recreation: xml_text(entry, "RequiresRecreation"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    (
        xml_text(change, "Action"),
        xml_text(change, "Replacement"),
        details,
    )
}

/// One row: an edit to the deployed revision, and what the plan must say about the DB
/// instance it changes.
struct Case {
    case: &'static str,
    stack: &'static str,
    edits: &'static [(&'static str, &'static str)],
    replacement: &'static str,
    details: &'static [(&'static str, &'static str)],
}

/// The class the rule generalises over: each create-only property of a DB instance the base
/// revision states, each modifiable one, and the two kinds together.
const CASES: &[Case] = &[
    // The recorded trace's own property: the DATABASE the instance hosts is created with the
    // instance and `ModifyDBInstance` takes no `DBName` at all.
    Case {
        case: "the database the instance hosts",
        stack: "rds-replacement-dbname",
        edits: &[(r#""DBName":"appa""#, r#""DBName":"appb""#)],
        replacement: replacement::TRUE,
        details: &[("DBName", requires_recreation::ALWAYS)],
    },
    Case {
        case: "the master user the instance is reached as",
        stack: "rds-replacement-master-username",
        edits: &[(
            r#""MasterUsername":"appadmin""#,
            r#""MasterUsername":"otheradmin""#,
        )],
        replacement: replacement::TRUE,
        details: &[("MasterUsername", requires_recreation::ALWAYS)],
    },
    Case {
        case: "whether the instance's storage is encrypted",
        stack: "rds-replacement-storage-encrypted",
        edits: &[(r#""StorageEncrypted":false"#, r#""StorageEncrypted":true"#)],
        replacement: replacement::TRUE,
        details: &[("StorageEncrypted", requires_recreation::ALWAYS)],
    },
    Case {
        case: "the character set the instance stores text in",
        stack: "rds-replacement-character-set",
        edits: &[(
            r#""CharacterSetName":"utf8""#,
            r#""CharacterSetName":"latin1""#,
        )],
        replacement: replacement::TRUE,
        details: &[("CharacterSetName", requires_recreation::ALWAYS)],
    },
    // The other side of the rule: `ModifyDBInstance` takes the storage an instance is given,
    // the backups it keeps and the class it runs on, so each is a change to the instance that
    // exists.
    Case {
        case: "the storage the instance is given",
        stack: "rds-replacement-allocated-storage",
        edits: &[(r#""AllocatedStorage":"20""#, r#""AllocatedStorage":"25""#)],
        replacement: replacement::FALSE,
        details: &[("AllocatedStorage", requires_recreation::NEVER)],
    },
    Case {
        case: "the backups the instance keeps",
        stack: "rds-replacement-backup-retention",
        edits: &[(
            r#""BackupRetentionPeriod":1"#,
            r#""BackupRetentionPeriod":7"#,
        )],
        replacement: replacement::FALSE,
        details: &[("BackupRetentionPeriod", requires_recreation::NEVER)],
    },
    Case {
        case: "the class the instance runs on",
        stack: "rds-replacement-instance-class",
        edits: &[(
            r#""DBInstanceClass":"db.t3.micro""#,
            r#""DBInstanceClass":"db.t3.small""#,
        )],
        replacement: replacement::FALSE,
        details: &[("DBInstanceClass", requires_recreation::NEVER)],
    },
    // One of each at once: the plan's `Replacement` is the verdict of the change with the
    // most impact, and both details are reported.
    Case {
        case: "the database the instance hosts and the backups it keeps",
        stack: "rds-replacement-dbname-and-backups",
        edits: &[
            (r#""DBName":"appa""#, r#""DBName":"appb""#),
            (
                r#""BackupRetentionPeriod":1"#,
                r#""BackupRetentionPeriod":7"#,
            ),
        ],
        replacement: replacement::TRUE,
        details: &[
            ("BackupRetentionPeriod", requires_recreation::NEVER),
            ("DBName", requires_recreation::ALWAYS),
        ],
    },
];

/// The stack an execution row deploys: one unnamed mysql DB instance whose database the
/// update changes, publishing the handle and the values a client reads it by.
const EXECUTED_TEMPLATE: &str = r#"{"Resources":{
     "Database":{"Type":"AWS::RDS::DBInstance","Properties":{
        "Engine":"mysql",
        "DBInstanceClass":"db.t3.micro",
        "DBName":"appa",
        "MasterUsername":"appadmin",
        "MasterUserPassword":"an-example-master-password"}}},
   "Outputs":{
     "InstanceId":{"Value":{"Ref":"Database"}},
     "ResourceId":{"Value":{"Fn::GetAtt":["Database","DbiResourceId"]}},
     "Port":{"Value":{"Fn::GetAtt":["Database","Endpoint.Port"]}}}}"#;

#[tokio::test]
async fn executing_a_db_name_change_deploys_a_new_db_instance_and_takes_the_old_one_down() {
    let (router, _engine) = router_with_engine();
    let case = "a DBName change deployed";
    let stack = "rds-replacement-executed";
    let (code, body) = create_stack(&router, stack, EXECUTED_TEMPLATE, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack must be created, got {code} {body}"
    );
    let created = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&created, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: got {created}"
    );
    let first_id = output_value(&created, "InstanceId").expect("the stack publishes the Ref");
    let first_resource_id =
        output_value(&created, "ResourceId").expect("the stack publishes the resource id");
    let first_port = output_value(&created, "Port").expect("the stack publishes the port");

    // The same declaration with another database: a change no `ModifyDBInstance` can make.
    let (code, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member(
                "TemplateBody",
                &EXECUTED_TEMPLATE.replace(r#""DBName":"appa""#, r#""DBName":"appb""#),
            ),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the update must be accepted, got {code} {body}"
    );
    let updated = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&updated, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: got {updated}"
    );

    // The stack publishes the instance the update created, not the one it replaced.
    let second_id = output_value(&updated, "InstanceId").expect("the stack publishes the Ref");
    let second_resource_id =
        output_value(&updated, "ResourceId").expect("the stack publishes the resource id");
    let second_port = output_value(&updated, "Port").expect("the stack publishes the port");
    assert_ne!(
        second_id, first_id,
        "case {case:?}: a replacement is a new DB instance, got {updated}"
    );
    assert_ne!(
        second_resource_id, first_resource_id,
        "case {case:?}: the new instance carries its own resource id, got {updated}"
    );
    assert_ne!(
        second_port, first_port,
        "case {case:?}: the new instance serves its own endpoint, got {updated}"
    );

    // ROUND TRIP through Amazon RDS's own wire: the instance the stack now publishes serves
    // the database the update declared, and the one it replaced is gone.
    let (code, reported) = rds(
        &router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", second_id.as_str())],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: the new instance must be readable, got {code} {reported}"
    );
    assert_eq!(
        xml_text(&reported, "DBName").as_deref(),
        Some("appb"),
        "case {case:?}: got {reported}"
    );
    let (code, gone) = rds(
        &router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", first_id.as_str())],
    )
    .await;
    assert_eq!(
        (code, xml_text(&gone, "Code").as_deref()),
        (404, Some("DBInstanceNotFound")),
        "case {case:?}: the replaced instance must be taken down, got {code} {gone}"
    );
}

#[tokio::test]
async fn a_change_set_reports_what_each_changed_db_instance_property_does_to_the_instance() {
    let (router, engine) = router_with_engine();
    for case in CASES {
        let name = case.case;
        let (code, body) = create_stack(&router, case.stack, BASE_TEMPLATE, &[]).await;
        assert_eq!(
            code, 200,
            "case {name:?}: the stack the change set is for must be created, got {code} {body}"
        );
        // Amazon RDS answers a new DB instance `creating` and reaches `available` on its own,
        // and the stack's create is held open until it does, so the deployment is given past
        // that instant before the change set is asked for.
        wait_for_the_transition().await;
        settle_the_store(&router, &engine).await;

        let (code, body) = call(
            &router,
            "CreateChangeSet",
            &[
                member("StackName", case.stack),
                member("ChangeSetName", "replacement"),
                member("TemplateBody", &edited(case.edits)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {name:?}: the change set must be created, got {code} {body}"
        );
        settle_the_store(&router, &engine).await;

        let (code, body) = call(
            &router,
            "DescribeChangeSet",
            &[
                member("ChangeSetName", "replacement"),
                member("StackName", case.stack),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {name:?}: the change set must be describable, got {code} {body}"
        );
        assert_eq!(
            xml_text(&body, "LogicalResourceId").as_deref(),
            Some(INSTANCE),
            "case {name:?}: expected the change of {INSTANCE:?}, got {body}"
        );

        let (action, replaces, details) = reported_change(&body);
        assert_eq!(
            action.as_deref(),
            Some(change_action::MODIFY),
            "case {name:?}: expected a Modify, got {action:?} in {body}"
        );
        assert_eq!(
            replaces.as_deref(),
            Some(case.replacement),
            "case {name:?}: expected Replacement {:?}, got {replaces:?} in {body}",
            case.replacement
        );
        let expected: Vec<Detail> = case
            .details
            .iter()
            .map(|(property, recreation)| detail(property, recreation))
            .collect();
        assert_eq!(
            details, expected,
            "case {name:?}: expected Details {expected:?}, got {details:?} in {body}"
        );
    }
}
