// SPDX-License-Identifier: CC0-1.0

//! Which GRANTS a stack's service role needs for the handler calls of one resource type, end
//! to end over the wire.
//!
//! RULE under test: **CloudFormation runs a resource type's handler with the credentials of the
//! stack's service role — "CloudFormation uses the role's credentials to make calls on your
//! behalf" (`CreateStackInput$RoleARN`) — and each call that handler makes is an ordinary
//! request of the service that owns the resource, so the grant it needs is the grant that
//! request needs: the IAM action `<service prefix>:<operation name>`. Under IAM's default deny
//! a role whose readable identity documents allow every permission the type's handlers require
//! EXCEPT one cannot make the call that one action names: the resource takes the failed status
//! of the step that made it, carrying the owning service's own 403 refusal (the sentence naming
//! the role session, the action and the resource ARN, inside the diagnostic its SDK appends and
//! the envelope CloudFormation quotes a provider's answer in, classified `AccessDenied`), the
//! stack takes that step's failed terminal status, and nothing of the call is carried out in the
//! owning service. Restoring the withheld grant lets the next operation through.**
//!
//! The rows differ along what the rule generalises over: WHICH handler permission is withheld
//! and therefore which lifecycle step cannot be carried out — the create's own call, the call an
//! added tag makes, the call a removed tag makes, the call a changed attribute makes, and the
//! delete's own call — with a control that withholds an action of another service entirely, so
//! no gate refusing every call could pass. The second test is the recovery the rule implies: the
//! grant is restored on the same role and the update that was refused, and then the delete,
//! succeed.
//!
//! Nothing here sleeps for a span the emulator owes: every wait polls for the CONDITION (a stack
//! reporting a settled status, a resource recording its event) under the shared harness's
//! generous deadline.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

use crate::declared_topic_policy_decides_the_alarm_action::sqs;
use crate::harness::{
    assert_sdk_diagnostic_with_attempts, call, create_stack, envelope_parts,
    failed_resource_reason, member, settled_stack_status, xml_text, REGION,
};
use crate::service_role_permissions::{put_role_policy, service_role};
use crate::stack_lifecycle::ACCOUNT_ID;

/// The logical id every template here declares its queue under.
const QUEUE: &str = "Queue";

/// The role session CloudFormation makes a stack's calls under, which is what an authorization
/// refusal of one of them names the caller by: `RoleARN` is the role "that CloudFormation
/// assumes", and a refusal names the SESSION of an assumed role rather than the role.
const SESSION: &str = "AWSCloudFormation";

/// The `HandlerErrorCode` class a call that was not permitted is reported under, which is what
/// tells a client whether the failed resource is worth retrying.
const ACCESS_DENIED_CLASS: &str = "AccessDenied";

/// The name Amazon SQS's own SDK client reports that service under, and the status a service
/// answers an unauthorized call with — the two values its diagnostic states.
const SQS_SDK_NAME: &str = "Sqs";
const UNAUTHORIZED_STATUS: u16 = 403;

/// The permissions the handlers of `AWS::SQS::Queue` require, which is the least-privilege set a
/// client grants a stack that deploys one.
///
/// Source: that resource type's own registry schema — the `permissions` its `create`, `read`,
/// `update`, `delete` and `list` handlers declare, which is what `DescribeType` reports as
/// `SchemaHandlerPermissions`
/// (`smithy_query(query="doc", shape="DescribeTypeOutput", member="Schema")`). The set is the
/// union of those lists, so a role carrying it can run every handler of the type.
const QUEUE_HANDLER_PERMISSIONS: &[&str] = &[
    "sqs:CreateQueue",
    "sqs:DeleteQueue",
    "sqs:GetQueueAttributes",
    "sqs:GetQueueUrl",
    "sqs:ListQueueTags",
    "sqs:ListQueues",
    "sqs:SetQueueAttributes",
    "sqs:TagQueue",
    "sqs:UntagQueue",
];

/// The tag key and value one row's update adds, and the one another row's update removes.
const TAG_KEY: &str = "owner";
const TAG_VALUE: &str = "platform";

