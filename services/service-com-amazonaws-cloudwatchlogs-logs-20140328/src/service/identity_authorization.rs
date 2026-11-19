// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the principal that SIGNED a request perform the action it asked for —
//! the authorization step taken in FRONT of every operation, before anything is read or
//! written.
//!
//! Every operation of this API is signed with IAM credentials (`sigv4` is the only auth
//! scheme the model declares, `smithy_query(query="service_info")`), and AWS decides such a
//! call at the IAM layer before it is dispatched: a caller whose identity-based policies do
//! not allow the action on the resource the request names is told it is not authorized
//! rather than told anything about what the account holds, and nothing of the account
//! changes on the way to that refusal. The decision is taken per request, so a statement
//! `iam:PutRolePolicy` wrote refuses the next call and one `iam:DeleteRolePolicy` removed
//! stops granting it — which is what makes a client's least-privilege test, and a
//! revocation drill, mean anything here.
//!
//! No error shape of this model carries that refusal for most of its operations — the error
//! sets in the crate root's `OPERATIONS.md` declare `AccessDeniedException` on a minority of
//! them — because the refusal belongs to the authorization layer ahead of the service's own
//! code rather than to any one operation: it is the AWS-common `AccessDeniedException`,
//! answered here through this service's own scaffold channel
//! ([`crate::scaffolded::ACCESS_DENIED_CODE`], HTTP
//! [`crate::scaffolded::ACCESS_DENIED_STATUS`]) so a caller reads the same code and status
//! whichever operation it called.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who it is, what it may do, and the decision** —
//!    [`crate::state::identity_authorization`], the one place this service resolves the
//!    principal an access key id speaks for, reads the identity-based documents and the
//!    permissions boundary in force on it, and asks
//!    [`::service_common::iam_authorization`] about an `(action, resource)` pair. The
//!    callers it does not bound — the account's own published credential, which every
//!    in-process call another emulated service makes to this one presents, a principal
//!    carrying a grant this emulator cannot read, and an engine with no STS or IAM store —
//!    are named there, as is the sentence a refusal carries. Nothing is cached, so a policy
//!    a write to IAM changed decides the next call;
//! 2. **what the request asks for** — the action is the operation's own name under this
//!    service's namespace ([`crate::state::identity_authorization::action_for`]) and the
//!    resource is read off the request alone here ([`addressed_resource`]) before anything
//!    is looked up, so the decision cannot depend on whether that resource exists and a
//!    refused caller learns nothing about what the account holds;
//! 3. **where the decision is taken** — this module's [`IamAuthorized`], which every row of
//!    the routing table this service serves its requests from is wrapped in
//!    ([`crate::service::management_events::audited_table`]), in front of the operation that
//!    serves the call. Wrapping is what makes it ONE decision: every operation this service
//!    serves is judged by the same rule before its handler, so no handler can forget it.
//!    The call this gate REFUSES is recorded as a management event of this service, because
//!    it reaches no handler and the shared audit wrapper therefore never sees it (see
//!    [`crate::service::management_events::record_refused_at_authorization`]).
//!
//! ## The resource a request is judged against
//!
//! CloudWatch Logs has TWO resource types an action of its own authorizes against, and a
//! call is judged against the one it ADDRESSES:
//!
//! * a call that names a log STREAM is judged against that stream's own ARN,
//!   `arn:<partition>:logs:<region>:<account>:log-group:<group>:log-stream:<stream>` — what a
//!   read or a write of one stream's events is performed on;
//! * a call that names a log GROUP and no stream is judged against the GROUP's own ARN,
//!   `arn:<partition>:logs:<region>:<account>:log-group:<group>`, which carries no
//!   `log-stream:` segment at all: a group-scoped call is not a call on a stream, so a
//!   statement written `log-group:<group>:*` — the form that covers what is UNDER the group —
//!   does not reach it, while `log-group:<group>` and the prefix wildcards above it do.
//!
//! So:
//!
//! * a request names the group it addresses in a member of its own: `logGroupName`, the
//!   name, or `logGroupIdentifier`, "the name or ARN of the log group" (the two members this
//!   model names a log group by), and the stream in `logStreamName`;
//! * an identifier that already carries an ARN names that group itself, whichever account
//!   and Region holds it, so it is judged as carried;
//! * a request that names an ARN OF THIS SERVICE in `resourceArn` addresses that resource —
//!   a tagging call, whose `AmazonResourceName` is the resource being tagged — so it too is
//!   judged as carried. An ARN of ANOTHER service carried there is not a resource of this
//!   one (a delivery source's `resourceArn` is the resource that SENDS the logs), so it
//!   decides nothing;
//! * a request that names none of them — a listing, a call addressed by a query id or a task
//!   token, a call about one of this service's other resource kinds — addresses no single
//!   resource of either type, which this service states as its stream ARN with BOTH segments
//!   empty: a statement covering the account's log groups allows such a call and one scoped
//!   to a single group does not;
//! * a request that carries NO MEMBER AT ALL has nothing in it to name a resource with, so
//!   there is no ARN to build from it: such a call is about every log group of the account,
//!   and is judged against this service's log-group ARN carrying IAM's own wildcard
//!   ([`ADDRESSED_BY_NO_MEMBER`]).
//!
//! Every part comes from the request and from the credential scope it was signed under, so
//! nothing is looked up to decide a call and a refused caller learns nothing about what the
//! account holds.
//!
//! ## The operations that take the decision themselves
//!
//! [`DECIDED_BY_THE_OPERATION`] names the operations this gate does not judge — because the
//! call acts on SEVERAL resources, or on one that is not a member of the request, so the
//! handler is the only place that knows which ARNs the decision is about. This gate defers to
//! them rather than judging the same call twice against a resource their handler would not
//! use. Where such a handler can name the resource, it asks
//! [`crate::state::identity_authorization::refusal`] itself and answers the same sentence,
//! from the same evaluator, through its own modelled `AccessDeniedException`; where the
//! request carries no resolvable resource at all, there is nothing to decide and the handler
//! refuses the request for what IS wrong with it — the order AWS answers in, since a call
//! whose target cannot be resolved is not a call on any resource.

