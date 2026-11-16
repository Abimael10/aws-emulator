// SPDX-License-Identifier: CC0-1.0

//! The account's DEFAULT AUTHORIZER, end to end over HTTP: electing one of its custom
//! authorizers, reading the election back, clearing it, and what the election blocks.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see: the status, the `x-amzn-errortype` header it resolves the error shape
//! from, the `message` it surfaces, and the response members.
//!
//! The rule under test: an account holds ONE default authorizer at a time, recorded as an
//! attachment to one authorizer record rather than as a value of its own. So
//!
//! * `SetDefaultAuthorizer` against an authorizer the account HOLDS answers 200 with that
//!   authorizer's name and the `authorizerArn` its create minted, whatever members the
//!   create stated and whichever status the record carries;
//! * `DescribeDefaultAuthorizer` reports the elected record's own
//!   `AuthorizerDescription` — the same one `DescribeAuthorizer` reports, including every
//!   later `UpdateAuthorizer` of it — and an account that has elected none is refused
//!   `ResourceNotFoundException` (404);
//! * electing the authorizer that ALREADY holds the election is a duplicate and is
//!   refused `ResourceAlreadyExistsException` (409), naming that authorizer and its ARN,
//!   while electing a DIFFERENT one replaces the election in force, because the default is
//!   single-valued;
//! * a name the account holds no authorizer under elects nothing and is refused
//!   `ResourceNotFoundException` (404), worded as the authorizer family words a miss;
//! * `ClearDefaultAuthorizer` removes the election and leaves the authorizer, answering an
//!   empty 200, and is refused `ResourceNotFoundException` (404) while the account has
//!   elected none;
//! * `DeleteAuthorizer` of the authorizer holding the election is refused
//!   `DeleteConflictException` (409) whatever status that record carries, and succeeds once
//!   the default has changed and the record is out of service.
//!
//! The table spans what the rule generalises over: the form and length of the
//! `authorizerName` within its modelled `@pattern` `^[\w=,@-]+$` and `@length 1..128`,
//! which members the create states, which status the elected record carries (the whole
//! `AuthorizerStatus` enum, from `smithy_query(query="shape",shape="AuthorizerStatus")`),
//! how many authorizers the account holds, and whether the election is freed by clearing
//! it or by electing another authorizer.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    DEFAULT_AUTHORIZER_STATUS, INACTIVE_AUTHORIZER_STATUS, MISSING_DEFAULT_AUTHORIZER_MESSAGE,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region every request in this suite is signed for.
const REGION: &str = "ap-southeast-2";