/// The visibility timeout a queue is declared with, and the one an attribute-changing update
/// declares instead — "the length of time during which a message will be unavailable after a
/// message is delivered from the queue" (`AWS::SQS::Queue`'s `VisibilityTimeout`).
const DECLARED_TIMEOUT: u64 = 30;
const CHANGED_TIMEOUT: u64 = 45;

/// A router carrying CloudFormation, the Amazon SQS the declared queue really lives in, and the
/// IAM that holds the role whose documents decide the deployment's calls — all on ONE engine,
/// the shape `core/router/src/lib.rs` mounts every service in.
///
/// Its own router rather than the shared harness's, for the reason
/// `declared_topic_policy_decides_the_alarm_action` records: mounting Amazon SQS for every suite
/// of this target would change what the accounts those suites build hold.
fn deployed_under_a_role_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// The role's inline policy: an allow of every handler permission of the type EXCEPT
/// `withheld` — which for an action the type's handlers do not require at all leaves the whole
/// set granted.
fn allowing_every_permission_but(withheld: Option<&str>) -> Value {
    Value::Array(
        QUEUE_HANDLER_PERMISSIONS
            .iter()
            .filter(|action| Some(**action) != withheld)
            .map(|action| json!({"Effect": "Allow", "Action": action, "Resource": "*"}))
            .collect(),
    )
}

/// The template body declaring one queue with `timeout` as its visibility timeout, tagged when
/// `tagged` says so.
fn template(queue_name: &str, timeout: u64, tagged: bool) -> String {
    let mut properties = json!({
        "QueueName": queue_name,
        "VisibilityTimeout": timeout,
    });
    if tagged {
        properties["Tags"] = json!([{"Key": TAG_KEY, "Value": TAG_VALUE}]);
    }
    json!({
        "Resources": {QUEUE: {"Type": "AWS::SQS::Queue", "Properties": properties}},
    })
    .to_string()
}

/// Which lifecycle step one row's withheld permission stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Create,
    Update,
    Delete,
}

/// What one row's update asks of the queue, which is what decides WHICH call the update makes:
/// Amazon SQS has one API for each (`TagQueue`, `UntagQueue`, `SetQueueAttributes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Update {
    AddsATag,
    RemovesATag,
    ChangesAnAttribute,
}

impl Update {
    /// Whether the CREATE declares the tag: a row whose update REMOVES one starts from a queue
    /// that carries it.
    fn tagged_at_create(self) -> bool {
        self == Self::RemovesATag
    }

    /// The template the update submits.
    fn template(self, queue_name: &str) -> String {
        match self {
            Self::AddsATag => template(queue_name, DECLARED_TIMEOUT, true),
            Self::RemovesATag => template(queue_name, DECLARED_TIMEOUT, false),
            Self::ChangesAnAttribute => template(queue_name, CHANGED_TIMEOUT, false),
        }
    }
}

/// One row: which permission the role does NOT carry, what the update asks for, and which step
/// that permission stops — `None` for a control whose whole lifecycle goes through.
struct Row {
    case: &'static str,
    /// The stack, the role and the queue are all named after this.
    name: &'static str,
    withheld: &'static str,
    update: Update,
    fails_at: Option<Step>,
}

const ROWS: &[Row] = &[
    Row {
        case: "the create's own call",
        name: "qperm-create",
        withheld: "sqs:CreateQueue",
        update: Update::AddsATag,
        fails_at: Some(Step::Create),
    },
    Row {
        case: "the call an added tag makes",
        name: "qperm-tag",
        withheld: "sqs:TagQueue",
        update: Update::AddsATag,
        fails_at: Some(Step::Update),
    },
    Row {
        case: "the call a removed tag makes",
        name: "qperm-untag",
        withheld: "sqs:UntagQueue",
        update: Update::RemovesATag,
        fails_at: Some(Step::Update),
    },
    Row {
        case: "the call a changed attribute makes",
        name: "qperm-attributes",
        withheld: "sqs:SetQueueAttributes",
        update: Update::ChangesAnAttribute,
        fails_at: Some(Step::Update),
    },
    Row {
        case: "the delete's own call",
        name: "qperm-delete",
        withheld: "sqs:DeleteQueue",
        update: Update::AddsATag,
        fails_at: Some(Step::Delete),
    },
    // A CONTROL: what is withheld is no permission of this type's handlers at all, so every
    // call the lifecycle makes is granted and the whole lifecycle goes through.
    Row {
        case: "an action of another service entirely",
        name: "qperm-elsewhere",
        withheld: "ssm:PutParameter",
        update: Update::AddsATag,
        fails_at: None,
    },
];

