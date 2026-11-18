// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the principal that SIGNED a request perform the action it asked for —
//! the authorization step taken in FRONT of every operation, before anything is read or
//! written.
//!
//! Every operation of this API is signed with IAM credentials (`sigv4` is the only auth
//! scheme the model declares, `smithy_query(query="digest")`), and Step Functions decides
//! such a call at the IAM layer before it is dispatched: a caller whose identity-based
//! policies do not allow the action on the resource the request names is told it is not
//! authorized rather than told anything about what the account holds, and nothing of the
//! account changes on the way to that refusal. The decision is taken per request, so a
//! statement `iam:PutRolePolicy` wrote refuses the next call and one `iam:DeleteRolePolicy`
//! removed stops granting it — which is what makes a client's least-privilege test, and a
//! revocation drill, mean anything here.
//!
//! No error shape of this model carries that refusal — no operation of this service lists
//! an access error (`smithy_query(query="digest")`) — because it belongs to the
//! authorization layer ahead of the service's own code rather than to any one operation:
//! it is the AWS-common `AccessDeniedException`, rendered here in this service's own
//! `awsJson1_0` error envelope ([`super::wire`]) with HTTP 400, the status this endpoint
//! answers every refusal of a caller under.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who it is, what it may do, and the decision** — [`super::authorization`], the one
//!    place this service resolves the principal an access key id speaks for, reads the
//!    identity-based documents and the permissions boundary in force on it, and asks
//!    [`::service_common::iam_authorization`] about an `(action, resource)` pair
//!    ([`super::authorization::caller_action_denial`]); the callers it does not bound — the
//!    account's own published credential, which every in-process call another emulated
//!    service makes to this one presents, a principal carrying a grant this emulator cannot
//!    read, and an engine with no STS or IAM store — are named there. Nothing is cached, so
//!    a policy `iam:PutRolePolicy` wrote refuses the next call and one it replaced stops
//!    granting on the next call;
//! 2. **what the request asks for** — the action and the resource, read off the request
//!    alone here ([`super::authorization::operation_action`], [`addressed_resource`])
//!    before anything is looked up, so the decision cannot depend on whether that resource
//!    exists and a refused caller learns nothing about what the account holds;
//! 3. **where the decision is taken** — this module's [`IamAuthorized`], in front of the
//!    routing table. Wrapping is what makes it ONE decision: every modelled operation is
//!    judged by the same rule before its handler, so no handler can forget it, none of them
//!    takes it twice, and no handler of this service carries an authorization step of its
//!    own.
//!
//! ## The action and the resource
//!
//! The action is `states:<OperationName>` — this service's own ARN namespace and the
//! operation's shape name, spelled by [`super::authorization::operation_action`] — so one
//! statement scoped to `states:*` covers every operation of it and nothing outside it.
//!
//! A request may ask for MORE than the operation it names: a member that switches on a
//! capability AWS gates on an action of its own makes the call ask for that action TOO, and
//! the caller has to hold both ([`CAPABILITY_ACTIONS`]). Each asked-for action is decided by
//! the same rule, in the order the request asks for them, and the first one the documents in
//! force do not allow refuses the call — so a caller allowed the operation but not the
//! capability is served every call that does not ask for it and refused the ones that do.
//!
//! The resource is what the request ADDRESSES: the ARN it carries in the member THAT
//! OPERATION names its resource by ([`ADDRESSED_BY`], keyed by operation so a member
//! another operation names its resource by cannot decide this one), the resource the TASK
//! TOKEN it carries belongs to for an operation addressed that way
//! ([`ADDRESSED_BY_TASK_TOKEN`]), the ARN a create would MINT — for the name it carries
//! ([`minted_resource`]), or, for an alias, inside the state machine its routing
//! configuration points at ([`super::request_conditions::minted_alias_arn`]) — and
//! otherwise [`any_resource`]: a call that names no resource of this service at all, which
//! is judged against every resource of the KIND its API is about in the account and Region
//! the request was made for, written in this service's own ARN form with the resource name
//! wildcarded rather than as a bare `*` — and, for an API that acts on no resource of the
//! account at all, with the resource TYPE wildcarded too. That ARN is what AWS reports such
//! a call was refused on, and it is what a statement has to cover to grant it.
//!
//! The ARN a request carries may be a QUALIFIED one, and a qualifier is not always part of
//! the resource: an operation that reaches a state machine THROUGH an alias or a version
//! is taken on the state machine itself, with the qualifier carried as a condition key
//! instead ([`super::request_conditions`]), so a grant of the action on a state machine
//! covers every way of naming it.
//!
//! A `SendTask*` call carries no ARN at all — the task it settles is named by the
//! `taskToken` the service itself minted for that task — and a task token of this service
//! NAMES the resource its task is of ([`crate::state::read_task_token`]): the activity a
//! worker was handed the task by, and for a callback (`.waitForTaskToken`) task the
//! execution waiting at it, whose own resource is the STATE MACHINE it is a run of. So such
//! a call is judged against that resource, read out of the token the request carried and
//! before anything is looked up, exactly as every other request is judged against the
//! resource it names. A value that is not a token this service ever minted names nothing, so
//! a call carrying one is judged as a call that named no resource at all.

use ::std::sync::Arc;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema_server::protocol::ServerProtocol;
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

use super::authorization::{caller_action_denial, operation_action, service_action};

/// The wire error code of an identity-based authorization refusal, as AWS's "Common
/// Errors" table names it — what a client branches on for "not permitted".
pub const ACCESS_DENIED_CODE: &str = "AccessDeniedException";