use ::std::sync::Arc;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_schema_server::protocol::ServerProtocol;
use ::service_common::audited_operation::CallKind;
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

use crate::state::identity_authorization::refusal;

/// The member a request names the log group it addresses by.
const LOG_GROUP_NAME_MEMBER: &str = "logGroupName";

/// The member a request names the log group it addresses by when the model lets it carry
/// either the name or the ARN ("the name or ARN of the log group").
const LOG_GROUP_IDENTIFIER_MEMBER: &str = "logGroupIdentifier";

/// The member a request names the log stream it addresses by.
const LOG_STREAM_NAME_MEMBER: &str = "logStreamName";

/// The member a request names the resource it addresses by its ARN
/// (`AmazonResourceName`) — what a tagging call of this service is addressed by.
const RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// The operations this gate does not judge, because the resource the call addresses is not a
/// member of the request (see the module docs).
///
/// SOURCE, for adding an entry: a handler that asks
/// [`crate::state::identity_authorization::refusal`] itself, or one whose request names no
/// resolvable resource for a decision to be about. Sorted, which
/// [`decided_by_the_operation`] relies on for `binary_search`.
pub const DECIDED_BY_THE_OPERATION: &[&str] = &[
    // "Creates a delivery … by pairing exactly one delivery source and one delivery
    // destination": the call acts on BOTH, so its handler decides it against each of them
    // in the order the request names them.
    "CreateDelivery",
    // The resource this call addresses is named by NOTHING in the request except the
    // `logObjectPointer` it carries: the pointer is the service's own handle on one large
    // log object, so which resource the call reaches is whatever the pointer decodes to. A
    // pointer this service did not issue decodes to no resource at all, so there is nothing
    // to decide the call against and the operation refuses it as the bad parameter it is,
    // reading and writing nothing of the store on the way.
    "GetLogObject",
    // The call addresses the DELIVERY its `id` names, whose ARN its handler builds from
    // that id.
    "UpdateDeliveryConfiguration",
];

