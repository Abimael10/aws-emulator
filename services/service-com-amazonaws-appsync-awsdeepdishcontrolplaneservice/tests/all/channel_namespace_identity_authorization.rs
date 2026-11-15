// SPDX-License-Identifier: CC0-1.0

//! IAM authorization in front of the calls that address ONE channel namespace of an Event
//! API — `GetChannelNamespace`, `UpdateChannelNamespace` and `DeleteChannelNamespace`, which
//! share the URI `/v2/apis/{apiId}/channelNamespaces/{name}` and differ in the method and
//! the action each names.
//!
//! The rule under test: one of these calls is served only to a caller whose identity-based
//! IAM policies allow THAT call's own action (`appsync:<OperationName>`) on the channel
//! namespace the request addresses — the namespace's own ARN, which is the API's ARN with
//! `/channelNamespace/<name>` under it — and every other caller is refused with the modelled
//! `AccessDeniedException` (HTTP 403,
//! `smithy_query(query="shape", shape="AccessDeniedException")`) naming the principal, the
//! action and that ARN, before the operation reads or writes anything. So a refused reader
//! learns nothing of the namespace's configuration, a refused update leaves the stored
//! configuration exactly as it was, a refused delete leaves the namespace in place, and a
//! caller the policies DO allow reaches the handler and gets its own answer — which is what
//! tells a decision apart from a blanket refusal.
//!
//! Asserted over two tables that differ along what the rule generalises over: the OPERATION
//! (a read, an update and a delete — three methods and three actions over one URI), and what
//! the caller's policies SAY (nothing at all, an allow of this action on this namespace, a
//! wildcard allow, an allow of this action on every child of the API that holds it, an allow
//! of another action of this service on this namespace, an allow of this action on another
//! namespace of the same API, and a wildcard allow beside an explicit deny of this action).
//!
//! Every principal is a real one: the users, the documents they carry and the decision taken
//! about them are IAM's own, written over the same engine through the harness
//! [`crate::identity_authorization`].

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::authorization::ACTION_PREFIX;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

use crate::identity_authorization::{
    allow, allow_all_but, arn, default_deny_message, Emulator, Reply, ACCESS_DENIED,
    ACCESS_DENIED_STATUS, ACCOUNT_KEY,
};

/// AppSync's fixed text for a channel namespace the reader cannot resolve, which is what the
/// namespace a served delete removed answers afterwards.
const NAMESPACE_NOT_FOUND: &str = "Channel namespace not found.";

/// An action of this service none of the rows below names, used for the document that allows
/// the wrong action on the right resource.
const ANOTHER_OPERATION: &str = "ListChannelNamespaces";

/// The name of a namespace a policy is written about but no request addresses, used for the
/// document that allows the right action on the wrong resource.
const ELSEWHERE: &str = "ns-elsewhere";

/// An auth-mode list naming each type in `auth_types`
/// (`AuthModes`/`AuthMode`, `smithy_query(query="shape", shape="AuthMode")`).
fn modes(auth_types: &[&str]) -> Value {
    Value::Array(auth_types.iter().map(|ty| json!({"authType": ty})).collect())
}

/// One Event API configuration whose authorization providers set up BOTH types the
/// namespaces here override with, so every mode these rows carry is a configured one and the
/// authorization decision is the only thing that can refuse them.
fn event_config() -> Value {
    json!({
        "authProviders": [{"authType": "API_KEY"}, {"authType": "AWS_IAM"}],
        "connectionAuthModes": modes(&["API_KEY"]),
        "defaultPublishAuthModes": modes(&["API_KEY"]),
        "defaultSubscribeAuthModes": modes(&["API_KEY"]),
    })
}

/// The subscribe modes every namespace here is created with, which a refused update must
/// leave in place.
fn created_subscribe_modes() -> Value {
    modes(&["API_KEY", "AWS_IAM"])
}

/// The subscribe modes the update row asks for — a different list from the created one, so
/// whether the update took effect is visible in a later read.
fn updated_subscribe_modes() -> Value {
    modes(&["AWS_IAM"])
}