/// The resource a request that names no resource of this service in a member of its own is
/// judged against, as AWS writes it for such a request — a listing, a call carrying a task
/// token this service never minted, a state tested and a definition validated (see the
/// module docs): every resource of the KIND that request's API is about, in the Region the
/// request was made for and the account this emulator serves, written in this service's own
/// ARN form with the resource NAME wildcarded
/// (`crate::state::every_resource_of_type_arn`) rather than as a bare `*`.
///
/// That ARN is what AWS reports such a call was refused on, and it is what a statement has
/// to cover to grant it: an API about state machines is judged against the state machines of
/// the Region (`arn:<partition>:states:<region>:<account>:stateMachine:*`), one about
/// activities against its activities, and so on down [`API_RESOURCE_TYPE`]. An API that is
/// about no one resource type — a tagging call, and the two that decide a definition the
/// request itself carries, acting on no resource of the account — is judged against every
/// resource of this service ([`crate::state::every_resource_arn`]), the ARN with both the
/// type and the name wildcarded.
#[must_use]
pub fn any_resource(operation: &str, region: &str) -> String {
    match api_resource_type(operation) {
        Some(resource_type) => crate::state::every_resource_of_type_arn(region, resource_type),
        None => crate::state::every_resource_arn(region),
    }
}

/// The resource TYPE each operation's API is ABOUT, keyed by the operation's shape name:
/// the type of resource the call acts on, whether or not its request names one.
///
/// AWS reports a call that fixed no resource of its own as refused on the resource pattern
/// of the API's own category rather than on a bare wildcard, so the category is a fact of
/// each operation — read from what the operation DOES: a state machine API (every create,
/// read, write and listing of state machines, their versions and their aliases, and every
/// start of one), an activity API (every create, read, listing and poll of activities, and
/// the three `SendTask*` calls a worker settles a task with), an execution API (every read,
/// stop and redrive of one execution, and the Map Runs of one), or a Map Run API. An empty
/// row is an API about no one type: the tagging calls take "the ARN for the Step Functions
/// state machine or activity", so they are about both, and the two calls that decide a
/// DEFINITION the request itself carries act on no resource of the account at all — a call
/// of one of those is refused on every resource of this service, the TYPE wildcarded as
/// well as the name.
///
/// Source: every operation `smithy_query(query="digest",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`
/// binds, with the resource each one's own documentation says it acts on. Public, so the
/// catalog is re-derived from the model by this crate's own tests
/// (`tests/all/identity_policy_authorization.rs`) rather than only transcribed here.
pub const API_RESOURCE_TYPE: &[(&str, &str)] = &[
    ("CreateActivity", crate::state::ACTIVITY_RESOURCE),
    ("CreateStateMachine", crate::state::STATE_MACHINE_RESOURCE),
    (
        "CreateStateMachineAlias",
        crate::state::STATE_MACHINE_RESOURCE,
    ),
    ("DeleteActivity", crate::state::ACTIVITY_RESOURCE),
    ("DeleteStateMachine", crate::state::STATE_MACHINE_RESOURCE),
    (
        "DeleteStateMachineAlias",
        crate::state::STATE_MACHINE_RESOURCE,
    ),
    (
        "DeleteStateMachineVersion",
        crate::state::STATE_MACHINE_RESOURCE,
    ),
    ("DescribeActivity", crate::state::ACTIVITY_RESOURCE),
    ("DescribeExecution", crate::state::EXECUTION_RESOURCE),
    ("DescribeMapRun", crate::state::MAP_RUN_RESOURCE),
    ("DescribeStateMachine", crate::state::STATE_MACHINE_RESOURCE),
    (
        "DescribeStateMachineAlias",
        crate::state::STATE_MACHINE_RESOURCE,
    ),
    (
        "DescribeStateMachineForExecution",
        crate::state::STATE_MACHINE_RESOURCE,
    ),
    ("GetActivityTask", crate::state::ACTIVITY_RESOURCE),
    ("GetExecutionHistory", crate::state::EXECUTION_RESOURCE),
    ("ListActivities", crate::state::ACTIVITY_RESOURCE),
    ("ListExecutions", crate::state::STATE_MACHINE_RESOURCE),
    ("ListMapRuns", crate::state::EXECUTION_RESOURCE),
    (
        "ListStateMachineAliases",
        crate::state::STATE_MACHINE_RESOURCE,
    ),
    (
        "ListStateMachineVersions",
        crate::state::STATE_MACHINE_RESOURCE,
    ),
    ("ListStateMachines", crate::state::STATE_MACHINE_RESOURCE),
    // "The Amazon Resource Name (ARN) for the Step Functions state machine or activity":
    // a tagging call is about both taggable types, so it is about no one type.
    ("ListTagsForResource", ""),
    (
        "PublishStateMachineVersion",
        crate::state::STATE_MACHINE_RESOURCE,
    ),
    ("RedriveExecution", crate::state::EXECUTION_RESOURCE),
    ("SendTaskFailure", crate::state::ACTIVITY_RESOURCE),
    ("SendTaskHeartbeat", crate::state::ACTIVITY_RESOURCE),
    ("SendTaskSuccess", crate::state::ACTIVITY_RESOURCE),
    ("StartExecution", crate::state::STATE_MACHINE_RESOURCE),
    ("StartSyncExecution", crate::state::STATE_MACHINE_RESOURCE),
    ("StopExecution", crate::state::EXECUTION_RESOURCE),
    ("TagResource", ""),
    // "Accepts the definition of a single state and executes it. You can test a state
    // without creating a state machine or updating an existing state machine": the call
    // decides the definition its own request carries and reads, writes and mints no
    // resource of the account, so it is about no one resource type.
    ("TestState", ""),
    ("UntagResource", ""),
    ("UpdateMapRun", crate::state::MAP_RUN_RESOURCE),
    ("UpdateStateMachine", crate::state::STATE_MACHINE_RESOURCE),
    (
        "UpdateStateMachineAlias",
        crate::state::STATE_MACHINE_RESOURCE,
    ),
    // "You can validate that a state machine definition is correct without creating a
    // state machine resource": the same definition-only API as `TestState` above, about no
    // resource of the account and so about no one resource type.
    ("ValidateStateMachineDefinition", ""),
];