/// Whether `operation` takes the identity decision in its own handler.
#[must_use]
pub fn decided_by_the_operation(operation: &str) -> bool {
    DECIDED_BY_THE_OPERATION.binary_search(&operation).is_ok()
}

/// The operations whose REQUEST carries no member at all, so there is nothing in it to name
/// a log group — or anything else — with.
///
/// Such a call is not addressed to one record: it is about the account's log groups as a
/// whole, which is the resource IAM's own wildcard spells
/// ([`crate::state::identity_authorization::ANY_RESOURCE`]) under this service's log-group
/// ARN. A request that DOES carry members naming a log group is judged against the resource
/// those members address — the group's own ARN, or the stream's where it names one (see the
/// module docs) — and one that carries members naming none against this service's stream ARN
/// with both segments empty, which is the resource a listing addresses. All three are
/// different resources.
///
/// SOURCE, for adding an entry: an operation whose modelled input declares no members,
/// `smithy_query(query="operation", operation="<Operation>")`. Sorted, which
/// [`addressed_by_no_member`] relies on for `binary_search`.
pub const ADDRESSED_BY_NO_MEMBER: &[&str] = &["GetStorageTierPolicy"];

/// Whether `operation` is addressed by no member of its request (see
/// [`ADDRESSED_BY_NO_MEMBER`]).
#[must_use]
pub fn addressed_by_no_member(operation: &str) -> bool {
    ADDRESSED_BY_NO_MEMBER.binary_search(&operation).is_ok()
}

/// The resource this request is judged against: what it ADDRESSES (see the module docs).
///
/// Read off the request's own body, which on this protocol is the operation's input as
/// JSON. A body that is not a JSON document at all addresses nothing of this service, so it
/// is judged exactly as a request that named no log group.
fn addressed_resource(ctx: &OperationContext<'_>, operation: &str, req: &RtRequest) -> String {
    let region = crate::state::request_region(ctx.parts());
    if addressed_by_no_member(operation) {
        return log_group_resource_wildcard(&region);
    }
    let submitted: Option<::serde_json::Value> = req
        .body()
        .bytes()
        .and_then(|body| ::serde_json::from_slice(body).ok());
    addressed_in(submitted.as_ref(), &region)
}