/// Create one Event API with the account's own credential and answer the id the service
/// minted for it.
async fn event_api(emulator: &Emulator, label: &str) -> String {
    let created = emulator
        .call(
            ACCOUNT_KEY,
            "POST",
            "/v2/apis",
            Some(&json!({
                "name": format!("Events_ns_gate_{label}"),
                "eventConfig": event_config(),
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {label:?}: expected CreateApi 200, got {created:?}"
    );
    created.body["api"]["apiId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {label:?}: CreateApi answers an apiId, got {created:?}"))
        .to_string()
}

/// Create one channel namespace under `api_id` with the account's own credential, and answer
/// the record the create reported — the state every assertion below is made against.
async fn namespace(emulator: &Emulator, api_id: &str, name: &str) -> Value {
    let created = emulator
        .call(
            ACCOUNT_KEY,
            "POST",
            &format!("/v2/apis/{api_id}/channelNamespaces"),
            Some(&json!({
                "name": name,
                "subscribeAuthModes": created_subscribe_modes(),
                "publishAuthModes": modes(&["API_KEY"]),
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: expected CreateChannelNamespace 200, got {created:?}"
    );
    created.body["channelNamespace"].clone()
}

/// The URI one channel namespace is addressed by.
fn path(api_id: &str, name: &str) -> String {
    format!("/v2/apis/{api_id}/channelNamespaces/{name}")
}

/// One call that addresses one channel namespace: the action it names, how a client sends it,
/// and the status the HANDLER answers a caller the policies allow — which is how a served
/// call is told from a refused one.
struct Row {
    operation: &'static str,
    method: &'static str,
    body: Option<Value>,
    served_status: u16,
}

impl Row {
    /// The action a policy states about this call.
    fn action(&self) -> String {
        format!("{ACTION_PREFIX}:{}", self.operation)
    }
}

/// The three calls that address one namespace, each with the answer its handler gives a
/// caller the policies allow.
fn rows() -> Vec<Row> {
    vec![
        Row {
            operation: "GetChannelNamespace",
            method: "GET",
            body: None,
            served_status: 200,
        },
        Row {
            operation: "UpdateChannelNamespace",
            method: "POST",
            body: Some(json!({
                "subscribeAuthModes": updated_subscribe_modes(),
                "publishAuthModes": modes(&["API_KEY"]),
            })),
            served_status: 200,
        },
        Row {
            operation: "DeleteChannelNamespace",
            method: "DELETE",
            body: None,
            served_status: 200,
        },
    ]
}

/// What a document a principal carries leads to.
enum Outcome {
    /// The handler's own answer.
    Served,
    /// The modelled refusal, carrying the sentence IAM words that kind of document with.
    Refused(String),
}

/// One kind of identity-based document, written about the call under test.
#[derive(Clone, Copy)]
enum Policy {
    /// No document at all: the position a freshly created IAM user is in.
    Nothing,
    /// An allow of this call's action on the namespace the request addresses.
    ThisActionHere,
    /// An allow of every action of this service on every resource.
    Everything,
    /// An allow of this call's action on every child of the API that holds the namespace.
    ThisActionOnEveryChild,
    /// An allow of ANOTHER action of this service on the namespace addressed.
    AnotherActionHere,
    /// An allow of this call's action on another namespace of the same API.
    ThisActionElsewhere,
    /// A wildcard allow of this service with an explicit deny of this call's action.
    EverythingButThisAction,
}

impl Policy {
    const ALL: [Policy; 7] = [
        Policy::Nothing,
        Policy::ThisActionHere,
        Policy::Everything,
        Policy::ThisActionOnEveryChild,
        Policy::AnotherActionHere,
        Policy::ThisActionElsewhere,
        Policy::EverythingButThisAction,
    ];

    /// How an assertion names this document.
    fn case(self) -> &'static str {
        match self {
            Policy::Nothing => "no policy at all",
            Policy::ThisActionHere => "an allow of this action on this namespace",
            Policy::Everything => "a wildcard allow of every action on everything",
            Policy::ThisActionOnEveryChild => {
                "an allow of this action on every child of the API"
            }
            Policy::AnotherActionHere => "an allow of another action on this namespace",
            Policy::ThisActionElsewhere => "an allow of this action on another namespace",
            Policy::EverythingButThisAction => {
                "a wildcard allow with an explicit deny of this action"
            }
        }
    }

    /// The document itself, for the call `row` makes on the namespace `name` of `api_id`.
    fn document(self, row: &Row, api_id: &str, name: &str) -> Option<String> {
        let action = row.action();
        let here = arn(&state::channel_namespace_resource(api_id, name));
        match self {
            Policy::Nothing => None,
            Policy::ThisActionHere => Some(allow(&action, &here)),
            Policy::Everything => Some(allow(&format!("{ACTION_PREFIX}:*"), "*")),
            Policy::ThisActionOnEveryChild => Some(allow(
                &action,
                &arn(&format!("{}/*", state::api_resource(api_id))),
            )),
            Policy::AnotherActionHere => Some(allow(
                &format!("{ACTION_PREFIX}:{ANOTHER_OPERATION}"),
                &here,
            )),
            Policy::ThisActionElsewhere => Some(allow(
                &action,
                &arn(&state::channel_namespace_resource(api_id, ELSEWHERE)),
            )),
            Policy::EverythingButThisAction => Some(allow_all_but(&action)),
        }
    }

    /// What that document leads to for `row` on the namespace whose ARN is `resource`.
    fn outcome(self, principal_arn: &str, row: &Row, resource: &str) -> Outcome {
        match self {
            // A grant naming this action and this resource lets the call through, whether it
            // names them exactly or covers them with a wildcard.
            Policy::ThisActionHere | Policy::Everything | Policy::ThisActionOnEveryChild => {
                Outcome::Served
            }
            // An explicit deny is the strongest refusal IAM has, and it says so.
            Policy::EverythingButThisAction => Outcome::Refused(explicit_deny_message(
                principal_arn,
                &row.action(),
                resource,
            )),
            // Every other document leaves the request to IAM's default deny.
            Policy::Nothing | Policy::AnotherActionHere | Policy::ThisActionElsewhere => {
                Outcome::Refused(default_deny_message(principal_arn, row.operation, resource))
            }
        }
    }
}

/// The refusal IAM words for a request an explicit `Deny` in an identity-based policy covers.
fn explicit_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         with an explicit deny in an identity-based policy"
    )
}

/// Assert `reply` is the modelled 403 whose message is `expected`.
fn assert_refused(case: &str, operation: &str, expected: &str, reply: &Reply) {
    assert_eq!(
        (reply.status, reply.error_code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case:?} / {operation}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} \
         {:?} with {:?}",
        reply.status,
        reply.error_code,
        reply.body
    );
    assert_eq!(
        reply.message(),
        expected,
        "case {case:?} / {operation}: expected IAM's sentence {expected:?}, got {:?}",
        reply.message()
    );
}

/// RULE: a caller whose identity-based policies allow nothing is refused every call that
/// addresses one channel namespace, with the modelled `AccessDeniedException` (403) naming
/// the principal, the action and the namespace's own ARN — and the refused call performs
/// nothing, so the namespace keeps the configuration the create stored.
#[tokio::test]
async fn a_caller_no_policy_allows_is_refused_every_channel_namespace_call() {
    let emulator = Emulator::new();
    let api_id = event_api(&emulator, "zero").await;
    let principal = emulator.user("ns_zero_policy", None).await;

    for (index, row) in rows().into_iter().enumerate() {
        // A namespace of its own per row: were a refused update or delete to take effect,
        // the read below would see it.
        let name = format!("ns-zero-{index}");
        let stored = namespace(&emulator, &api_id, &name).await;
        let resource = arn(&state::channel_namespace_resource(&api_id, &name));
        // The ARN the decision is taken about is the namespace's OWN — the one the service
        // minted for it, hung off the API that holds it, rather than a spelling of this
        // suite's.
        assert_eq!(
            stored["channelNamespaceArn"].as_str(),
            Some(resource.as_str()),
            "case {name:?}: expected the stored ARN to be the one a policy names, got \
             {stored:?}"
        );

        let reply = emulator
            .call(
                &principal.access_key_id,
                row.method,
                &path(&api_id, &name),
                row.body.as_ref(),
            )
            .await;
        assert_refused(
            &name,
            row.operation,
            &default_deny_message(&principal.arn, row.operation, &resource),
            &reply,
        );

        // The decision was taken in front of the handler, so the namespace is still there
        // and still carries every value the create stored.
        let read = emulator
            .call(ACCOUNT_KEY, "GET", &path(&api_id, &name), None)
            .await;
        assert_eq!(
            read.status, 200,
            "case {name:?} / {}: expected the refused call to leave the namespace in place, \
             got {read:?}",
            row.operation
        );
        assert_eq!(
            read.body["channelNamespace"], stored,
            "case {name:?} / {}: expected the stored record untouched, got {read:?}",
            row.operation
        );
    }
}

/// RULE: what the caller's policies say decides the same signed call — a grant that names
/// this action and this namespace serves it, and anything less (another action, another
/// namespace, nothing at all) or an explicit deny refuses it with IAM's own sentence for that
/// document.
#[tokio::test]
async fn what_the_policies_say_decides_the_same_channel_namespace_call() {
    let emulator = Emulator::new();

    for (row_index, row) in rows().into_iter().enumerate() {
        let api_id = event_api(&emulator, &format!("policy{row_index}")).await;
        for (policy_index, policy) in Policy::ALL.into_iter().enumerate() {
            // A namespace of its own per case: a served update or delete changes the one it
            // addresses, so no case starts from what another case left behind.
            let name = format!("ns-{row_index}-{policy_index}");
            namespace(&emulator, &api_id, &name).await;
            let resource = arn(&state::channel_namespace_resource(&api_id, &name));
            let document = policy.document(&row, &api_id, &name);
            let principal = emulator
                .user(
                    &format!("ns_policy_{row_index}_{policy_index}"),
                    document.as_deref(),
                )
                .await;
            let case = format!("{} under {}", row.operation, policy.case());

            let reply = emulator
                .call(
                    &principal.access_key_id,
                    row.method,
                    &path(&api_id, &name),
                    row.body.as_ref(),
                )
                .await;
            match policy.outcome(&principal.arn, &row, &resource) {
                Outcome::Served => {
                    assert_eq!(
                        reply.status, row.served_status,
                        "case {case:?}: expected the handler's own {} answer, got {} {:?} \
                         with {:?}",
                        row.served_status, reply.status, reply.error_code, reply.body
                    );
                    assert_ne!(
                        reply.error_code, ACCESS_DENIED,
                        "case {case:?}: expected the call to reach the handler, got {:?}",
                        reply.body
                    );
                }
                Outcome::Refused(message) => {
                    assert_refused(&case, row.operation, &message, &reply)
                }
            }
        }
    }
}

/// RULE from the other side: a caller the policies allow reaches each of the three handlers
/// and what they do is backed by the store — the read reports the stored record, the update
/// replaces the configuration the next read reports while leaving the namespace's identity
/// and creation instant alone, and the delete makes the namespace absent.
#[tokio::test]
async fn an_allowed_caller_reaches_the_handlers_and_their_writes_take_effect() {
    let emulator = Emulator::new();
    let api_id = event_api(&emulator, "allowed").await;
    let name = "ns-allowed";
    let stored = namespace(&emulator, &api_id, name).await;
    let principal = emulator
        .user("ns_allowed", Some(&allow(&format!("{ACTION_PREFIX}:*"), "*")))
        .await;
    let uri = path(&api_id, name);

    let read = emulator
        .call(&principal.access_key_id, "GET", &uri, None)
        .await;
    assert_eq!(
        read.status, 200,
        "case \"an allowed read\": expected 200, got {read:?}"
    );
    assert_eq!(
        read.body["channelNamespace"], stored,
        "case \"an allowed read\": expected the stored record, got {read:?}"
    );

    let updated = emulator
        .call(
            &principal.access_key_id,
            "POST",
            &uri,
            Some(&json!({
                "subscribeAuthModes": updated_subscribe_modes(),
                "publishAuthModes": modes(&["API_KEY"]),
            })),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case \"an allowed update\": expected 200, got {updated:?}"
    );
    let after = emulator
        .call(&principal.access_key_id, "GET", &uri, None)
        .await;
    assert_eq!(
        after.body["channelNamespace"]["subscribeAuthModes"],
        updated_subscribe_modes(),
        "case \"an allowed update\": expected the modes the update asked for, got {after:?}"
    );
    assert_eq!(
        after.body["channelNamespace"]["channelNamespaceArn"], stored["channelNamespaceArn"],
        "case \"an allowed update\": expected the namespace's identity kept, got {after:?}"
    );
    assert_eq!(
        after.body["channelNamespace"]["created"], stored["created"],
        "case \"an allowed update\": expected the creation instant kept, got {after:?}"
    );

    let deleted = emulator
        .call(&principal.access_key_id, "DELETE", &uri, None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "case \"an allowed delete\": expected 200, got {deleted:?}"
    );
    let gone = emulator
        .call(&principal.access_key_id, "GET", &uri, None)
        .await;
    assert_eq!(
        (gone.status, gone.error_code.as_str()),
        (404, "NotFoundException"),
        "case \"an allowed delete\": expected the namespace absent afterwards, got {gone:?}"
    );
    assert_eq!(
        gone.message(),
        NAMESPACE_NOT_FOUND,
        "case \"an allowed delete\": expected AppSync's absent-namespace sentence, got \
         {gone:?}"
    );
}