/// The resource type `operation`'s API is about, and `None` for an API about no one type —
/// including an operation this catalog does not carry, which is judged against every
/// resource of this service rather than against a type guessed from its name.
#[must_use]
pub fn api_resource_type(operation: &str) -> Option<&'static str> {
    API_RESOURCE_TYPE
        .iter()
        .find(|(named, _)| *named == operation)
        .map(|(_, resource_type)| *resource_type)
        .filter(|resource_type| !resource_type.is_empty())
}

/// The member of each operation's INPUT that names the ARN of the resource that request
/// addresses, keyed by the operation's shape name: the one member (or, where the model
/// declares two, the members in the order they decide) read off the body, so a value a
/// request carries under a member its own operation does not model decides nothing. An
/// empty row is an operation whose request names no resource ARN at all.
///
/// Source: every operation of this model and its own input shape
/// (`smithy_query(query="operation",operation="<Op>",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`
/// for each of the 37 operations `query="digest"` lists). Public, so that catalog is
/// re-derived from the model by this crate's own tests
/// (`tests/all/identity_policy_authorization.rs`) rather than only transcribed here.
pub const ADDRESSED_BY: &[(&str, &[&str])] = &[
    ("CreateActivity", &[]),
    ("CreateStateMachine", &[]),
    ("CreateStateMachineAlias", &[]),
    ("DeleteActivity", &["activityArn"]),
    ("DeleteStateMachine", &["stateMachineArn"]),
    ("DeleteStateMachineAlias", &["stateMachineAliasArn"]),
    ("DeleteStateMachineVersion", &["stateMachineVersionArn"]),
    ("DescribeActivity", &["activityArn"]),
    ("DescribeExecution", &["executionArn"]),
    ("DescribeMapRun", &["mapRunArn"]),
    ("DescribeStateMachine", &["stateMachineArn"]),
    ("DescribeStateMachineAlias", &["stateMachineAliasArn"]),
    ("DescribeStateMachineForExecution", &["executionArn"]),
    ("GetActivityTask", &["activityArn"]),
    ("GetExecutionHistory", &["executionArn"]),
    ("ListActivities", &[]),
    // "Lists all executions of a state machine or a Map Run": the request names one or
    // the other, and is judged against whichever it carries.
    ("ListExecutions", &["stateMachineArn", "mapRunArn"]),
    ("ListMapRuns", &["executionArn"]),
    ("ListStateMachineAliases", &["stateMachineArn"]),
    ("ListStateMachineVersions", &["stateMachineArn"]),
    ("ListStateMachines", &[]),
    ("ListTagsForResource", &["resourceArn"]),
    ("PublishStateMachineVersion", &["stateMachineArn"]),
    ("RedriveExecution", &["executionArn"]),
    // The three `SendTask*` operations name no ARN: the task each settles is named by the
    // `taskToken` the service minted for it, which is where their resource is read from
    // ([`ADDRESSED_BY_TASK_TOKEN`]).
    ("SendTaskFailure", &[]),
    ("SendTaskHeartbeat", &[]),
    ("SendTaskSuccess", &[]),
    ("StartExecution", &["stateMachineArn"]),
    ("StartSyncExecution", &["stateMachineArn"]),
    ("StopExecution", &["executionArn"]),
    ("TagResource", &["resourceArn"]),
    ("TestState", &[]),
    ("UntagResource", &["resourceArn"]),
    ("UpdateMapRun", &["mapRunArn"]),
    ("UpdateStateMachine", &["stateMachineArn"]),
    ("UpdateStateMachineAlias", &["stateMachineAliasArn"]),
    ("ValidateStateMachineDefinition", &[]),
];

/// The members `operation` names the ARN of the resource it addresses by, in the order
/// they decide — empty for an operation that names none, and for one this catalog does not
/// carry, which is judged against every resource rather than against a member read off a
/// request it does not belong to.
#[must_use]
pub fn addressed_by(operation: &str) -> &'static [&'static str] {
    ADDRESSED_BY
        .iter()
        .find(|(named, _)| *named == operation)
        .map_or(&[], |(_, members)| *members)
}

/// The member a request carries the TASK TOKEN of the task it is about in.
///
/// Source: the one member each of the operations below declares the token under
/// (`SendTaskSuccessInput$taskToken`, `SendTaskFailureInput$taskToken`,
/// `SendTaskHeartbeatInput$taskToken`).
const TASK_TOKEN_MEMBER: &str = "taskToken";

/// The operations whose request names the resource it is about through a TASK TOKEN rather
/// than through an ARN of its own.
///
/// Source: every operation of this model whose input declares [`TASK_TOKEN_MEMBER`] — the
/// three `SendTask*` calls a worker settles or renews a task with
/// (`smithy_query(query="operation",operation="SendTaskSuccess,SendTaskFailure,SendTaskHeartbeat",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`).
/// Public, so that catalog is re-derived from the model by this crate's own tests
/// (`tests/all/identity_policy_authorization.rs`) rather than only transcribed here.
pub const ADDRESSED_BY_TASK_TOKEN: &[&str] =
    &["SendTaskFailure", "SendTaskHeartbeat", "SendTaskSuccess"];

/// Whether `operation` names the resource it addresses through a task token
/// ([`ADDRESSED_BY_TASK_TOKEN`]).
#[must_use]
pub fn addressed_by_task_token(operation: &str) -> bool {
    ADDRESSED_BY_TASK_TOKEN.contains(&operation)
}