/// The wire error shape of a resource this service does not hold.
const NOT_FOUND: &str = "ResourceNotFoundException";
/// The wire error shape of a resource that already exists.
const ALREADY_EXISTS: &str = "ResourceAlreadyExistsException";
/// The wire error shape of a delete held back by an attachment.
const DELETE_CONFLICT: &str = "DeleteConflictException";
/// The wire error shape of a request this service will not act on.
const INVALID_REQUEST: &str = "InvalidRequestException";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// the region: the scope is how the router picks the claiming service and how the service
/// scopes the ARNs it mints. The signature itself is never checked here — the harness turns
/// request authentication off, exactly as an unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The `x-amzn-errortype` header, verbatim — including any namespace this service
    /// qualifies a shape name with.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<&str> {
        self.body.get(name).and_then(Value::as_str)
    }

    /// The `message` the body carries, or `""` when it carries none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The `authorizerDescription` a reader of one authorizer reports.
    fn description(&self) -> Option<&Value> {
        self.body.get("authorizerDescription")
    }

    /// A member of the reported `AuthorizerDescription`.
    fn member(&self, name: &str) -> Option<&Value> {
        self.description().and_then(|value| value.get(name))
    }

    /// A string member of the reported `AuthorizerDescription`.
    fn member_str(&self, name: &str) -> Option<&str> {
        self.member(name).and_then(Value::as_str)
    }

    /// Whether the reported error shape is `shape` — the header carries the shape name,
    /// optionally qualified with the service's API doc namespace.
    fn is_error_shape(&self, shape: &str) -> bool {
        self.error_type
            .as_deref()
            .is_some_and(|reported| reported == shape || reported.starts_with(&format!("{shape}:")))
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_type,
            body,
        }
    }

    async fn create(&self, name: &str, body: &Value) -> Reply {
        self.call("POST", &format!("/authorizer/{name}"), Some(body))
            .await
    }

    async fn describe(&self, name: &str) -> Reply {
        self.call("GET", &format!("/authorizer/{name}"), None).await
    }

    async fn update(&self, name: &str, body: &Value) -> Reply {
        self.call("PUT", &format!("/authorizer/{name}"), Some(body))
            .await
    }

    async fn delete(&self, name: &str) -> Reply {
        self.call("DELETE", &format!("/authorizer/{name}"), None)
            .await
    }

    async fn set_default(&self, name: &str) -> Reply {
        self.call(
            "POST",
            "/default-authorizer",
            Some(&json!({ "authorizerName": name })),
        )
        .await
    }

    async fn describe_default(&self) -> Reply {
        self.call("GET", "/default-authorizer", None).await
    }

    async fn clear_default(&self) -> Reply {
        self.call("DELETE", "/default-authorizer", None).await
    }

    /// Create an authorizer and assert the create succeeded, so a case's own assertions
    /// are about the election and not about its setup.
    async fn created(&self, case: &str, name: &str, body: &Value) {
        let created = self.create(name, body).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the setup CreateAuthorizer for {name:?} must succeed, got \
             {created:?}"
        );
    }
}

/// The ARN an authorizer created in this suite's region must carry.
fn expected_arn(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:authorizer/{name}")
}

/// A Lambda function ARN for an authorizer to call, varied per case only so no two records
/// share a value.
fn function_arn(suffix: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:authorize-{suffix}")
}

/// How this service words the refusal of an election of the authorizer that already holds
/// the account's default.
fn duplicate_default(name: &str) -> String {
    format!("Duplicate default authorizer {name} already configured for this account")
}

/// How this service words the refusal of a delete of the authorizer holding the election.
fn undeletable_default(name: &str) -> String {
    format!("Cannot delete default authorizer {name}. Change default and retry delete.")
}

/// One row of the election table: a case name, the authorizer name, the create body and
/// the status that create resolves to.
struct Row {
    case: &'static str,
    name: String,
    create: Value,
    created_status: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a create that states nothing but the function to call",
            name: "default_authz".to_string(),
            create: json!({ "authorizerFunctionArn": function_arn("plain") }),
            created_status: DEFAULT_AUTHORIZER_STATUS,
        },
        Row {
            case: "an explicitly ACTIVE authorizer that validates a token header",
            name: "Election_Authz=7,@host-1".to_string(),
            create: json!({
                "authorizerFunctionArn": function_arn("token"),
                "status": "ACTIVE",
                "tokenKeyName": "X-Amz-Custom-Token",
                "tokenSigningPublicKeys": {"FIRST_KEY": "-----BEGIN PUBLIC KEY-----\nAAAA"},
            }),
            created_status: "ACTIVE",
        },
        Row {
            case: "an authorizer created out of service, which the election does not read",
            name: "elected-while-inactive".to_string(),
            create: json!({
                "authorizerFunctionArn": function_arn("inactive"),
                "status": INACTIVE_AUTHORIZER_STATUS,
                "signingDisabled": true,
            }),
            created_status: INACTIVE_AUTHORIZER_STATUS,
        },
        Row {
            case: "the shortest legal authorizer name",
            name: "a".to_string(),
            create: json!({
                "authorizerFunctionArn": function_arn("short"),
                "enableCachingForHttp": true,
            }),
            created_status: DEFAULT_AUTHORIZER_STATUS,
        },
        Row {
            case: "the longest legal authorizer name",
            name: "z".repeat(128),
            create: json!({ "authorizerFunctionArn": function_arn("long") }),
            created_status: DEFAULT_AUTHORIZER_STATUS,
        },
    ]
}

