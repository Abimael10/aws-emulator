// SPDX-License-Identifier: CC0-1.0

//! What a client LISTING a stack's change sets sees of the imports it asked for, end to end
//! over the awsQuery wire.
//!
//! RULE: a change set whose IMPORT plan names an identifier the service owning the type does
//! not hold is no plan at all — it settles `FAILED` with `ExecutionStatus` `UNAVAILABLE` and
//! the owning resource provider's own not-found diagnostic as its `StatusReason` — and EVERY
//! read of the stack's change sets reports that verdict, `ListChangeSets`' summary as much as
//! `DescribeChangeSet`, because the summary projects the same stored record. An import whose
//! named resources all exist settles `CREATE_COMPLETE`, available for execution, with no
//! reason and one `Import` change per resource it adopts.
//!
//! The rows differ along what the rule generalises over: which type's provider is called and
//! whether it words the absence itself, whether the resource is really there, whether the
//! identifier comes from a name the TEMPLATE states or only from the import mapping's own
//! `ResourceIdentifier`, and how many resources one mapping names — a mapping of two whose
//! second entry is missing fails on that entry even though the first resolves.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{
    change_set_status, execution_status,
};

use crate::change_set_lifecycle::{create_change_set, describe_change_set, settle_the_store};
use crate::dynamic_references::put_parameter;
use crate::dynamodb_table_resources::dynamodb;
use crate::harness::{
    call, create_stack, member, router_with_engine, xml_blocks, xml_text, xml_texts,
};
use crate::stack_lifecycle::describe_stack;
use crate::stack_resources::element;

/// The stack every change set below is created for, and the resource its own revision
/// declares — so the imports are planned against a stack that already manages something.
const STACK: &str = "listed-import-change-sets";
const MARKER_LOGICAL_ID: &str = "Marker";
const MARKER_PARAMETER: &str = "/listed-import/marker";

/// The resources a client created out of band for the stack to adopt.
const PRESENT_TABLE: &str = "listed-import-orders";
const PRESENT_PARAMETER: &str = "/listed-import/present";

/// ...and the identifiers a mapping could name that nothing ever created.
const ABSENT_TABLE: &str = "listed-import-absent-table";
const ABSENT_PARAMETER: &str = "/listed-import/absent";
const ABSENT_BUCKET: &str = "listed-import-absent-bucket";

/// One declaration an import's template carries beside the stack's own: the logical id the
/// mapping names it by, its type, and the properties the template spells for it.
struct Declared {
    logical_id: &'static str,
    resource_type: &'static str,
    properties: &'static str,
}

/// One `ResourcesToImport` entry: the declaration it adopts, and the identifier it names the
/// existing resource by.
struct Mapped {
    logical_id: &'static str,
    resource_type: &'static str,
    /// The type's primary identifier property, which is what a mapping file keys an entry by.
    identifier_key: &'static str,
    identifier: &'static str,
}

/// One row: an IMPORT change set of the stack, and what every read of the stack's change sets
/// must report for it.
struct Row {
    case: &'static str,
    change_set: &'static str,
    declarations: &'static [Declared],
    mapping: &'static [Mapped],
    status: &'static str,
    execution_status: &'static str,
    /// The `StatusReason` the change set must carry, empty where it carries none.
    reason: String,
}

/// A DynamoDB table declared the way the CDK synthesizes one it is importing: the mapping
/// file, not the template, is what names it.
const UNNAMED_TABLE: &str = r#""KeySchema":[{"AttributeName":"pk","KeyType":"HASH"}],
    "AttributeDefinitions":[{"AttributeName":"pk","AttributeType":"S"}],
    "BillingMode":"PAY_PER_REQUEST""#;

const UNNAMED_TABLE_DECLARATION: &[Declared] = &[Declared {
    logical_id: "AdoptedTable",
    resource_type: "AWS::DynamoDB::Table",
    properties: UNNAMED_TABLE,
}];

const PRESENT_PARAMETER_DECLARATION: &[Declared] = &[Declared {
    logical_id: "AdoptedParameter",
    resource_type: "AWS::SSM::Parameter",
    properties: r#""Name":"/listed-import/present","Type":"String","Value":"held""#,
}];

const ABSENT_PARAMETER_DECLARATION: &[Declared] = &[Declared {
    logical_id: "AdoptedParameter",
    resource_type: "AWS::SSM::Parameter",
    properties: r#""Name":"/listed-import/absent","Type":"String","Value":"held""#,
}];

const TABLE_AND_BUCKET_DECLARATIONS: &[Declared] = &[
    Declared {
        logical_id: "AdoptedTable",
        resource_type: "AWS::DynamoDB::Table",
        properties: UNNAMED_TABLE,
    },
    Declared {
        logical_id: "AdoptedBucket",
        resource_type: "AWS::S3::Bucket",
        properties: r#""BucketName":"listed-import-absent-bucket""#,
    },
];