/// The resource of this service the task token `token` belongs to, or `None` for a value
/// this service never minted — which names nothing, so a call carrying one is judged as a
/// call that named no resource at all.
///
/// A token NAMES the resource its task is of ([`crate::state::read_task_token`]): the
/// ACTIVITY a worker was handed the task by, which is itself a resource of this service, and
/// for a callback (`.waitForTaskToken`) task the EXECUTION waiting at it — a run of a state
/// machine, so the resource the task belongs to is that STATE MACHINE
/// ([`crate::state::execution_state_machine_arn`]). RECORDED behaviour for that second
/// reading: a refusal of a `SendTask*` call made with a callback token names the state
/// machine the waiting execution runs on `on resource:`, not the execution and not every
/// resource — the model declares no error of its own for an authorization refusal, so the
/// recorded refusal is what says which resource such a call is about.
///
/// Both readings are functions of the token alone, so nothing is looked up to decide the
/// call and a token whose task has already been closed is judged exactly as one whose task
/// is outstanding.
fn token_resource(token: &str) -> Option<String> {
    let named = crate::state::read_task_token(token)?;
    Some(crate::state::execution_state_machine_arn(&named).unwrap_or(named))
}

/// The operation of THIS service that `routing_key` names, spelled by this service's own
/// catalog of them ([`ADDRESSED_BY`], which carries every operation the model binds and is
/// re-derived from the model by this crate's tests), and `None` for a key that names no
/// operation of this service.
///
/// A routing key is read off a request header, so it is a caller's text until it has been
/// resolved: resolving it here hands back a `&'static str` out of a CLOSED set, which is
/// what lets a caller's key be used as a value — the name of the API a call was made
/// against ([`super::api_capacity`]) — without the set of values it can take growing with
/// the requests made.
#[must_use]
pub fn served_operation(routing_key: &str) -> Option<&'static str> {
    ADDRESSED_BY
        .iter()
        .find(|(named, _)| *named == routing_key)
        .map(|(named, _)| *named)
}

/// A capability a request can ask for BESIDE the operation it names: the operation that
/// models it, the boolean member that switches it on, and the name of the action AWS grants
/// it by — the request is decided against that action as well as the operation's own, so a
/// caller that holds the operation but not the capability is refused exactly the calls that
/// ask for the capability.
///
/// Source: every member of this model whose own documentation names an IAM action of this
/// service as its requirement — `TestStateInput$revealSecrets`
/// (`smithy_query(query="doc",shape="TestStateInput",member="revealSecrets",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`:
/// "If you set revealSecrets to true, you must make sure that the IAM user that calls the
/// TestState API has permission for the states:RevealSecrets action ... Without this
/// permission, Step Functions throws an access denied error"). Public, so that reading of
/// the model is checked against the model itself by this crate's own tests
/// (`tests/all/test_state_reveal_secrets_authorization.rs`) rather than only transcribed
/// here.
pub const CAPABILITY_ACTIONS: &[(&str, &str, &str)] =
    &[("TestState", "revealSecrets", "RevealSecrets")];

/// The capabilities `operation` can be asked for beside itself, as `(member, action name)`
/// pairs — empty for an operation that models none, which is every other one.
#[must_use]
pub fn capabilities(operation: &str) -> Vec<(&'static str, &'static str)> {
    CAPABILITY_ACTIONS
        .iter()
        .filter(|(models, _, _)| *models == operation)
        .map(|(_, member, action)| (*member, *action))
        .collect()
}

/// Every IAM action this submitted input asks for, in the order they are decided: the
/// operation's OWN action first, then the action of each capability a member of the request
/// switches on, then the action of each further permission a member of it IMPLIES
/// ([`IMPLIED_ACTIONS`]).
///
/// A capability member that is absent, is `false` or carries anything but a boolean asks for
/// nothing — the modelled default of such a member is `false`, so a request that does not
/// switch it on is decided exactly as one that does not model it at all.
fn requested_actions(submitted: Option<&::serde_json::Value>, operation: &str) -> Vec<String> {
    let mut asked = vec![operation_action(operation)];
    for (member, action) in capabilities(operation) {
        let switched_on = submitted
            .and_then(|body| body.get(member))
            .and_then(::serde_json::Value::as_bool)
            .unwrap_or(false);
        if switched_on {
            asked.push(service_action(action));
        }
    }
    for (member, asked_for, action) in IMPLIED_ACTIONS {
        if member.0 != operation {
            continue;
        }
        if asked_for(submitted.and_then(|body| body.get(member.1))) {
            asked.push(service_action(action));
        }
    }
    asked
}

/// Whether the value a member carries ASKS FOR the permission its row names.
type AsksFor = fn(Option<&::serde_json::Value>) -> bool;