#[tokio::test]
async fn an_elected_authorizer_is_reported_as_the_account_default_and_then_cleared() {
    for Row {
        case,
        name,
        create,
        created_status,
    } in rows()
    {
        // A fresh account per row: the default is single-valued, so each row starts with
        // none elected.
        let emulator = Emulator::new();
        emulator.created(case, &name, &create).await;
        let arn = expected_arn(&name);

        // Before any election the account has no default authorizer, and the reader says
        // so rather than describing nothing.
        let unset = emulator.describe_default().await;
        assert_eq!(
            unset.status, 404,
            "case {case:?}: an account that has elected no default must be refused 404, got \
             {unset:?}"
        );
        assert!(
            unset.is_error_shape(NOT_FOUND),
            "case {case:?}: expected {NOT_FOUND}, got {unset:?}"
        );
        assert_eq!(
            unset.message(),
            MISSING_DEFAULT_AUTHORIZER_MESSAGE,
            "case {case:?}: expected the unset sentence, got {unset:?}"
        );

        // The election answers the elected record's own identity.
        let elected = emulator.set_default(&name).await;
        assert_eq!(
            elected.status, 200,
            "case {case:?}: electing an authorizer the account holds must answer 200, got \
             {elected:?}"
        );
        assert_eq!(
            elected.text("authorizerName"),
            Some(name.as_str()),
            "case {case:?}: expected the election to report the authorizer's name, got \
             {elected:?}"
        );
        assert_eq!(
            elected.text("authorizerArn"),
            Some(arn.as_str()),
            "case {case:?}: expected the election to report the minted ARN {arn}, got \
             {elected:?}"
        );

        // The default's description is the elected record's own, member for member: the
        // election resolves the record rather than copying it.
        let read_default = emulator.describe_default().await;
        let read_record = emulator.describe(&name).await;
        assert_eq!(
            read_default.status, 200,
            "case {case:?}: the elected default must be readable, got {read_default:?}"
        );
        assert_eq!(
            read_default.description(),
            read_record.description(),
            "case {case:?}: expected the default to describe the elected record, got \
             {read_default:?} against {read_record:?}"
        );
        assert_eq!(
            read_default.member_str("authorizerName"),
            Some(name.as_str()),
            "case {case:?}: expected the default to be {name:?}, got {read_default:?}"
        );
        assert_eq!(
            read_default.member_str("status"),
            Some(created_status),
            "case {case:?}: expected the elected record's status {created_status:?}, got \
             {read_default:?}"
        );

        // Clearing removes the ELECTION and leaves the authorizer in the store.
        let cleared = emulator.clear_default().await;
        assert_eq!(
            cleared.status, 200,
            "case {case:?}: clearing an elected default must answer 200, got {cleared:?}"
        );
        assert_eq!(
            cleared.body,
            json!({}),
            "case {case:?}: ClearDefaultAuthorizer models no output members, got {cleared:?}"
        );
        let after_clear = emulator.describe_default().await;
        assert_eq!(
            after_clear.status, 404,
            "case {case:?}: no default must be reported after the clear, got {after_clear:?}"
        );
        let still_held = emulator.describe(&name).await;
        assert_eq!(
            still_held.description(),
            read_record.description(),
            "case {case:?}: the clear must leave the authorizer untouched, got {still_held:?}"
        );
    }
}