/// The resource the submitted input `submitted` addresses, in `region` (see the module
/// docs).
///
/// A member that is absent, is not a string or is empty names no resource, so such a request
/// is judged exactly as one that named none at all.
fn addressed_in(submitted: Option<&::serde_json::Value>, region: &str) -> String {
    let member = |name: &str| {
        submitted
            .and_then(|body| body.get(name))
            .and_then(::serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let named = member(LOG_GROUP_NAME_MEMBER).or_else(|| member(LOG_GROUP_IDENTIFIER_MEMBER));
    // An identifier that is already an ARN names the group itself, whichever account and
    // Region holds it, so it is judged as carried rather than rebuilt here.
    if let Some(identifier) = named.filter(|value| is_arn(value)) {
        return identifier.to_string();
    }
    // A request carrying no log group at all, but naming an ARN of THIS service as the
    // resource it addresses, is judged against that ARN (see the module docs).
    if let Some(resource) = named
        .is_none()
        .then(|| member(RESOURCE_ARN_MEMBER).filter(|value| is_own_arn(value)))
        .flatten()
    {
        return resource.to_string();
    }
    log_group_resource(
        region,
        named.unwrap_or_default(),
        member(LOG_STREAM_NAME_MEMBER).unwrap_or_default(),
    )
}

/// Whether `value` is an ARN at all.
fn is_arn(value: &str) -> bool {
    crate::state::arn_service_and_resource(value).is_some()
}

/// Whether `value` is an ARN of THIS service — one whose vendor field is this service's own
/// namespace, which is what makes it a resource this service's actions are decided against.
fn is_own_arn(value: &str) -> bool {
    crate::state::arn_service_and_resource(value)
        .is_some_and(|(service, _)| service == crate::state::identity_authorization::ACTION_PREFIX)
}

/// The resource ARN CloudWatch Logs names a call on the log group `group` and the log stream
/// `stream` by: the GROUP's own ARN where the request names a group and no stream, and the
/// STREAM's ARN otherwise (see the module docs).
///
/// Both forms come from this service's own ARN builders, so the resource a call is decided
/// against is spelled exactly as the resource this service reports for it.
fn log_group_resource(region: &str, group: &str, stream: &str) -> String {
    if stream.is_empty() && !group.is_empty() {
        return crate::state::log_group_arn(region, group, false);
    }
    crate::state::log_stream_arn(region, group, stream)
}

/// The resource ARN CloudWatch Logs names a call on EVERY log group of the account by: its
/// log-group ARN carrying IAM's own wildcard
/// ([`crate::state::identity_authorization::ANY_RESOURCE`]). See [`ADDRESSED_BY_NO_MEMBER`]
/// for which calls those are.
fn log_group_resource_wildcard(region: &str) -> String {
    ::engine::emulator_arn(
        crate::state::identity_authorization::ACTION_PREFIX,
        region,
        &format!(
            "log-group:{}",
            crate::state::identity_authorization::ANY_RESOURCE
        ),
    )
}

/// Every operation of this service, served only to a caller IAM allows to perform it.
///
/// It holds no operation of its own beyond the dispatch it wraps: the one it serves a
/// permitted request through is the dispatch this service already answers
/// `Service::override_operation` with, so the framework still owns the whole deserialize ->
/// validate -> handle -> serialize sequence and every status, code and message it answers
/// with.
pub struct IamAuthorized<Eng> {
    /// The operation shape name, which is both this entry's routing key and the name of the
    /// IAM action the request is judged under — and the `eventName` the record of a refused
    /// call carries.
    name: &'static str,
    /// The shared engine, so the principal's policies are read from the one store every
    /// service of this emulator shares.
    engine: Arc<Eng>,
    /// What the wrapped operation's own semantics make of a call to it, for the record of a
    /// call this gate REFUSES: the kind is the event's `readOnly`, so a refused read is
    /// recorded as a read exactly as the read itself would have been. `None` for an operation
    /// whose calls are no management events at all
    /// ([`super::management_events::NOT_A_MANAGEMENT_EVENT`]), which has no record when it is
    /// refused either.
    kind: Option<CallKind>,
    /// Serves the request the decision admitted.
    served: Box<dyn ErasedOperation>,
}

impl<Eng> IamAuthorized<Eng> {
    /// Put `served`, already constructed from the shared engine, behind the IAM decision,
    /// recording a refused call as a management event of `kind` where its calls are ones.
    pub fn new(
        name: &'static str,
        engine: Arc<Eng>,
        kind: Option<CallKind>,
        served: Box<dyn ErasedOperation>,
    ) -> Self {
        Self {
            name,
            engine,
            kind,
            served,
        }
    }

    /// The refusal as this endpoint puts it on the wire: the AWS-common
    /// `AccessDeniedException`, rendered in the same `awsJson1_1` envelope every other error
    /// of this endpoint is rendered in.
    fn denied_response(&self, message: &str) -> RtResponse {
        let refused = crate::scaffolded::AccessDeniedException::new(message);
        let rendered = ::aws_smithy_schema_server::rpc::AwsJson11().serialize_error(
            &refused,
            &crate::scaffolded::ACCESS_DENIED_SCHEMA,
            crate::scaffolded::ACCESS_DENIED_CODE,
            crate::scaffolded::ACCESS_DENIED_STATUS,
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
                        crate::scaffolded::ACCESS_DENIED_STATUS,
                    )
                    .expect("the client-error status of this service is a status"),
                    ::aws_smithy_types::body::SdkBody::from(
                        format!(
                            "{{\"__type\":\"{}\"}}",
                            crate::scaffolded::ACCESS_DENIED_CODE
                        )
                        .as_bytes(),
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
            // WHICH operation this is: the name this entry serves, which is the routing key
            // the protocol resolves for it, so the action asked about cannot drift from the
            // operation served.
            let operation = self.name;
            if !decided_by_the_operation(operation) {
                let resource = addressed_resource(ctx, operation, req);
                if let Some(message) = refusal(&*self.engine, ctx, operation, &resource) {
                    ::tracing::info!(
                        operation = %operation,
                        resource = %resource,
                        "iam does not allow this caller to perform this action"
                    );
                    if let Some(kind) = self.kind {
                        // The refused call is a management event of this service too: event
                        // history holds the calls this account's API answered, and a client
                        // alarming on denied attempts reads exactly these. Recorded here
                        // because the call reaches no handler, so the shared wrapper around
                        // the operation never sees it (see [`super::management_events`]), and
                        // as the LAST thing before the answer is rendered, with nothing
                        // awaited in between.
                        super::management_events::record_refused_at_authorization(
                            &*self.engine,
                            ctx,
                            operation,
                            kind,
                            &message,
                        );
                    }
                    return self.denied_response(&message);
                }
            }
            self.served.run(svc, req, ctx).await
        })
    }

    /// The wrapped dispatch's own answer: `None` is "defer to the modelled operation the
    /// routing key names", so wrapping must not change which schemes sign an operation.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.served.auth_schemes()
    }
}

