// SPDX-License-Identifier: CC0-1.0

//! The reason a stack's event carries for a resource whose physical name the stack already
//! holds, read from a deployment whose rollback was suppressed, end to end over the awsQuery
//! wire.
//!
//! RULE: a deployment that would create a resource under a physical name a resource of the
//! SAME stack already holds is one CLOUDFORMATION refuses itself — nothing is asked of the
//! service that owns the type — so the resource's `CREATE_FAILED` event carries
//! CloudFormation's own reason, `<physical name> already exists in stack <that stack's id>`,
//! and never a resource handler's wrapped report. The reason is a value a client compares on,
//! so it carries only the colliding name (from the request's own template) and the owning
//! stack's id (from the emulator's state): nothing of the one request that happened to ask —
//! no request token, no handler error code.
//!
//! `cross_stack_name_collision.rs` owns the half of that rule where ANOTHER stack of the
//! Region holds the name, and `change_set_import_and_drift.rs` reads the same reason off one
//! change-set leg. What this suite is the table for is the DEPLOYMENT side of the same-stack
//! half: the reason read where a client reads it, off a stack parked in `CREATE_FAILED`,
//! because a request that suppressed its rollback leaves the failed resource's event there to
//! be read ("Specifies whether to disable rollback of the stack if the stack operation fails",
//! `CreateStackInput$DisableRollback`).
//!
//! The rows differ along what the rule generalises over: the type whose namespace the
//! contested name is in (and so which service would have been asked), how the request
//! suppressed the rollback (`DisableRollback=true`, `OnFailure=DO_NOTHING`), which call
//! deployed the template (`CreateStack`, `ExecuteChangeSet`), whether the colliding
//! declaration's logical id sorts before or after the holder's, and whether the template also
//! carries a resource type CloudFormation serves itself — a legacy-synthesizer CDK template's
//! `AWS::CDK::Metadata`, which is accepted beside the rest and must not turn the deployment
//! into a template-format refusal. A control row whose two names DIFFER deploys clean, so no
//! row passes against a deployment that fails every second resource.
//!
//! Every row's colliding declaration states `DependsOn` the holder, so WHICH of the two
//! declarations ends up holding the name is decided by the template's own dependency graph —
//! "a resource that depends on another is scheduled only once that one has finished" — and not
//! by whatever order the crate happens to walk a template's declarations in.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::CDK_METADATA;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

use crate::change_set_lifecycle::{create_change_set, settle_the_store};
use crate::cross_stack_name_collision::HANDLER_WRAPPER;
use crate::harness::{call, create_stack, member, router_with_engine, xml_text};
use crate::stack_lifecycle::resource_event_reason;
use crate::update_rollback_recovery::{parameter_value, resource_status_of, template};

/// The default pair of logical ids: the resource that HOLDS the contested name, and the
/// declaration that asks for it a second time. `"Bad"` sorts BEFORE `"Keep"`, so these rows
/// are the ones whose colliding declaration would be walked first were the order the ids'
/// rather than the `DependsOn` the template states.
const HOLDER: &str = "Keep";
const COLLIDING: &str = "Bad";

/// The other pair, for the rows that sort the other way round: `"Alpha"` before `"Zeta"`, so
/// the holder's id is the one that sorts first.
const SORTED_FIRST_HOLDER: &str = "Alpha";
const SORTED_LAST_COLLIDING: &str = "Zeta";

/// The logical id the AWS CDK gives the resource it adds to every stack it synthesizes —
/// declared by the row that carries a legacy-synthesizer template's inert metadata resource.
const CDK_LOGICAL_ID: &str = "CDKMetadata";

/// The two per-request values a report by the service that OWNS the type carries: neither
/// belongs in CloudFormation's own reason. (What such a report OPENS with is spelled once, in
/// `cross_stack_name_collision.rs`, and imported above.)
const REQUEST_TOKEN: &str = "RequestToken:";
const HANDLER_ERROR_CODE: &str = "HandlerErrorCode:";

/// The type the rows that read the deployed resource back through its owning service declare.
const PARAMETER: &str = "AWS::SSM::Parameter";

/// How one row deploys its template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeployedBy {
    /// `CreateStack`, with the row's own members on the call.
    CreateStack,
    /// A `CREATE` change set, executed with `DisableRollback=true` — "the `DisableRollback`
    /// parameter" is `ExecuteChangeSet`'s own member.
    ExecutedChangeSet,
}