#[tokio::test]
async fn electing_the_authorizer_that_already_holds_the_default_is_a_duplicate() {
    // Two authorizers per case, so a repeat election and a change of election are both
    // reachable: the names vary along the modelled pattern and length.
    for (case, first, second) in [
        ("plain names", "first_authz".to_string(), "second_authz".to_string()),
        (
            "names at the pattern's edges",
            "First=Authz,1@host".to_string(),
            "s".repeat(128),
        ),
    ] {
        let emulator = Emulator::new();
        emulator
            .created(case, &first, &json!({ "authorizerFunctionArn": function_arn("first") }))
            .await;
        emulator
            .created(case, &second, &json!({ "authorizerFunctionArn": function_arn("second") }))
            .await;

        let elected = emulator.set_default(&first).await;
        assert_eq!(
            elected.status, 200,
            "case {case:?}: the first election must answer 200, got {elected:?}"
        );

        // The same authorizer again is the election the account already has: a duplicate,
        // refused 409 and naming the record that holds it.
        let duplicate = emulator.set_default(&first).await;
        assert_eq!(
            duplicate.status, 409,
            "case {case:?}: re-electing the authorizer that holds the default must be refused \
             409, got {duplicate:?}"
        );
        assert!(
            duplicate.is_error_shape(ALREADY_EXISTS),
            "case {case:?}: expected {ALREADY_EXISTS}, got {duplicate:?}"
        );
        assert_eq!(
            duplicate.message(),
            duplicate_default(&first),
            "case {case:?}: expected the duplicate-election sentence, got {duplicate:?}"
        );
        assert_eq!(
            duplicate.text("resourceId"),
            Some(first.as_str()),
            "case {case:?}: expected the refusal to name the elected authorizer, got \
             {duplicate:?}"
        );
        assert_eq!(
            duplicate.text("resourceArn"),
            Some(expected_arn(&first).as_str()),
            "case {case:?}: expected the refusal to carry the elected ARN, got {duplicate:?}"
        );
        let unchanged = emulator.describe_default().await;
        assert_eq!(
            unchanged.member_str("authorizerName"),
            Some(first.as_str()),
            "case {case:?}: the refused duplicate must leave the election in force, got \
             {unchanged:?}"
        );

        // A DIFFERENT authorizer is a change of election, not a duplicate: the account's
        // default is single-valued, so the new election replaces the one in force.
        let changed = emulator.set_default(&second).await;
        assert_eq!(
            changed.status, 200,
            "case {case:?}: electing another authorizer must answer 200, got {changed:?}"
        );
        assert_eq!(
            changed.text("authorizerArn"),
            Some(expected_arn(&second).as_str()),
            "case {case:?}: expected the new election's ARN, got {changed:?}"
        );
        let moved = emulator.describe_default().await;
        assert_eq!(
            moved.member_str("authorizerName"),
            Some(second.as_str()),
            "case {case:?}: expected the default to be {second:?}, got {moved:?}"
        );

        // And the duplicate is now the SECOND authorizer's, because the refusal is about
        // the election in force and not about the first name elected.
        let duplicate_again = emulator.set_default(&second).await;
        assert_eq!(
            duplicate_again.message(),
            duplicate_default(&second),
            "case {case:?}: expected the duplicate to name the authorizer in force, got \
             {duplicate_again:?}"
        );
        let re_elected = emulator.set_default(&first).await;
        assert_eq!(
            re_elected.status, 200,
            "case {case:?}: the authorizer the election moved off must be electable again, got \
             {re_elected:?}"
        );
    }
}

#[tokio::test]
async fn a_name_the_account_holds_no_authorizer_under_elects_nothing() {
    let emulator = Emulator::new();

    // One authorizer exists and holds the election, so the refusals below are about the
    // NAME rather than about an empty store.
    let held = "held_authz";
    emulator
        .created("setup", held, &json!({ "authorizerFunctionArn": function_arn("held") }))
        .await;
    let elected = emulator.set_default(held).await;
    assert_eq!(
        elected.status, 200,
        "the setup election must succeed, got {elected:?}"
    );

    // The authorizer family words a miss `Authorizer <name> not found`, with the name the
    // request addressed as its only variable part.
    for name in [
        "a",
        "held_authz_2",
        "HELD_AUTHZ",
        "Missing=Authz,1@host-2",
        &"m".repeat(128),
    ] {
        let refused = emulator.set_default(name).await;
        assert_eq!(
            refused.status, 404,
            "case {name:?}: electing a name the account holds nothing under must be refused \
             404, got {refused:?}"
        );
        assert!(
            refused.is_error_shape(NOT_FOUND),
            "case {name:?}: expected {NOT_FOUND}, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            format!("Authorizer {name} not found"),
            "case {name:?}: expected the authorizer family's sentence, got {refused:?}"
        );
        let unchanged = emulator.describe_default().await;
        assert_eq!(
            unchanged.member_str("authorizerName"),
            Some(held),
            "case {name:?}: the refused election must leave the one in force, got {unchanged:?}"
        );
    }
}