/// The template one row submits: the stack's own declaration plus the row's, each imported
/// one carrying the `DeletionPolicy` CloudFormation requires of an import.
fn template(declarations: &[Declared]) -> String {
    let mut resources = vec![format!(
        r#""{MARKER_LOGICAL_ID}":{{"Type":"AWS::SSM::Parameter","Properties":{{
            "Name":"{MARKER_PARAMETER}","Type":"String","Value":"held"}}}}"#
    )];
    for declared in declarations {
        resources.push(format!(
            r#""{}":{{"Type":"{}","DeletionPolicy":"Retain","Properties":{{{}}}}}"#,
            declared.logical_id, declared.resource_type, declared.properties
        ));
    }
    format!(r#"{{"Resources":{{{}}}}}"#, resources.join(","))
}

/// The `ResourcesToImport` members a mapping is sent as, in the order it names its entries.
fn mapping_members(mapping: &[Mapped]) -> Vec<(String, String)> {
    let mut members = Vec::new();
    for (index, entry) in mapping.iter().enumerate() {
        let position = index + 1;
        members.push(member(
            &format!("ResourcesToImport.member.{position}.ResourceType"),
            entry.resource_type,
        ));
        members.push(member(
            &format!("ResourcesToImport.member.{position}.LogicalResourceId"),
            entry.logical_id,
        ));
        members.push(member(
            &format!("ResourcesToImport.member.{position}.ResourceIdentifier.entry.1.key"),
            entry.identifier_key,
        ));
        members.push(member(
            &format!("ResourcesToImport.member.{position}.ResourceIdentifier.entry.1.value"),
            entry.identifier,
        ));
    }
    members
}

/// What one `ChangeSetSummary` of a `ListChangeSets` answer reports about a change set's
/// outcome: the name it was created under, the two statuses it has reached, and the reason
/// that came with them.
type Summarised = (String, String, String, String);

/// Every summary `ListChangeSets` reports for `stack`, read inside its own `<member>` block so
/// each status stays paired with its own change set.
async fn listed(router: &MultiServiceRouter, stack: &str) -> Vec<Summarised> {
    let (code, body) = call(router, "ListChangeSets", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "the stack's change sets must be listable, got {code} {body}"
    );
    let summaries = xml_blocks(&body, "Summaries")
        .into_iter()
        .next()
        .unwrap_or_default();
    xml_blocks(&summaries, "member")
        .iter()
        .map(|entry| {
            (
                element(entry, "ChangeSetName").unwrap_or_default(),
                element(entry, "Status").unwrap_or_default(),
                element(entry, "ExecutionStatus").unwrap_or_default(),
                element(entry, "StatusReason").unwrap_or_default(),
            )
        })
        .collect()
}

/// The rows: the imports the stack is asked for, and the verdict each one earns.
///
/// Each not-found sentence is the one the type's OWN resource provider states for a resource
/// it cannot read — Amazon DynamoDB and Amazon S3 word the absence themselves, while a type
/// whose handler reports through the generic resource-provider base class is reported in the
/// uniform "Resource of type '<type>' with identifier '<id>' was not found."
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a table the mapping file names that nothing holds",
            change_set: "import-absent-table",
            declarations: UNNAMED_TABLE_DECLARATION,
            mapping: &[Mapped {
                logical_id: "AdoptedTable",
                resource_type: "AWS::DynamoDB::Table",
                identifier_key: "TableName",
                identifier: ABSENT_TABLE,
            }],
            status: change_set_status::FAILED,
            execution_status: execution_status::UNAVAILABLE,
            reason: format!("Table: {ABSENT_TABLE} does not exist."),
        },
        Row {
            case: "a table the mapping file names that IS there",
            change_set: "import-present-table",
            declarations: UNNAMED_TABLE_DECLARATION,
            mapping: &[Mapped {
                logical_id: "AdoptedTable",
                resource_type: "AWS::DynamoDB::Table",
                identifier_key: "TableName",
                identifier: PRESENT_TABLE,
            }],
            status: change_set_status::CREATE_COMPLETE,
            execution_status: execution_status::AVAILABLE,
            reason: String::new(),
        },
        Row {
            case: "a parameter the TEMPLATE names that nothing holds",
            change_set: "import-absent-parameter",
            declarations: ABSENT_PARAMETER_DECLARATION,
            mapping: &[Mapped {
                logical_id: "AdoptedParameter",
                resource_type: "AWS::SSM::Parameter",
                identifier_key: "Name",
                identifier: ABSENT_PARAMETER,
            }],
            status: change_set_status::FAILED,
            execution_status: execution_status::UNAVAILABLE,
            reason: format!(
                "Resource of type 'AWS::SSM::Parameter' with identifier '{ABSENT_PARAMETER}' \
                 was not found."
            ),
        },
        Row {
            case: "a parameter the TEMPLATE names that IS there",
            change_set: "import-present-parameter",
            declarations: PRESENT_PARAMETER_DECLARATION,
            mapping: &[Mapped {
                logical_id: "AdoptedParameter",
                resource_type: "AWS::SSM::Parameter",
                identifier_key: "Name",
                identifier: PRESENT_PARAMETER,
            }],
            status: change_set_status::CREATE_COMPLETE,
            execution_status: execution_status::AVAILABLE,
            reason: String::new(),
        },
        Row {
            case: "a mapping of two whose second entry nothing holds",
            change_set: "import-one-of-two-absent",
            declarations: TABLE_AND_BUCKET_DECLARATIONS,
            mapping: &[
                Mapped {
                    logical_id: "AdoptedTable",
                    resource_type: "AWS::DynamoDB::Table",
                    identifier_key: "TableName",
                    identifier: PRESENT_TABLE,
                },
                Mapped {
                    logical_id: "AdoptedBucket",
                    resource_type: "AWS::S3::Bucket",
                    identifier_key: "BucketName",
                    identifier: ABSENT_BUCKET,
                },
            ],
            status: change_set_status::FAILED,
            execution_status: execution_status::UNAVAILABLE,
            reason: "Bucket not found".to_string(),
        },
    ]
}