/// One row: the template to deploy, how it is deployed, and the contested name.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The two declarations' type, so the row can say which service would have been asked.
    resource_type: &'static str,
    /// The name the HOLDER carries, which is what the collision is about.
    contested: String,
    /// The name the second declaration asks for: the same as `contested` for every row but the
    /// control.
    asked_for: String,
    /// The logical ids of the holder and of the declaration that asks for its name.
    holder_id: &'static str,
    colliding_id: &'static str,
    resources: Value,
    deployed_by: DeployedBy,
    /// The request members that suppress the rollback, for a `CreateStack` row.
    suppressed_by: Vec<(String, String)>,
    /// Whether the row's template also declares the inert `AWS::CDK::Metadata` resource.
    carries_cdk_metadata: bool,
    /// Whether the row's two names collide at all.
    collides: bool,
}

/// A declaration of `resource_type` whose name property carries `name`.
///
/// The name property of each type is the one the CloudFormation resource and property types
/// reference publishes for it: `Name` for `AWS::SSM::Parameter`, `BucketName` for
/// `AWS::S3::Bucket`, `LogGroupName` for `AWS::Logs::LogGroup`.
fn named(resource_type: &str, name: &str, value: &str) -> Value {
    let properties = match resource_type {
        PARAMETER => json!({"Name": name, "Type": "String", "Value": value}),
        "AWS::S3::Bucket" => json!({"BucketName": name}),
        "AWS::Logs::LogGroup" => json!({"LogGroupName": name}),
        other => panic!("no name property is spelled here for {other}"),
    };
    json!({"Type": resource_type, "Properties": properties})
}

/// The `Resources` section of one row: the holder, the second declaration, and — where the row
/// asks for it — the inert resource a legacy-synthesizer CDK template carries.
///
/// The second declaration always states `DependsOn` the holder, so the holder is the resource
/// that gets the name and the collision is found against it whatever order the walk would
/// otherwise have taken the two declarations in.
fn resources(
    resource_type: &str,
    contested: &str,
    asked_for: &str,
    (holder_id, colliding_id): (&str, &str),
    cdk_metadata: bool,
) -> Value {
    let mut colliding = named(resource_type, asked_for, "the-second-declaration");
    colliding
        .as_object_mut()
        .expect("a resource declaration is an object")
        .insert("DependsOn".to_string(), json!(holder_id));
    let mut declared = Map::new();
    declared.insert(
        holder_id.to_string(),
        named(resource_type, contested, "the-holder"),
    );
    declared.insert(colliding_id.to_string(), colliding);
    if cdk_metadata {
        declared.insert(
            CDK_LOGICAL_ID.to_string(),
            json!({
                "Type": CDK_METADATA,
                "Properties": {"Analytics": "v2:deflate64:H4sIAAAAAAAA"},
            }),
        );
    }
    Value::Object(declared)
}

/// Build one row from what it varies.
struct Varies {
    case: &'static str,
    stack: &'static str,
    resource_type: &'static str,
    contested: &'static str,
    asked_for: &'static str,
    deployed_by: DeployedBy,
    suppressed_by: Vec<(String, String)>,
    /// `(the holder's logical id, the colliding declaration's)`.
    ids: (&'static str, &'static str),
    carries_cdk_metadata: bool,
}

impl From<Varies> for Row {
    fn from(varies: Varies) -> Self {
        Row {
            case: varies.case,
            stack: varies.stack,
            resource_type: varies.resource_type,
            contested: varies.contested.to_string(),
            asked_for: varies.asked_for.to_string(),
            holder_id: varies.ids.0,
            colliding_id: varies.ids.1,
            resources: resources(
                varies.resource_type,
                varies.contested,
                varies.asked_for,
                varies.ids,
                varies.carries_cdk_metadata,
            ),
            deployed_by: varies.deployed_by,
            suppressed_by: varies.suppressed_by,
            carries_cdk_metadata: varies.carries_cdk_metadata,
            collides: varies.contested == varies.asked_for,
        }
    }
}