/// The ARN a queue of `name` carries in this account and Region, which is what a policy
/// statement names it by and what an authorization refusal about it reports.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT_ID}:{name}")
}

/// The sentence AWS refuses a call no identity policy allows with, worded by the workspace's one
/// authorization refusal: the principal, the action as the refusing service words it, the
/// resource, and which of IAM's rules withheld the grant.
fn expected_refusal(role_name: &str, action: &str, resource: &str) -> String {
    let principal = format!("arn:aws:sts::{ACCOUNT_ID}:assumed-role/{role_name}/{SESSION}");
    let named = ::service_common::iam_authorization::reported_action(action);
    ::service_common::iam_authorization::not_authorized_by_identity_policy(
        &principal,
        &named,
        resource,
        &::service_common::iam_authorization::no_identity_policy_allows(&named),
    )
}

/// The url Amazon SQS serves the queue named `name`, or `None` when that service holds no such
/// queue — read through SQS's OWN wire, which is what says whether a call was made at all.
async fn queue_url(router: &MultiServiceRouter, case: &str, name: &str) -> Option<String> {
    let (http_status, body) = sqs(router, "GetQueueUrl", &json!({"QueueName": name})).await;
    assert!(
        matches!(http_status, 200 | 400),
        "case {case:?}: GetQueueUrl({name:?}) answered {http_status} {body}"
    );
    body.get("QueueUrl")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// The tags Amazon SQS holds for the queue `url`, as its own `ListQueueTags` reports them.
async fn queue_tags(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let (http_status, body) = sqs(router, "ListQueueTags", &json!({"QueueUrl": url})).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: ListQueueTags({url:?}) must answer 200, got {http_status} {body}"
    );
    body.get("Tags").cloned().unwrap_or(Value::Null)
}