/// RULE: every read of the stack's change sets reports the FAILED verdict an unresolvable
/// import earned, with the owning provider's own diagnostic — so a client that lists the
/// stack's change sets sees exactly the imports that could not be planned, and the ones whose
/// resources are really there as plans available for execution.
#[tokio::test]
async fn an_unresolvable_import_is_listed_as_a_failed_change_set() {
    let (router, engine) = router_with_engine();

    // The resources a client created out of band, which nothing of CloudFormation's manages.
    let (code, answered) = dynamodb(
        &router,
        "CreateTable",
        &json!({
            "TableName": PRESENT_TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        code, 200,
        "the client's own CreateTable must be accepted, got {code} {answered}"
    );
    put_parameter(
        &router,
        "the parameter a client created out of band",
        PRESENT_PARAMETER,
        "String",
        "held",
    )
    .await;

    // The stack the imports are planned against, deployed from its own revision.
    let (code, answered) = create_stack(&router, STACK, &template(&[]), &[]).await;
    assert_eq!(
        code, 200,
        "the stack the imports are planned for must be created, got {code} {answered}"
    );
    settle_the_store(&router, &engine).await;
    let (_, described) = describe_stack(&router, STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "the stack must reach CREATE_COMPLETE before the imports, got {described}"
    );

    let rows = rows();
    for row in &rows {
        let case = row.case;
        let mut members = vec![
            member("ChangeSetType", "IMPORT"),
            member("TemplateBody", &template(row.declarations)),
        ];
        members.extend(mapping_members(row.mapping));
        let (code, answered) = create_change_set(&router, STACK, row.change_set, &members).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the change set must be created, got {code} {answered}"
        );
    }
    settle_the_store(&router, &engine).await;

    // What a client LISTING the stack's change sets sees: one summary per change set, each
    // carrying the verdict its own plan earned.
    let reported = listed(&router, STACK).await;
    let expected: Vec<Summarised> = rows
        .iter()
        .map(|row| {
            (
                row.change_set.to_string(),
                row.status.to_string(),
                row.execution_status.to_string(),
                row.reason.clone(),
            )
        })
        .collect();
    assert_eq!(
        reported, expected,
        "the listed change sets must report every import's own verdict: expected \
         {expected:?}, got {reported:?}"
    );

    // ...and the count the whole rule turns on: an import over a resource nothing holds is a
    // FAILED change set a client can find by listing, not a silent success.
    let failed: Vec<&Summarised> = reported
        .iter()
        .filter(|(_, status, _, _)| status == change_set_status::FAILED)
        .collect();
    let expected_failures: Vec<&str> = rows
        .iter()
        .filter(|row| row.status == change_set_status::FAILED)
        .map(|row| row.change_set)
        .collect();
    assert_eq!(
        failed
            .iter()
            .map(|(name, _, _, _)| name.as_str())
            .collect::<Vec<&str>>(),
        expected_failures,
        "the stack must hold one FAILED change set per unresolvable import, got {failed:?}"
    );

    // The summary is a PROJECTION of the record `DescribeChangeSet` reports in full, so both
    // reads answer the same statuses and the same reason — over a plan that reports one
    // `Import` change per resource the mapping names, whichever verdict it settled to.
    for row in &rows {
        let case = row.case;
        let (code, plan) = describe_change_set(&router, row.change_set, Some(STACK)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the change set must be describable, got {code} {plan}"
        );
        let described = (
            xml_text(&plan, "Status").unwrap_or_default(),
            xml_text(&plan, "ExecutionStatus").unwrap_or_default(),
            xml_text(&plan, "StatusReason").unwrap_or_default(),
        );
        let wanted = (
            row.status.to_string(),
            row.execution_status.to_string(),
            row.reason.clone(),
        );
        assert_eq!(
            described, wanted,
            "case {case:?}: the described change set must agree with its summary: expected \
             {wanted:?}, got {described:?} in {plan}"
        );
        let actions = xml_texts(&plan, "Action");
        let wanted_actions: Vec<String> =
            row.mapping.iter().map(|_| "Import".to_string()).collect();
        assert_eq!(
            actions, wanted_actions,
            "case {case:?}: expected {wanted_actions:?}, got {actions:?} in {plan}"
        );
    }
}