/// The class the rule covers.
fn rows() -> Vec<Row> {
    let disable_rollback = || vec![member("DisableRollback", "true")];
    vec![
        Varies {
            case: "a parameter name the stack already holds, DisableRollback=true",
            stack: "kept-collide-param",
            resource_type: PARAMETER,
            contested: "/kept-collide-param/held",
            asked_for: "/kept-collide-param/held",
            deployed_by: DeployedBy::CreateStack,
            suppressed_by: disable_rollback(),
            ids: (HOLDER, COLLIDING),
            carries_cdk_metadata: false,
        }
        .into(),
        // "DO_NOTHING - if the stack creation fails, do nothing. This is equivalent to
        // specifying true for the DisableRollback parameter."
        Varies {
            case: "a parameter name the stack already holds, OnFailure=DO_NOTHING",
            stack: "kept-collide-do-nothing",
            resource_type: PARAMETER,
            contested: "/kept-collide-do-nothing/held",
            asked_for: "/kept-collide-do-nothing/held",
            deployed_by: DeployedBy::CreateStack,
            suppressed_by: vec![member("OnFailure", "DO_NOTHING")],
            ids: (HOLDER, COLLIDING),
            carries_cdk_metadata: false,
        }
        .into(),
        // Another namespace, and the pair of logical ids that sorts the other way round.
        Varies {
            case: "a bucket name the stack already holds, the holder's id sorting first",
            stack: "kept-collide-bucket",
            resource_type: "AWS::S3::Bucket",
            contested: "kept-collide-bucket-held",
            asked_for: "kept-collide-bucket-held",
            deployed_by: DeployedBy::CreateStack,
            suppressed_by: disable_rollback(),
            ids: (SORTED_FIRST_HOLDER, SORTED_LAST_COLLIDING),
            carries_cdk_metadata: false,
        }
        .into(),
        // Another namespace again, and another service that is never asked.
        Varies {
            case: "a log group name the stack already holds",
            stack: "kept-collide-logs",
            resource_type: "AWS::Logs::LogGroup",
            contested: "/kept/collide/logs/held",
            asked_for: "/kept/collide/logs/held",
            deployed_by: DeployedBy::CreateStack,
            suppressed_by: disable_rollback(),
            ids: (SORTED_FIRST_HOLDER, SORTED_LAST_COLLIDING),
            carries_cdk_metadata: false,
        }
        .into(),
        // The legacy-synthesizer CDK template shape: an inert resource type CloudFormation
        // serves itself, beside the collision.
        Varies {
            case: "a template carrying the CDK's inert metadata resource beside the collision",
            stack: "kept-collide-cdk",
            resource_type: PARAMETER,
            contested: "/kept-collide-cdk/held",
            asked_for: "/kept-collide-cdk/held",
            deployed_by: DeployedBy::CreateStack,
            suppressed_by: disable_rollback(),
            ids: (HOLDER, COLLIDING),
            carries_cdk_metadata: true,
        }
        .into(),
        // The same record answers the same way whichever call deploys the template.
        Varies {
            case: "a change set executed with the rollback disabled",
            stack: "kept-collide-change-set",
            resource_type: PARAMETER,
            contested: "/kept-collide-change-set/held",
            asked_for: "/kept-collide-change-set/held",
            deployed_by: DeployedBy::ExecutedChangeSet,
            suppressed_by: Vec::new(),
            ids: (HOLDER, COLLIDING),
            carries_cdk_metadata: false,
        }
        .into(),
        // The CONTROL: two declarations of one type whose names DIFFER both deploy.
        Varies {
            case: "two parameters of one stack whose names differ",
            stack: "kept-collide-none",
            resource_type: PARAMETER,
            contested: "/kept-collide-none/held",
            asked_for: "/kept-collide-none/other",
            deployed_by: DeployedBy::CreateStack,
            suppressed_by: disable_rollback(),
            ids: (HOLDER, COLLIDING),
            carries_cdk_metadata: false,
        }
        .into(),
    ]
}