/// One implied permission: the `(operation, member)` whose value asks for it, whether that
/// value asks for it, and the name of the action AWS grants it by.
type ImpliedAction = ((&'static str, &'static str), AsksFor, &'static str);

/// A permission a request asks for because of WHAT IT DOES rather than because of the
/// operation it names: the `(operation, member)` whose value asks for it, whether that
/// value asks for it, and the action AWS grants it by.
///
/// AWS gates a capability like this on its own action even where the operation's own
/// documentation does not say so, and refuses the whole call — creating nothing — when the
/// caller holds the operation but not the implied action:
///
/// * passing `tags` on a create TAGS the resource being created, which is
///   `states:TagResource`: "to create a resource with tags you must have permissions to
///   use the action that creates the resource and the `TagResource` action" is AWS's own
///   rule for tag-on-create, and it is why a caller denied the tagging action cannot
///   escape a tag-based permission boundary by writing its tags through the create;
/// * asking a create or an update to `publish` mints a VERSION of the state machine, which
///   is the work `states:PublishStateMachineVersion` grants — the same action the
///   standalone `PublishStateMachineVersion` operation is taken under — so a caller denied
///   it gets no version and no state machine from a create that asked for one.
///
/// A member that is absent, or carries a value that asks for nothing (no tags at all, a
/// `publish` left at its `false` default), implies nothing: such a request is decided
/// exactly as one that does not carry the member.
const IMPLIED_ACTIONS: &[ImpliedAction] = &[
    (("CreateActivity", "tags"), writes_tags, "TagResource"),
    (("CreateStateMachine", "tags"), writes_tags, "TagResource"),
    (
        ("CreateStateMachine", "publish"),
        switched_on,
        "PublishStateMachineVersion",
    ),
    (
        ("UpdateStateMachine", "publish"),
        switched_on,
        "PublishStateMachineVersion",
    ),
];

/// Whether the member carries tags to write: a non-empty list, since a request carrying an
/// empty one writes none.
fn writes_tags(member: Option<&::serde_json::Value>) -> bool {
    member
        .and_then(::serde_json::Value::as_array)
        .is_some_and(|tags| !tags.is_empty())
}

/// Whether the member switches its capability ON: the boolean `true`, since the modelled
/// default of such a member is `false`.
fn switched_on(member: Option<&::serde_json::Value>) -> bool {
    member
        .and_then(::serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// The member a request that MINTS a resource names it by.
const NAME_MEMBER: &str = "name";

/// The ARN the create `operation` would MINT for the resource `name` in `region`, or
/// `None` for an operation that mints none.
///
/// Source: the two operations of this model whose input carries the `name` of the resource
/// it creates and no ARN of an existing one, and whose resource's ARN is a function of that
/// name alone (`CreateStateMachineInput$name`, `CreateActivityInput$name`) — an alias is
/// named inside the state machine its routing configuration points at, so the ARN a
/// `CreateStateMachineAlias` mints is read by
/// [`super::request_conditions::minted_alias_arn`] instead. Such a request addresses the
/// resource it is about to mint,
/// so it is judged against that ARN — built by the one place this service spells each form
/// ([`crate::state::state_machine_arn`], [`crate::state::activity_arn`]) and from the
/// request alone, so nothing is looked up and nothing is created to decide it.
#[must_use]
pub fn minted_resource(operation: &str, region: &str, name: &str) -> Option<String> {
    match operation {
        "CreateStateMachine" => Some(crate::state::state_machine_arn(region, name)),
        "CreateActivity" => Some(crate::state::activity_arn(region, name)),
        _ => None,
    }
}

/// The operation's input as this protocol carries it: the request body read as JSON, and
/// `None` for a body that is not a JSON document at all — which names no resource of this
/// service and asks for no capability, so it is judged exactly as a request that carried
/// neither.
fn submitted_input(req: &RtRequest) -> Option<::serde_json::Value> {
    req.body()
        .bytes()
        .and_then(|body| ::serde_json::from_slice(body).ok())
}

/// The resource the submitted input `submitted` addresses, for `operation` in `region`
/// (see the module docs).
///
/// A member that is absent, is not a string or is empty names no resource, so such a
/// request is judged exactly as one that named none at all.
fn addressed_in(submitted: Option<&::serde_json::Value>, operation: &str, region: &str) -> String {
    let member = |name: &str| {
        submitted
            .and_then(|body| body.get(name))
            .and_then(::serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    for named in addressed_by(operation) {
        if let Some(arn) = member(named) {
            return arn;
        }
    }
    // A `SendTask*` call names its resource only through the token the service minted for
    // the task: the activity the task was handed out by, or the state machine whose
    // execution is waiting at a callback task. Read from the token itself, so nothing is
    // looked up to decide it.
    if addressed_by_task_token(operation) {
        if let Some(named) = member(TASK_TOKEN_MEMBER).as_deref().and_then(token_resource) {
            return named;
        }
    }
    if let Some(minted) = member(NAME_MEMBER)
        .and_then(|name| minted_resource(operation, region, &name))
        .or_else(|| super::request_conditions::minted_alias_arn(submitted, operation))
    {
        return minted;
    }
    any_resource(operation, region)
}

/// The operations whose request is refused outright when the ARN it names belongs to
/// ANOTHER account ([`super::authorization::foreign_account_denial`]): the calls about a
/// state machine's VERSIONS and ALIASES, and the listings of them.
///
/// A version and an alias are reached only through the state machine that holds them, and
/// the account is part of that reference: such a call is about a resource of one account,
/// so an ARN of another account names nothing this endpoint can answer for — AWS refuses
/// it as a resource the caller may not access rather than as a resource that is absent,
/// which is what keeps a cross-account mistake from being reported as a missing resource
/// and keeps the refusal from disclosing whether the other account holds it.
///
/// Source: every operation of this model whose input names a state machine alias or
/// version ARN, or lists the aliases or versions of a state machine
/// (`smithy_query(query="digest",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`).
/// Public, so the catalog is read back by this crate's own tests.
pub const OWNERSHIP_CHECKED: &[&str] = &[
    "CreateStateMachineAlias",
    "DeleteStateMachineAlias",
    "DeleteStateMachineVersion",
    "DescribeStateMachineAlias",
    "ListStateMachineAliases",
    "ListStateMachineVersions",
    "UpdateStateMachineAlias",
];

/// Whether `operation` refuses an ARN of another account outright, for the ARN
/// `addressed` it named ([`OWNERSHIP_CHECKED`]).
///
/// A QUALIFIED state machine ARN names a VERSION or an ALIAS whichever operation carries
/// it, so a call about one is ownership-checked too: an unqualified ARN of another account
/// names a state machine that is simply not here, which the operation's own
/// does-not-exist error reports.
#[must_use]
pub fn ownership_checked(operation: &str, addressed: &str) -> bool {
    OWNERSHIP_CHECKED.contains(&operation)
        || super::request_conditions::state_machine_qualifier(addressed).is_some()
}

/// Why the policies in force do not let this caller perform what this request asks for —
/// `operation` itself, every capability a member of it switches on and every permission a
/// member of it implies ([`requested_actions`]) — or `None` when they let it, including
/// every caller the gate does not bound (see the module docs).
///
/// Call it BEFORE the operation runs: a refused request must read and write nothing. The
/// decision itself is [`super::authorization::caller_action_denial`], the one place this
/// service asks what the documents in force on a caller say about an `(action, resource)`
/// pair; this reads the pairs off the request, together with the condition keys that
/// request fixes ([`super::request_conditions`]), so a statement scoped to one of them is
/// decided on its own terms.
fn denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    operation: &str,
    req: &RtRequest,
) -> Option<String> {
    let submitted = submitted_input(req);
    let region = crate::state::request_region(ctx.parts());
    let addressed = addressed_in(submitted.as_ref(), operation, &region);
    // A VERSION or an ALIAS of a state machine is reached only through the account that
    // holds it: such a call names its resource by an ARN whose account field is part of
    // the reference, and an ARN of another account names a resource no document of this
    // account can grant over — so it is refused before anything is looked up
    // ([`OWNERSHIP_CHECKED`]).
    if ownership_checked(operation, &addressed) {
        if let Some(refusal) = super::authorization::foreign_account_denial(engine, ctx, &addressed)
        {
            return Some(refusal);
        }
    }
    // Every action the request asks for has to be allowed, on every resource the request
    // is ABOUT ([`super::request_conditions::asked_for`]), so the FIRST decision the
    // documents in force do not cover refuses the call — and the refusal names that
    // action and that resource, which is what tells the caller which grant it is missing.
    let asked =
        super::request_conditions::asked_for(engine, submitted.as_ref(), operation, &addressed);
    requested_actions(submitted.as_ref(), operation)
        .iter()
        .find_map(|action| {
            asked.iter().find_map(|about| {
                caller_action_denial(engine, ctx, action, &about.resource, &about.conditions)
            })
        })
}

/// Every modelled operation of this service, served only to a caller IAM allows to perform
/// it.
///
/// It holds no operation of its own: the one it serves a permitted request through is the
/// one the service's routing table names for that request, reached through the SAME
/// wrapper every response of this service already passes
/// (`service_common::query_error_envelope::RequestIdHeaderOperation`), so the framework
/// still owns the whole deserialize -> validate -> handle -> serialize sequence and every
/// status, code and message it answers with. A refusal of its own is reported with the
/// request id too, because that is a property of every response this service sends.
pub struct IamAuthorized<Eng> {
    /// The shared engine, so the principal's policies are read from the one store every
    /// service of this emulator shares.
    engine: Arc<Eng>,
    /// Serves the request the decision admitted, names the request each response
    /// answers, and counts what the served call consumed
    /// ([`super::api_capacity`]).
    served: super::api_capacity::Metered<Eng>,
    /// This service's wire protocol, held rather than built per refusal: the envelope a
    /// refusal is rendered in is the one every other error of this endpoint is rendered in.
    protocol: super::wire::SwfJsonProtocol,
}

impl<Eng> IamAuthorized<Eng> {
    /// Put every modelled operation of this service behind the IAM decision, reading the
    /// policies in force from `engine`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            served: super::api_capacity::Metered::new(Arc::clone(&engine)),
            engine,
            protocol: super::wire::SwfJsonProtocol::new(),
        }
    }

    /// The refusal as this endpoint puts it on the wire: the AWS-common
    /// `AccessDeniedException`, in this service's own `awsJson1_0` envelope and under the
    /// status it answers every client refusal with.
    fn denied_response(&self, message: &str) -> RtResponse {
        let refused = crate::scaffolded::AccessDeniedException::new(message);
        let rendered = self.protocol.serialize_error(
            &refused,
            &crate::scaffolded::ACCESS_DENIED_EXCEPTION_SCHEMA,
            ACCESS_DENIED_CODE,
            super::wire::CLIENT_ERROR_STATUS,
        );
        match rendered {
            Ok(response) => response,
            // The envelope is built from this crate's own schema and a message of its own,
            // so it cannot fail to render; a failure is still answered rather than leaving
            // the caller waiting, and it is answered as a REFUSAL, never as a success.
            Err(failed) => {
                ::tracing::warn!(
                    error = %failed,
                    "the authorization refusal could not be rendered in this service's envelope"
                );
                RtResponse::new(
                    ::aws_smithy_runtime_api::http::StatusCode::try_from(
                        super::wire::CLIENT_ERROR_STATUS,
                    )
                    .expect("the client-error status of this service is a status"),
                    ::aws_smithy_types::body::SdkBody::from(
                        format!("{{\"__type\":\"{ACCESS_DENIED_CODE}\"}}").as_bytes(),
                    ),
                )
            }
        }
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for IamAuthorized<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // What the CLOCK and the KEY owe this service's records does not depend on
            // which call is being made, or on whether this caller may make it: an
            // execution of a state machine on a customer managed KMS key does its
            // remaining work under that key, so an outcome that comes due while the key
            // cannot be used for the work is a FAILURE rather than the outcome the walk
            // computed ([`crate::state::encryption::fail_due_runs_the_key_refuses`]).
            // It is materialised here, in front of every call this service serves, for
            // the same reason the management event behind this seam is recorded here:
            // it is a property of the service answering at this instant and of no one
            // operation, so no handler can forget it and none decides it twice.
            crate::state::encryption::fail_due_runs_the_key_refuses(
                &*self.engine,
                crate::state::now(),
            );
            // WHICH operation this is, read off the routing key this protocol resolves —
            // the same key the generated table is keyed by, so the action asked about
            // cannot drift from the operation served. A request the key cannot be read
            // from names no operation of this service and is left to the framework, which
            // answers it as an unroutable request.
            if let Ok(operation) =
                ::aws_smithy_schema_server::rpc::routing_key_from_x_amz_target(req)
            {
                if let Some(message) = denial(&*self.engine, ctx, operation, req) {
                    ::tracing::info!(
                        operation = %operation,
                        "iam does not allow this caller to perform this action"
                    );
                    return ::service_common::query_error_envelope::with_request_id_header(
                        self.denied_response(&message),
                    );
                }
            }
            self.served.run(svc, req, ctx).await
        })
    }

    /// `None` is "defer to the modelled operation the routing key names", which is the
    /// operation a permitted request is served through: wrapping must not change which
    /// schemes sign an operation.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        None
    }
}