/// One attribute of the queue `url`, as Amazon SQS reports it.
async fn queue_attribute(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    attribute: &str,
) -> Option<String> {
    let (http_status, body) = sqs(
        router,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": [attribute]}),
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: GetQueueAttributes({url:?}) must answer 200, got {http_status} {body}"
    );
    body.pointer(&format!("/Attributes/{attribute}"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Whether the queue carries the tag the rows add and remove.
fn carries_the_tag(tags: &Value) -> bool {
    tags.get(TAG_KEY).and_then(Value::as_str) == Some(TAG_VALUE)
}

/// Assert that the `ResourceStatusReason` the queue recorded on its `recorded` event is the
/// owning service's refusal of a call of `action` on the queue, as CloudFormation reports a
/// provider's answer: the refusal quoted inside the envelope, that service's SDK diagnostic
/// closing it, and the failure classified as a call that was not permitted.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    row_name: &str,
    recorded: &str,
    action: &str,
) {
    let reason = failed_resource_reason(case, router, REGION, row_name, QUEUE, recorded).await;
    let (quoted, _token, class) = envelope_parts(case, &reason);
    assert_eq!(
        class, ACCESS_DENIED_CLASS,
        "case {case:?}: expected the failure classified as a call that was not permitted, got \
         {class:?} out of {reason:?}"
    );
    let sentence = expected_refusal(row_name, action, &queue_arn(row_name));
    assert!(
        quoted.contains(&sentence),
        "case {case:?}: expected the refusal {sentence:?} in the reason, got {quoted:?}"
    );
    assert_sdk_diagnostic_with_attempts(case, &quoted, SQS_SDK_NAME, UNAUTHORIZED_STATUS);
}

/// RULE: a handler call whose IAM action the stack's service role is not granted is not made —
/// the resource fails that step with the owning service's own authorization refusal and the
/// stack takes that step's failed terminal status — while a role granted every permission the
/// type's handlers require carries the whole lifecycle out.
#[tokio::test]
async fn a_withheld_queue_handler_permission_stops_the_call_that_needs_it() {
    for row in ROWS {
        let case = row.case;
        let router = deployed_under_a_role_router();
        // The stack, the role and the queue are named after the row, so one row's records can
        // never decide another's.
        let role_arn = service_role(
            &router,
            row.name,
            allowing_every_permission_but(Some(row.withheld)),
        )
        .await;

        // ---------------------------------------------------------------- the create
        let mut extra = vec![member("RoleARN", &role_arn)];
        if row.fails_at == Some(Step::Create) {
            // The failed create is kept exactly as it failed, so the row reads the status the
            // refused call left rather than the rollback that would follow it.
            extra.push(member("DisableRollback", "true"));
        }
        let (http_status, created) = create_stack(
            &router,
            row.name,
            &template(row.name, DECLARED_TIMEOUT, row.update.tagged_at_create()),
            &extra,
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: CreateStack must be accepted — the decision is the RESOURCE's, not \
             the request's — got {http_status} {created}"
        );
        // The id the create minted, which is what a stack is still describable by once its
        // deletion has completed.
        let stack_id = xml_text(&created, "StackId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateStack reported no StackId: {created}"));
        let settled = settled_stack_status(case, &router, REGION, row.name).await;
        if row.fails_at == Some(Step::Create) {
            assert_eq!(
                settled,
                status::CREATE_FAILED,
                "case {case:?}: expected the stack to fail its create, got {settled:?}"
            );
            assert_refused(
                &router,
                case,
                row.name,
                resource_status::CREATE_FAILED,
                row.withheld,
            )
            .await;
            let held = queue_url(&router, case, row.name).await;
            assert!(
                held.is_none(),
                "case {case:?}: expected Amazon SQS to hold no queue at all, got {held:?}"
            );
            continue;
        }
        assert_eq!(
            settled,
            status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {settled:?}"
        );
        let url = queue_url(&router, case, row.name)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: Amazon SQS holds no queue {:?}", row.name));

        // ---------------------------------------------------------------- the update
        let (http_status, updated) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.name),
                member("TemplateBody", &row.update.template(row.name)),
            ],
        )
        .await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: UpdateStack must be accepted, got {http_status} {updated}"
        );
        let settled = settled_stack_status(case, &router, REGION, row.name).await;
        let tags = queue_tags(&router, case, &url).await;
        let timeout = queue_attribute(&router, case, &url, "VisibilityTimeout").await;
        if row.fails_at == Some(Step::Update) {
            assert_eq!(
                settled,
                status::UPDATE_ROLLBACK_COMPLETE,
                "case {case:?}: expected the refused update to roll back, got {settled:?}"
            );
            assert_refused(
                &router,
                case,
                row.name,
                resource_status::UPDATE_FAILED,
                row.withheld,
            )
            .await;
            // ...and the change the refused call would have made was not made: the queue is as
            // the create left it.
            assert_eq!(
                carries_the_tag(&tags),
                row.update.tagged_at_create(),
                "case {case:?}: expected the queue's tags to be as the create left them, got \
                 {tags:?}"
            );
            assert_eq!(
                timeout.as_deref(),
                Some(DECLARED_TIMEOUT.to_string().as_str()),
                "case {case:?}: expected the declared visibility timeout to stand, got \
                 {timeout:?}"
            );
            continue;
        }
        assert_eq!(
            settled,
            status::UPDATE_COMPLETE,
            "case {case:?}: expected the update to complete, got {settled:?}"
        );
        assert!(
            carries_the_tag(&tags),
            "case {case:?}: expected the update to have tagged the queue, got {tags:?}"
        );

        // ---------------------------------------------------------------- the delete
        let (http_status, deleted) =
            call(&router, "DeleteStack", &[member("StackName", row.name)]).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: DeleteStack must be accepted, got {http_status} {deleted}"
        );
        let settled = settled_stack_status(case, &router, REGION, &stack_id).await;
        let held = queue_url(&router, case, row.name).await;
        if row.fails_at == Some(Step::Delete) {
            assert_eq!(
                settled,
                status::DELETE_FAILED,
                "case {case:?}: expected the refused delete to fail the stack, got {settled:?}"
            );
            assert_refused(
                &router,
                case,
                row.name,
                resource_status::DELETE_FAILED,
                row.withheld,
            )
            .await;
            assert!(
                held.is_some(),
                "case {case:?}: expected Amazon SQS to still hold the queue the delete could \
                 not remove, got {held:?}"
            );
            continue;
        }
        assert_eq!(
            settled,
            status::DELETE_COMPLETE,
            "case {case:?}: expected the delete to complete, got {settled:?}"
        );
        assert!(
            held.is_none(),
            "case {case:?}: expected Amazon SQS to hold the queue no longer, got {held:?}"
        );
    }
}