/// Deploy one row's template and answer the stack's own id.
async fn deploy(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    row: &Row,
) -> String {
    let case = row.case;
    let body = template(row.resources.clone());
    match row.deployed_by {
        DeployedBy::CreateStack => {
            let (code, answer) = create_stack(router, row.stack, &body, &row.suppressed_by).await;
            assert_eq!(
                code, 200,
                "case {case:?}: CreateStack must be accepted — the creation starts and the \
                 stack reports the outcome — got {code} {answer}"
            );
        }
        DeployedBy::ExecutedChangeSet => {
            let (code, answer) = create_change_set(
                router,
                row.stack,
                "the-plan",
                &[
                    member("ChangeSetType", "CREATE"),
                    member("TemplateBody", &body),
                ],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: the change set must be created, got {code} {answer}"
            );
            settle_the_store(router, engine).await;
            let (code, answer) = call(
                router,
                "ExecuteChangeSet",
                &[
                    member("ChangeSetName", "the-plan"),
                    member("StackName", row.stack),
                    member("DisableRollback", "true"),
                ],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: the plan must be executable, got {code} {answer}"
            );
        }
    }
    // Once for the deployment's own conclusion, once for anything it owes after it.
    settle_the_store(router, engine).await;
    settle_the_store(router, engine).await;
    let (code, answer) = call(router, "DescribeStacks", &[member("StackName", row.stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStacks must answer 200, got {code} {answer}"
    );
    let reached = xml_text(&answer, "StackStatus").unwrap_or_default();
    let wanted = if row.collides {
        status::CREATE_FAILED
    } else {
        status::CREATE_COMPLETE
    };
    assert_eq!(
        reached, wanted,
        "case {case:?}: expected the deployment to leave the stack {wanted}, got {reached:?} \
         ({answer})"
    );
    xml_text(&answer, "StackId").unwrap_or_else(|| panic!("case {case:?}: a StackId"))
}

#[tokio::test]
async fn a_name_the_stack_itself_holds_fails_the_resource_in_cloudformations_own_words() {
    for row in rows() {
        let case = row.case;
        let (router, engine) = router_with_engine();
        let stack_id = deploy(&router, &engine, &row).await;

        // The holder is the declaration the other one depends on, so it is the one that gets
        // the name.
        let held = resource_status_of(&router, row.stack, row.holder_id).await;
        assert_eq!(
            held,
            resource_status::CREATE_COMPLETE,
            "case {case:?}: expected the resource holding {:?} to be created, got {held:?}",
            row.contested
        );

        if !row.collides {
            let other = resource_status_of(&router, row.stack, row.colliding_id).await;
            assert_eq!(
                other,
                resource_status::CREATE_COMPLETE,
                "case {case:?}: expected the second declaration, asking for {:?}, to deploy \
                 too, got {other:?}",
                row.asked_for
            );
            continue;
        }

        // The failure is CloudFormation's own, worded as CloudFormation words it: the fixed
        // text spelled out here, and its two variable parts taken from the request's own
        // template (the contested name) and from the emulator's state (the id of the stack
        // that holds it, read back through DescribeStacks).
        let reason = resource_event_reason(
            &router,
            row.stack,
            row.colliding_id,
            resource_status::CREATE_FAILED,
        )
        .await;
        let expected = format!("{} already exists in stack {stack_id}", row.contested);
        assert_eq!(
            reason, expected,
            "case {case:?}: expected the collision reason {expected:?}, got {reason:?}"
        );
        // ...so it is not the owning service's report, and nothing of the one request that
        // asked is in it.
        for leak in [HANDLER_WRAPPER, REQUEST_TOKEN, HANDLER_ERROR_CODE] {
            assert!(
                !reason.contains(leak),
                "case {case:?}: expected no {leak:?} in a reason clients compare on, got \
                 {reason:?}"
            );
        }
        // The stack parks with the failed resource's record still on it, which is what makes
        // the event above readable at all.
        let failed = resource_status_of(&router, row.stack, row.colliding_id).await;
        assert_eq!(
            failed,
            resource_status::CREATE_FAILED,
            "case {case:?}: expected the colliding resource to stay CREATE_FAILED on the \
             parked stack, got {failed:?}"
        );
        // The resource the holder created is untouched by the refused one: nothing of the
        // second declaration reached the service that owns the type.
        if row.resource_type == PARAMETER {
            let value = parameter_value(&router, &row.contested).await;
            assert_eq!(
                value.as_deref(),
                Some("the-holder"),
                "case {case:?}: expected Parameter Store to still hold the holder's own value \
                 under {:?}, got {value:?}",
                row.contested
            );
        }
        // A template carrying the inert type CloudFormation serves itself was accepted, and
        // that resource is on the stack: the collision is what failed, not the template.
        if row.carries_cdk_metadata {
            let inert = resource_status_of(&router, row.stack, CDK_LOGICAL_ID).await;
            assert_eq!(
                inert,
                resource_status::CREATE_COMPLETE,
                "case {case:?}: expected the inert {CDK_METADATA} resource to be deployed \
                 beside the collision, got {inert:?}"
            );
        }
    }
}