#[cfg(test)]
mod tests {
    use ::serde_json::json;

    use super::*;
    use crate::state::identity_authorization::action_for;

    /// The Region the ARNs below are read in.
    const REGION: &str = "us-west-2";

    /// RULE: a request is judged against the log group and log stream it NAMES — by name, or
    /// by the ARN an identifier carries — against the ARN OF THIS SERVICE it names as the
    /// resource it addresses, and otherwise against this service's stream ARN with the group
    /// and stream segments left empty. A request naming a group and no stream is judged
    /// against the GROUP's own ARN, which carries no `log-stream:` segment.
    ///
    /// The rows span both members this model names a log group by, a name and an ARN in the
    /// member that admits either, a request naming a stream as well, a tagging call addressed
    /// by this service's own ARN, one carrying ANOTHER service's ARN there, the three ways a
    /// request names no group (no member, an empty one, one carrying another type), a group of
    /// ANOTHER account reached by ARN, and operations the issue this rule was written for did
    /// not cite.
    #[test]
    fn a_request_is_judged_against_the_log_group_it_names() {
        let group = "/aws/lambda/handler";
        let elsewhere = "arn:aws:logs:eu-central-1:210987654321:log-group:shared:*";
        let account = ::engine::emulator_account_id();
        let group_arn =
            |group: &str| format!("arn:aws:logs:{REGION}:{account}:log-group:{group}");
        let stream_arn = |group: &str, stream: &str| {
            format!("arn:aws:logs:{REGION}:{account}:log-group:{group}:log-stream:{stream}")
        };
        // (case, the request body, the resource it addresses)
        let rows: Vec<(&str, ::serde_json::Value, String)> = vec![
            (
                "a write addressed by log group name and stream name",
                json!({ "logGroupName": group, "logStreamName": "2026/01/01/[$LATEST]a" }),
                stream_arn(group, "2026/01/01/[$LATEST]a"),
            ),
            (
                "a group-level call naming no stream",
                json!({ "logGroupName": group, "retentionInDays": 7 }),
                group_arn(group),
            ),
            (
                "a read addressed by log group identifier",
                json!({ "logGroupIdentifier": group }),
                group_arn(group),
            ),
            (
                "an identifier carrying the group's own ARN",
                json!({ "logGroupIdentifier": elsewhere }),
                elsewhere.to_string(),
            ),
            (
                "a name beside an identifier",
                json!({ "logGroupName": group, "logGroupIdentifier": elsewhere }),
                group_arn(group),
            ),
            (
                "a listing that names no log group",
                json!({ "logGroupNamePrefix": "/aws/" }),
                stream_arn("", ""),
            ),
            (
                "a tagging call addressed by this service's own ARN",
                json!({ "resourceArn": group_arn(group) }),
                group_arn(group),
            ),
            (
                "a call carrying another service's ARN as its resource",
                json!({ "name": "vended", "resourceArn": "arn:aws:s3:::vended-logs" }),
                stream_arn("", ""),
            ),
            (
                "a member carrying no text at all",
                json!({ "logGroupName": "" }),
                stream_arn("", ""),
            ),
            (
                "a member carrying a value of another type",
                json!({ "logGroupName": 7 }),
                stream_arn("", ""),
            ),
            (
                "a call addressed by a query id",
                json!({ "queryId": "9f1b" }),
                stream_arn("", ""),
            ),
            (
                "a stream read of a group reached by its own ARN",
                json!({ "logGroupIdentifier": group, "logStreamName": "one" }),
                stream_arn(group, "one"),
            ),
        ];
        for (case, body, expected) in rows {
            let addressed = addressed_in(Some(&body), REGION);
            assert_eq!(
                addressed, expected,
                "case {case:?}: expected the request to address {expected:?}, got {addressed:?}"
            );
        }
        // A body that is no JSON document at all names nothing either.
        let unreadable = addressed_in(None, REGION);
        let expected = stream_arn("", "");
        assert_eq!(
            unreadable, expected,
            "case \"a body that is not a JSON document\": expected {expected:?}, got \
             {unreadable:?}"
        );
    }