#[cfg(test)]
mod tests {
    use ::serde_json::json;

    use super::*;

    /// The Region the ARNs below are read in.
    const REGION: &str = "us-west-2";

    /// A minted task id, in the 8-4-4-4-12 lowercase-hexadecimal form this service renders
    /// a version-4 UUID in, so a token built from it is one this service would have minted.
    const MINTED_ID: &str = "0f1e2d3c-4b5a-4968-8776-65544332211a";

    /// Every resource of the kind `operation`'s API is about, in that Region: what a request
    /// naming no resource of its own is judged against.
    fn any_resource_of_the_region(operation: &str) -> String {
        any_resource(operation, REGION)
    }

    /// RULE: a request is judged against the ARN it NAMES, in the member ITS OWN
    /// operation names its resource by; one addressed by a TASK TOKEN is judged against the
    /// resource that token BELONGS TO; one that names neither is judged against the ARN its
    /// name mints, and otherwise against every resource of this service.
    ///
    /// The rows span every member of [`ADDRESSED_BY`] (including operations the issue this
    /// rule was written for did not cite), the precedence between the two members one
    /// operation may carry, both creates, every operation of
    /// [`ADDRESSED_BY_TASK_TOKEN`] and both kinds of resource a token names, the ways a
    /// request names no resource at all, and a member another operation would be judged by
    /// carried on an operation that does not model it.
    #[test]
    fn a_request_is_judged_against_the_arn_it_names() {
        let machine = crate::state::state_machine_arn(REGION, "addressed");
        let execution = crate::state::execution_arn(REGION, "addressed", "one");
        let activity = crate::state::activity_arn(REGION, "worker");
        let map_run = crate::state::map_run_arn(REGION, "addressed", "label", "map-run-id");
        let alias = crate::state::state_machine_alias_arn(&machine, "PROD");
        let version = crate::state::state_machine_version_arn(&machine, 1);
        // (case, operation, the request body, the resource it addresses)
        let rows: Vec<(&str, &str, ::serde_json::Value, String)> = vec![
            (
                "a read addressed by execution ARN",
                "DescribeExecution",
                json!({ "executionArn": execution }),
                execution.clone(),
            ),
            (
                "a start addressed by state machine ARN",
                "StartExecution",
                json!({ "stateMachineArn": machine, "name": "run" }),
                machine.clone(),
            ),
            (
                "a list addressed by Map Run ARN alone",
                "ListExecutions",
                json!({ "mapRunArn": map_run }),
                map_run.clone(),
            ),
            (
                "a list naming both a state machine and a Map Run",
                "ListExecutions",
                json!({ "stateMachineArn": machine, "mapRunArn": map_run }),
                machine.clone(),
            ),
            (
                "an alias read addressed by alias ARN",
                "DescribeStateMachineAlias",
                json!({ "stateMachineAliasArn": alias }),
                alias.clone(),
            ),
            (
                "a version delete addressed by version ARN",
                "DeleteStateMachineVersion",
                json!({ "stateMachineVersionArn": version }),
                version.clone(),
            ),
            (
                "an activity poll addressed by activity ARN",
                "GetActivityTask",
                json!({ "activityArn": activity, "workerName": "worker-1" }),
                activity.clone(),
            ),
            (
                "a tag read addressed by resourceArn",
                "ListTagsForResource",
                json!({ "resourceArn": machine }),
                machine.clone(),
            ),
            (
                "a state machine create, judged against the ARN its name mints",
                "CreateStateMachine",
                json!({ "name": "minted", "definition": "{}", "roleArn": "role" }),
                crate::state::state_machine_arn(REGION, "minted"),
            ),
            (
                "an activity create, judged against the ARN its name mints",
                "CreateActivity",
                json!({ "name": "minted" }),
                crate::state::activity_arn(REGION, "minted"),
            ),
            (
                "an alias create naming no version for it to route to",
                "CreateStateMachineAlias",
                json!({ "name": "PROD", "routingConfiguration": [] }),
                any_resource_of_the_region("CreateStateMachineAlias"),
            ),
            (
                "an alias create, judged against the ARN it would mint",
                "CreateStateMachineAlias",
                json!({
                    "name": "PROD",
                    "routingConfiguration": [{
                        "stateMachineVersionArn": version,
                        "weight": 100,
                    }],
                }),
                alias.clone(),
            ),
            (
                "a listing that names no resource",
                "ListStateMachines",
                json!({}),
                any_resource_of_the_region("ListStateMachines"),
            ),
            (
                "a settlement addressed by a callback task's token",
                "SendTaskSuccess",
                json!({
                    "taskToken": crate::state::task_token(MINTED_ID, &execution),
                    "output": "{}",
                }),
                machine.clone(),
            ),
            (
                "a failure addressed by an activity task's token",
                "SendTaskFailure",
                json!({ "taskToken": crate::state::task_token(MINTED_ID, &activity) }),
                activity.clone(),
            ),
            (
                "a heartbeat addressed by a callback task's token",
                "SendTaskHeartbeat",
                json!({ "taskToken": crate::state::task_token(MINTED_ID, &execution) }),
                machine.clone(),
            ),
            (
                "a task token this service never minted",
                "SendTaskSuccess",
                json!({ "taskToken": "token", "output": "{}" }),
                any_resource_of_the_region("SendTaskSuccess"),
            ),
            (
                "a state tested with no state machine around it",
                "TestState",
                json!({ "definition": r#"{"Type":"Pass","End":true}"# }),
                any_resource_of_the_region("TestState"),
            ),
            (
                "a task token carried on an operation that names none",
                "DescribeExecution",
                json!({ "taskToken": crate::state::task_token(MINTED_ID, &execution) }),
                any_resource_of_the_region("DescribeExecution"),
            ),
            (
                "a member carrying no text at all",
                "DescribeExecution",
                json!({ "executionArn": "" }),
                any_resource_of_the_region("DescribeExecution"),
            ),
            (
                "a member carrying a value of another type",
                "DescribeExecution",
                json!({ "executionArn": 7 }),
                any_resource_of_the_region("DescribeExecution"),
            ),
            (
                "a member the operation does not model, carried beside the one it does",
                "StartExecution",
                json!({ "stateMachineArn": machine, "executionArn": execution }),
                machine.clone(),
            ),
            (
                "a member the operation does not model, carried alone",
                "ListStateMachines",
                json!({ "stateMachineArn": machine }),
                any_resource_of_the_region("ListStateMachines"),
            ),
        ];
        for (case, operation, body, expected) in rows {
            let addressed = addressed_in(Some(&body), operation, REGION);
            assert_eq!(
                addressed, expected,
                "case {case:?}: expected the request to address {expected:?}, got {addressed:?}"
            );
        }
        // A body that is no JSON document at all names nothing either.
        let unreadable = addressed_in(None, "DescribeExecution", REGION);
        let every_resource = any_resource_of_the_region("DescribeExecution");
        assert_eq!(
            unreadable, every_resource,
            "case \"a body that is not a JSON document\": expected {every_resource:?}, got \
             {unreadable:?}"
        );
    }

    /// RULE: a request asks for the action of its own operation, plus the action of every
    /// capability a member of it switches ON and of every permission a member of it
    /// IMPLIES — so a capability member left at its `false` default, and a member that
    /// writes nothing, ask for nothing more than the operation.
    ///
    /// The rows span the capability switched on and off, left out, carrying a value of
    /// another type, an operation that models no capability at all, the capability
    /// member carried on an operation that does not model it, and each implied permission:
    /// tagging on both creates, publishing on the create and on the update, both together,
    /// and an empty tag list, which writes no tag.
    #[test]
    fn a_request_asks_for_the_action_of_every_capability_it_switches_on() {
        let pass = r#"{"Type":"Pass","End":true}"#;
        // (case, operation, the request body, the actions it asks for)
        let rows: Vec<(&str, &str, ::serde_json::Value, Vec<&str>)> = vec![
            (
                "the capability switched on",
                "TestState",
                json!({ "definition": pass, "revealSecrets": true }),
                vec!["states:TestState", "states:RevealSecrets"],
            ),
            (
                "the capability switched off",
                "TestState",
                json!({ "definition": pass, "revealSecrets": false }),
                vec!["states:TestState"],
            ),
            (
                "the capability left out",
                "TestState",
                json!({ "definition": pass }),
                vec!["states:TestState"],
            ),
            (
                "the capability member carrying a value of another type",
                "TestState",
                json!({ "definition": pass, "revealSecrets": "true" }),
                vec!["states:TestState"],
            ),
            (
                "an operation that models no capability",
                "DescribeExecution",
                json!({ "executionArn": "arn" }),
                vec!["states:DescribeExecution"],
            ),
            (
                "the capability member on an operation that does not model it",
                "ListStateMachines",
                json!({ "revealSecrets": true }),
                vec!["states:ListStateMachines"],
            ),
            (
                "a create that tags what it creates",
                "CreateStateMachine",
                json!({
                    "name": "runner",
                    "definition": pass,
                    "roleArn": "role",
                    "tags": [{ "key": "team", "value": "payments" }],
                }),
                vec!["states:CreateStateMachine", "states:TagResource"],
            ),
            (
                "a create carrying an empty tag list, which tags nothing",
                "CreateStateMachine",
                json!({ "name": "runner", "definition": pass, "roleArn": "role", "tags": [] }),
                vec!["states:CreateStateMachine"],
            ),
            (
                "an activity create that tags what it creates",
                "CreateActivity",
                json!({ "name": "worker", "tags": [{ "key": "team", "value": "payments" }] }),
                vec!["states:CreateActivity", "states:TagResource"],
            ),
            (
                "a create that publishes a version",
                "CreateStateMachine",
                json!({
                    "name": "runner",
                    "definition": pass,
                    "roleArn": "role",
                    "publish": true,
                }),
                vec![
                    "states:CreateStateMachine",
                    "states:PublishStateMachineVersion",
                ],
            ),
            (
                "a create that tags and publishes",
                "CreateStateMachine",
                json!({
                    "name": "runner",
                    "definition": pass,
                    "roleArn": "role",
                    "publish": true,
                    "tags": [{ "key": "team", "value": "payments" }],
                }),
                vec![
                    "states:CreateStateMachine",
                    "states:TagResource",
                    "states:PublishStateMachineVersion",
                ],
            ),
            (
                "an update that publishes a version",
                "UpdateStateMachine",
                json!({ "stateMachineArn": "arn", "publish": true }),
                vec![
                    "states:UpdateStateMachine",
                    "states:PublishStateMachineVersion",
                ],
            ),
            (
                "an update that publishes nothing",
                "UpdateStateMachine",
                json!({ "stateMachineArn": "arn", "publish": false }),
                vec!["states:UpdateStateMachine"],
            ),
            (
                "a tagging call, whose own action is the tagging one",
                "TagResource",
                json!({ "resourceArn": "arn", "tags": [{ "key": "team", "value": "x" }] }),
                vec!["states:TagResource"],
            ),
        ];
        for (case, operation, body, expected) in rows {
            let asked = requested_actions(Some(&body), operation);
            assert_eq!(
                asked, expected,
                "case {case:?}: expected the request to ask for {expected:?}, got {asked:?}"
            );
        }
        // A body that is no JSON document at all switches nothing on.
        let asked = requested_actions(None, "TestState");
        assert_eq!(
            asked,
            vec!["states:TestState".to_string()],
            "case \"a body that is not a JSON document\": expected the operation's action alone, \
             got {asked:?}"
        );
    }
}