#[tokio::test]
async fn clearing_is_refused_while_the_account_has_elected_no_default() {
    let emulator = Emulator::new();

    // Nothing elected yet: the clear addresses the election, which does not exist.
    let empty = emulator.clear_default().await;
    assert_eq!(
        empty.status, 404,
        "clearing an account that has elected no default must be refused 404, got {empty:?}"
    );
    assert!(
        empty.is_error_shape(NOT_FOUND),
        "expected {NOT_FOUND}, got {empty:?}"
    );
    assert_eq!(
        empty.message(),
        MISSING_DEFAULT_AUTHORIZER_MESSAGE,
        "expected the unset sentence, got {empty:?}"
    );

    // An authorizer the account holds, but has not elected, does not make the clear
    // admissible either.
    let name = "clearable_authz";
    emulator
        .created("setup", name, &json!({ "authorizerFunctionArn": function_arn("clear") }))
        .await;
    let still_empty = emulator.clear_default().await;
    assert_eq!(
        still_empty.status, 404,
        "an authorizer nobody elected is no election, got {still_empty:?}"
    );

    // Elected, cleared, and refused again: the clear is admissible exactly once per
    // election.
    let elected = emulator.set_default(name).await;
    assert_eq!(
        elected.status, 200,
        "the election must succeed, got {elected:?}"
    );
    let cleared = emulator.clear_default().await;
    assert_eq!(
        cleared.status, 200,
        "clearing an elected default must answer 200, got {cleared:?}"
    );
    let twice = emulator.clear_default().await;
    assert_eq!(
        twice.status, 404,
        "a second clear has no election to remove, got {twice:?}"
    );
    assert_eq!(
        twice.message(),
        MISSING_DEFAULT_AUTHORIZER_MESSAGE,
        "expected the unset sentence, got {twice:?}"
    );
}