/// RULE (the recovery): the role a stack RECORDS keeps acting for every later operation that
/// names no other one, so the grant it was missing can be restored on that same role — and the
/// update that was refused, and the delete after it, are then carried out.
#[tokio::test]
async fn restoring_the_withheld_grant_lets_the_next_update_and_the_delete_through() {
    let case = "the grant restored after a refused update";
    let name = "qperm-restored";
    let router = deployed_under_a_role_router();
    let role_arn = service_role(
        &router,
        name,
        allowing_every_permission_but(Some("sqs:TagQueue")),
    )
    .await;

    let (http_status, created) = create_stack(
        &router,
        name,
        &template(name, DECLARED_TIMEOUT, false),
        &[member("RoleARN", &role_arn)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: CreateStack must be accepted, got {http_status} {created}"
    );
    let stack_id = xml_text(&created, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateStack reported no StackId: {created}"));
    let settled = settled_stack_status(case, &router, REGION, name).await;
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the create to complete under a role granted it, got {settled:?}"
    );
    let url = queue_url(&router, case, name)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: Amazon SQS holds no queue {name:?}"));

    // The update adds a tag, which is the one call this role may not make.
    let tagging = template(name, DECLARED_TIMEOUT, true);
    let (http_status, updated) = call(
        &router,
        "UpdateStack",
        &[member("StackName", name), member("TemplateBody", &tagging)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: UpdateStack must be accepted, got {http_status} {updated}"
    );
    let settled = settled_stack_status(case, &router, REGION, name).await;
    assert_eq!(
        settled,
        status::UPDATE_ROLLBACK_COMPLETE,
        "case {case:?}: expected the refused update to roll back, got {settled:?}"
    );
    assert_refused(
        &router,
        case,
        name,
        resource_status::UPDATE_FAILED,
        "sqs:TagQueue",
    )
    .await;
    let tags = queue_tags(&router, case, &url).await;
    assert!(
        !carries_the_tag(&tags),
        "case {case:?}: expected the refused call to have tagged nothing, got {tags:?}"
    );

    // The grant is restored on the SAME role, which the stack keeps acting as.
    put_role_policy(&router, name, allowing_every_permission_but(None)).await;
    let (http_status, again) = call(
        &router,
        "UpdateStack",
        &[member("StackName", name), member("TemplateBody", &tagging)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: the resumed UpdateStack must be accepted, got {http_status} {again}"
    );
    let settled = settled_stack_status(case, &router, REGION, name).await;
    assert_eq!(
        settled,
        status::UPDATE_COMPLETE,
        "case {case:?}: expected the update to complete once the grant was restored, got \
         {settled:?}"
    );
    let tags = queue_tags(&router, case, &url).await;
    assert!(
        carries_the_tag(&tags),
        "case {case:?}: expected the queue to carry the declared tag, got {tags:?}"
    );

    // ...and so does the delete, whose own call the restored policy also grants.
    let (http_status, deleted) = call(&router, "DeleteStack", &[member("StackName", name)]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: DeleteStack must be accepted, got {http_status} {deleted}"
    );
    let settled = settled_stack_status(case, &router, REGION, &stack_id).await;
    assert_eq!(
        settled,
        status::DELETE_COMPLETE,
        "case {case:?}: expected the delete to complete, got {settled:?}"
    );
    let held = queue_url(&router, case, name).await;
    assert!(
        held.is_none(),
        "case {case:?}: expected Amazon SQS to hold the queue no longer, got {held:?}"
    );
}