    /// RULE: a request that carries no member at all is judged against every log group of the
    /// account, spelled with IAM's wildcard — not against the ARN a request naming no group
    /// builds, which is a different resource.
    #[test]
    fn a_request_with_no_member_is_judged_against_every_log_group() {
        let mut sorted = ADDRESSED_BY_NO_MEMBER.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            ADDRESSED_BY_NO_MEMBER.to_vec(),
            "the list is searched with binary_search, so it must stay sorted"
        );
        let account = ::engine::emulator_account_id();
        let wildcard = format!("arn:aws:logs:{REGION}:{account}:log-group:*");
        for operation in ADDRESSED_BY_NO_MEMBER {
            let addressed = log_group_resource_wildcard(REGION);
            assert_eq!(
                addressed, wildcard,
                "case {operation:?}: expected the account's log groups {wildcard:?}, got \
                 {addressed:?}"
            );
            assert!(
                addressed_by_no_member(operation),
                "case {operation:?}: expected the gate to judge it against every log group"
            );
        }
        // An operation whose request DOES carry members is judged against what they name,
        // even when the client sent none of them.
        for operation in ["DescribeLogGroups", "PutStorageTierPolicy", "GetLogEvents"] {
            assert!(
                !addressed_by_no_member(operation),
                "case {operation:?}: expected the resource to be read off the request"
            );
        }
    }

    /// RULE: this gate judges every operation of the service except the ones whose own
    /// handler takes the decision.
    #[test]
    fn the_gate_defers_only_to_the_operations_that_decide_themselves() {
        let mut sorted = DECIDED_BY_THE_OPERATION.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            DECIDED_BY_THE_OPERATION.to_vec(),
            "the deferral list is searched with binary_search, so it must stay sorted"
        );
        for operation in DECIDED_BY_THE_OPERATION {
            assert!(
                decided_by_the_operation(operation),
                "case {operation:?}: expected the gate to defer to the operation"
            );
        }
        for operation in [
            "DescribeLogGroups",
            "PutLogEvents",
            "GetDelivery",
            "PutDeliverySource",
            "TagResource",
        ] {
            assert!(
                !decided_by_the_operation(operation),
                "case {operation:?}: expected the gate to decide the request itself"
            );
        }
    }

    /// RULE: the action asked about is the operation's own name under this service's
    /// namespace, so one statement scoped to this service's actions covers every operation
    /// of it.
    #[test]
    fn the_action_asked_about_is_the_operations_own_name() {
        for (case, operation, expected) in [
            (
                "the listing the gate was written for",
                "DescribeLogGroups",
                "logs:DescribeLogGroups",
            ),
            (
                "an operation the issue did not cite",
                "PutRetentionPolicy",
                "logs:PutRetentionPolicy",
            ),
        ] {
            let action = action_for(operation);
            assert_eq!(
                action, expected,
                "case {case:?}: expected the action {expected:?}, got {action:?}"
            );
        }
    }
}