#[tokio::test]
async fn the_elected_authorizer_is_deletable_only_once_the_default_has_changed() {
    /// How a case frees the election it holds.
    enum Free {
        /// `ClearDefaultAuthorizer`.
        Clear,
        /// Electing the other authorizer the account holds.
        Elect(&'static str),
    }

    let cases = vec![
        (
            "an in-service default freed by clearing it",
            "delete_active",
            DEFAULT_AUTHORIZER_STATUS,
            Free::Clear,
        ),
        (
            "an out-of-service default freed by clearing it",
            "delete_inactive",
            INACTIVE_AUTHORIZER_STATUS,
            Free::Clear,
        ),
        (
            "a default freed by electing another authorizer",
            "delete_replaced",
            DEFAULT_AUTHORIZER_STATUS,
            Free::Elect("successor_authz"),
        ),
    ];

    for (case, name, status, free) in cases {
        let emulator = Emulator::new();
        emulator
            .created(
                case,
                name,
                &json!({
                    "authorizerFunctionArn": function_arn(name),
                    "status": status,
                }),
            )
            .await;
        if let Free::Elect(successor) = &free {
            emulator
                .created(
                    case,
                    successor,
                    &json!({ "authorizerFunctionArn": function_arn("successor") }),
                )
                .await;
        }
        let elected = emulator.set_default(name).await;
        assert_eq!(
            elected.status, 200,
            "case {case:?}: the election must succeed, got {elected:?}"
        );

        // The elected record is attached to the account, so its delete is refused
        // whatever status it carries — the attachment, not the status, is what blocks it.
        let before = emulator.describe(name).await;
        let refused = emulator.delete(name).await;
        assert_eq!(
            refused.status, 409,
            "case {case:?}: deleting the elected default must be refused 409, got {refused:?}"
        );
        assert!(
            refused.is_error_shape(DELETE_CONFLICT),
            "case {case:?}: expected {DELETE_CONFLICT}, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            undeletable_default(name),
            "case {case:?}: expected the election's refusal sentence, got {refused:?}"
        );
        let after = emulator.describe(name).await;
        assert_eq!(
            after.body, before.body,
            "case {case:?}: the refused delete must leave the authorizer untouched, got \
             {after:?}"
        );

        // Freed of the election, the record is deletable on the authorizer family's own
        // precondition: out of service first, then gone.
        match &free {
            Free::Clear => {
                let cleared = emulator.clear_default().await;
                assert_eq!(
                    cleared.status, 200,
                    "case {case:?}: the clear must succeed, got {cleared:?}"
                );
            }
            Free::Elect(successor) => {
                let changed = emulator.set_default(successor).await;
                assert_eq!(
                    changed.status, 200,
                    "case {case:?}: electing {successor:?} must succeed, got {changed:?}"
                );
            }
        }

        if status != INACTIVE_AUTHORIZER_STATUS {
            let in_service = emulator.delete(name).await;
            assert_eq!(
                in_service.status, 400,
                "case {case:?}: an authorizer in {status} status must still refuse its delete, \
                 got {in_service:?}"
            );
            assert!(
                in_service.is_error_shape(INVALID_REQUEST),
                "case {case:?}: expected {INVALID_REQUEST}, got {in_service:?}"
            );
            let deactivated = emulator
                .update(name, &json!({ "status": INACTIVE_AUTHORIZER_STATUS }))
                .await;
            assert_eq!(
                deactivated.status, 200,
                "case {case:?}: deactivating the authorizer must succeed, got {deactivated:?}"
            );
        }

        let deleted = emulator.delete(name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete must succeed once the default has changed and the \
             record is out of service, got {deleted:?}"
        );
        let gone = emulator.describe(name).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: the deleted authorizer must no longer be readable, got {gone:?}"
        );
    }
}

#[tokio::test]
async fn the_reported_default_follows_every_update_of_the_elected_authorizer() {
    let emulator = Emulator::new();
    let name = "tracked_authz";
    emulator
        .created(
            "setup",
            name,
            &json!({
                "authorizerFunctionArn": function_arn("tracked"),
                "tokenKeyName": "token",
            }),
        )
        .await;
    let elected = emulator.set_default(name).await;
    assert_eq!(
        elected.status, 200,
        "the election must succeed, got {elected:?}"
    );

    // Each update changes the elected record; the reader of the default reports the record
    // as it stands, because the election is a reference to it and not a copy of it.
    for (case, update) in [
        (
            "a new function to call",
            json!({ "authorizerFunctionArn": function_arn("tracked-replacement") }),
        ),
        (
            "a new token key and caching",
            json!({ "tokenKeyName": "other-token", "enableCachingForHttp": true }),
        ),
        (
            "the status moved out of service",
            json!({ "status": INACTIVE_AUTHORIZER_STATUS }),
        ),
    ] {
        let updated = emulator.update(name, &update).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: UpdateAuthorizer must succeed, got {updated:?}"
        );

        let read_default = emulator.describe_default().await;
        let read_record = emulator.describe(name).await;
        assert_eq!(
            read_default.description(),
            read_record.description(),
            "case {case:?}: expected the default to describe the elected record, got \
             {read_default:?} against {read_record:?}"
        );
        for (member, want) in update.as_object().expect("the update body is an object") {
            assert_eq!(
                read_default.member(member),
                Some(want),
                "case {case:?}: expected the default's {member} to read back as {want}, got \
                 {read_default:?}"
            );
        }
    }
}
