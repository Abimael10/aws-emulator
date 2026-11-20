// SPDX-License-Identifier: CC0-1.0

//! What the three `@connections` operations answer a caller IAM does not allow to manage the
//! connections of the stage it addressed — driven over HTTP through the same
//! `MultiServiceRouter` the production emulator serves with.
//!
//! > **RULE: these operations are published on the owning api's `execute-api` invoke
//! > endpoint, and every call made there is authorized by IAM against the calling
//! > principal's own identity-based policies: a call is served only where a readable document
//! > allows `execute-api:ManageConnections` on the resource it addresses —
//! > `arn:aws:execute-api:{region}:{account}:{api-id}/{stage}/{method}/@connections/{connectionId}`,
//! > the route within the stage — and every other call is refused `403` with the error code
//! > `AccessDeniedException` and the sentence IAM words an authorization failure in — whose
//! > resource is reported with the account masked down to its last four digits — taken BEFORE
//! > the connection store is consulted. The `ConnectionId`'s own SHAPE is judged ahead of
//! > that decision: a value that could never have been minted is a malformed request, which
//! > the endpoint refuses `400` without asking IAM anything.**
//!
//! WHO the api and the stage are comes from the service that owns them, through the shared
//! cell this service asks (`mgmt::state::connections_endpoint_authority`) — the same cell
//! that judges whether the addressing serves anything at all. These cases install a
//! resolution of their own, exactly as `connections_error_layer` installs a verdict of its
//! own: what this suite owns is the DECISION and its envelope, while the resolution against
//! real apis, stages and deployments is API Gateway V2's and is driven end to end in
//! `core/router/tests/all/apigatewayv2_websocket_deploy_then_connections.rs`.
//!
//! The role, its policies and the session live in the IAM and STS stores of the SAME shared
//! engine — the stores those services' own handlers write — so the documents under test are
//! the ones `iam:PutRolePolicy` stores and the principal a refusal blames is the one
//! `sts:GetCallerIdentity` answers for the key the request signed with.
//!
//! The rows span what the rule generalises over: the operation (all three, so all three
//! methods of the resource ARN), the document in force on the caller (none at all, the action
//! on every resource, on this very route, on the stage's connections by wildcard, on another
//! stage of the same api, another action of the same service, and an explicit deny beside a
//! grant of everything), the PRINCIPAL the credential speaks for (an assumed-role session and
//! the account's own published credential), and the `ConnectionId` the refused call carries (a
//! live one and a well-formed one nothing holds, plus one that could never have been minted,
//! which is refused as the malformed request it is).
//!
//! The REPORTED resource of a refusal is driven over its own rows, which vary the api, the
//! Region, the stage and the method the refused call addressed: the account is masked down to
//! its last four digits whatever addressing resolved the resource, and the whole account id
//! is in none of those sentences.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use engine::InMemoryEngine;
use serde_json::{json, Value};
use service_common::execute_api_connections_endpoint::AddressedStage;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

use crate::support::{call_signed_by, error_code, error_message, rendered, HOST, MINTED_IDS};

/// The api, Region and stage the installed resolution answers for — what a real resolution
/// reads out of the owner's own records.
const API_ID: &str = "k1l2m3n4o5";
const REGION: &str = "us-east-1";
const STAGE: &str = "dev";

/// The invoke path a caller of that stage addresses, which is what names it.
const STAGE_PREFIX: &str = "/dev";

/// The role the seeded session is a session of, and the session's own name.
const ROLE: &str = "connections-pusher";
const SESSION: &str = "pushing";

/// The access key id that session was minted under: an `ASIA` key, the prefix a temporary
/// credential carries.
const SESSION_KEY_ID: &str = "ASIAEXAMPLECONNECT01";

/// An access key id neither store holds a record for: the position the account's own
/// published credential is in, which holds no IAM policies at all.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The name of the inline policy a row attaches to the role.
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The action a `@connections` call asks for.
const MANAGE_CONNECTIONS: &str = "execute-api:ManageConnections";

/// The error code AWS words an authorization failure in.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The three operations, by the method each is reached with, the payload each sends, and the
/// status each answers a call it serves.
const OPERATIONS: [(&str, &[u8], u16); 3] =
    [("GET", b"", 200), ("POST", b"push", 200), ("DELETE", b"", 204)];

/// A registration instant in the past.
fn connected_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// The resolution standing in for the owner of apis and stages: one deployed stage, whose api
/// and Region are the ones every expected ARN below names.
fn the_deployed_stage(
    _engine: &dyn Any,
    _host: Option<&str>,
    prefix: &str,
) -> Option<AddressedStage> {
    (prefix == STAGE_PREFIX).then(|| AddressedStage {
        api_id: API_ID.to_string(),
        region: REGION.to_string(),
        stage_name: STAGE.to_string(),
    })
}

/// An engine whose shared cell carries [`the_deployed_stage`] and whose IAM and STS stores
/// hold the role and the session [`SESSION_KEY_ID`] speaks for.
fn engine_with_a_session() -> Arc<InMemoryEngine> {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    mgmt::state::connections_endpoint_authority(&*engine)
        .expect("the shared cell lives in this engine")
        .lock()
        .install_addressed_stage(the_deployed_stage);
    given_session(&engine);
    engine
}

/// The ARN the account holds the seeded role under.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", engine::emulator_account_id())
}

/// The principal the seeded session speaks for, as `sts:GetCallerIdentity` reports it — what
/// a refusal of a request signed with it must name.
fn session_arn() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{ROLE}/{SESSION}",
        engine::emulator_account_id()
    )
}

/// Record the role and the assumed-role session [`SESSION_KEY_ID`] speaks for, exactly as
/// `iam:CreateRole` and a vend of that role record them.
fn given_session(engine: &Arc<InMemoryEngine>) {
    let role_arn = role_arn();
    let iam_store = iam::service_state(&**engine).expect("the IAM store lives in this engine");
    iam_store.lock().roles.insert(
        iam::name_key(ROLE),
        service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: ROLE.to_string(),
            role_id: sts::role_id(&role_arn),
            arn: role_arn.clone(),
            create_date: aws_smithy_types::DateTime::from_secs(0),
            // The role's TRUST policy, which is what a vend of its credentials is authorized
            // by; nothing here is an identity-based grant.
            assume_role_policy_document: Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "lambda.amazonaws.com" },
                        "Action": "sts:AssumeRole"
                    }]
                })
                .to_string(),
            ),
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    let identity = sts::Identity {
        user_id: format!("{}:{SESSION}", sts::role_id(&role_arn)),
        account: engine::emulator_account_id().to_string(),
        arn: session_arn(),
    };
    // The vend as STS asked for it, through the registry's own constructor, so what is
    // recorded is exactly what a mint records. No session policy scopes it: what the
    // credential may do is the role's own policies, which is what each row puts on it.
    let vend = sts::NewSession {
        role_arn: Some(role_arn),
        ..sts::NewSession::new(identity, sts::SessionKind::AssumedRole, SESSION_SECONDS)
    };
    let sessions = sts::service_state(&**engine).expect("the STS store lives in this engine");
    sessions
        .lock()
        .insert(vend.into_session(SESSION_KEY_ID, SystemTime::now()))
        .expect("the vended session is recorded");
}

/// Attach `document` to the role as its one inline policy, replacing whatever it carried —
/// what `iam:PutRolePolicy` does. `None` takes every policy off it.
fn given_policy(engine: &Arc<InMemoryEngine>, document: Option<&Value>) {
    let store = iam::service_state(&**engine).expect("the IAM store lives in this engine");
    let mut store = store.lock();
    let key = iam::name_key(ROLE);
    match document {
        None => {
            store.role_inline_policies.remove(&key);
        }
        Some(document) => {
            store.role_inline_policies.insert(
                key,
                BTreeMap::from([(
                    iam::name_key(POLICY_NAME),
                    iam::InlinePolicy {
                        policy_name: POLICY_NAME.to_string(),
                        policy_document: document.to_string(),
                    },
                )]),
            );
        }
    }
}

/// An identity-based policy with one statement.
fn statement(effect: &str, action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }]
    })
}

/// The resource ARN a call reached by `method` addresses: the route within the stage, whose
/// connection id is the route's own parameter.
fn resource(method: &str) -> String {
    format!(
        "arn:aws:execute-api:{REGION}:{}:{API_ID}/{STAGE}/{method}/@connections/{{connectionId}}",
        engine::emulator_account_id()
    )
}

/// The resource a REFUSAL of that call NAMES: the same ARN with the account masked down to
/// its last four digits, which is how an `execute-api` endpoint reports a resource in the
/// sentence it refuses an unauthorized call with — a caller it just refused is not told the
/// whole account id. Spelled out here rather than taken from the code under test.
fn reported_resource(method: &str) -> String {
    let account = engine::emulator_account_id();
    let last_four = account
        .get(account.len().saturating_sub(4)..)
        .expect("the account id's last four digits");
    format!(
        "arn:aws:execute-api:{REGION}:********{last_four}:{API_ID}/{STAGE}/{method}/@connections/\
         {{connectionId}}"
    )
}

/// The sentence a refused call must carry: the principal, the action, the resource as the
/// endpoint reports it and the clause IAM closes an authorization failure with.
fn refusal_sentence(method: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {MANAGE_CONNECTIONS} on resource: \
         {resource} {reason}",
        principal = session_arn(),
        resource = reported_resource(method),
    )
}

/// The clause closing a refusal no readable document allowed.
fn no_grant() -> String {
    format!("because no identity-based policy allows the {MANAGE_CONNECTIONS} action")
}

/// The clause closing a refusal a readable document DENIED — a fact about that document, so
/// the sentence names it rather than the action.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The request path for `connection_id` on the deployed stage.
fn path(connection_id: &str) -> String {
    format!("{STAGE_PREFIX}/@connections/{connection_id}")
}

/// One row of the table: the document in force on the caller, and whether the call is served.
struct Row {
    case: &'static str,
    /// The document, built per row from the METHOD the row's call is reached by (so a grant
    /// scoped to this very route is the route of that call).
    document: fn(&str) -> Option<Value>,
    served: bool,
    /// The clause the refusal of a row this document does not admit closes with: the default
    /// deny of a call nothing allowed, or the explicit deny of a document that refused it.
    reason: Option<&'static str>,
}

/// The documents span the readings of `Action` and `Resource` a grant can take, plus the
/// absence of any document and an explicit deny beside a grant of everything.
const ROWS: &[Row] = &[
    Row {
        case: "no document at all",
        document: |_method| None,
        served: false,
        reason: None,
    },
    Row {
        case: "the action on every resource",
        document: |_method| Some(statement("Allow", MANAGE_CONNECTIONS, "*")),
        served: true,
        reason: None,
    },
    Row {
        case: "the action on this very route",
        document: |method| Some(statement("Allow", MANAGE_CONNECTIONS, &resource(method))),
        served: true,
        reason: None,
    },
    Row {
        case: "the stage's connections by wildcard",
        document: |method| {
            Some(statement(
                "Allow",
                MANAGE_CONNECTIONS,
                &format!(
                    "arn:aws:execute-api:{REGION}:{}:{API_ID}/{STAGE}/{method}/@connections/*",
                    engine::emulator_account_id()
                ),
            ))
        },
        served: true,
        reason: None,
    },
    Row {
        case: "the action on another stage of the same api",
        document: |method| {
            Some(statement(
                "Allow",
                MANAGE_CONNECTIONS,
                &format!(
                    "arn:aws:execute-api:{REGION}:{}:{API_ID}/other/{method}/@connections/*",
                    engine::emulator_account_id()
                ),
            ))
        },
        served: false,
        reason: None,
    },
    Row {
        case: "another action of the same service on every resource",
        document: |_method| Some(statement("Allow", "execute-api:Invoke", "*")),
        served: false,
        reason: None,
    },
    Row {
        case: "an explicit deny beside a grant of everything",
        document: |_method| {
            Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": MANAGE_CONNECTIONS, "Resource": "*" },
                    { "Effect": "Deny", "Action": MANAGE_CONNECTIONS, "Resource": "*" }
                ]
            }))
        },
        served: false,
        reason: Some(EXPLICIT_DENY),
    },
];

/// Each operation is served only where the caller's own policies allow the action on the
/// route it addresses, and refused `403 AccessDeniedException` with IAM's sentence otherwise.
#[tokio::test]
async fn each_operation_is_served_only_where_the_caller_may_manage_connections() {
    for (method, payload, served_status) in OPERATIONS {
        for row in ROWS {
            let engine = engine_with_a_session();
            let document = (row.document)(method);
            given_policy(&engine, document.as_ref());
            let live = MINTED_IDS[0].1;
            crate::support::register(&engine, live, "192.0.2.20", "pusher/1", connected_at());

            let case = format!("{method} with {}", row.case);
            let (status, headers, body) = call_signed_by(
                &engine,
                SESSION_KEY_ID,
                HOST,
                method,
                &path(live),
                payload.to_vec(),
            )
            .await;
            if row.served {
                assert_eq!(
                    status, served_status,
                    "case {case:?}: expected the operation's own {served_status}, got {status} {}",
                    rendered(&body)
                );
                continue;
            }
            assert_eq!(
                status, 403,
                "case {case:?}: expected HTTP 403, got {status} {}",
                rendered(&body)
            );
            assert_eq!(
                error_code(&headers),
                ACCESS_DENIED,
                "case {case:?}: expected {ACCESS_DENIED} in x-amzn-errortype, got {}",
                error_code(&headers)
            );
            let expected =
                refusal_sentence(method, row.reason.map_or_else(no_grant, str::to_string).as_str());
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?}: expected {expected:?}, got {}",
                rendered(&body)
            );
        }
    }
}

/// The decision precedes the connection store: a refused call touches no connection, whatever
/// `ConnectionId` it carried — a live one or a well-formed one nothing holds. A value that
/// could never have been minted is judged AHEAD of the decision, so the same unprivileged
/// caller is told its request is malformed (`400`) rather than that it may not invoke the
/// endpoint: the id is read off the path, and no record is consulted to refuse it either.
#[tokio::test]
async fn an_unauthorized_call_is_refused_before_the_connection_is_read() {
    let malformed = crate::support::ids_api_gateway_does_not_mint();
    let (malformed_form, malformed_id) = malformed[0].clone();
    // The ids span the three classes a request can carry, with the status and error code the
    // endpoint answers each class in for a caller holding no grant.
    let ids = [
        ("a live connection", MINTED_IDS[0].1.to_string(), 403, ACCESS_DENIED),
        (
            "a well-formed id nothing holds",
            MINTED_IDS[1].1.to_string(),
            403,
            ACCESS_DENIED,
        ),
        (
            "an id that could never have been minted",
            malformed_id,
            400,
            "BadRequestException",
        ),
    ];
    for (method, payload, _) in OPERATIONS {
        for (id_case, connection_id, expected_status, expected_code) in &ids {
            let engine = engine_with_a_session();
            // No document at all: the caller holds no grant.
            given_policy(&engine, None);
            let live = MINTED_IDS[0].1;
            crate::support::register(&engine, live, "192.0.2.21", "pusher/2", connected_at());

            let case = format!("{method} on {id_case}");
            let (status, headers, body) = call_signed_by(
                &engine,
                SESSION_KEY_ID,
                HOST,
                method,
                &path(connection_id),
                payload.to_vec(),
            )
            .await;
            assert_eq!(
                status, *expected_status,
                "case {case:?}: expected HTTP {expected_status}, got {status} {}",
                rendered(&body)
            );
            assert_eq!(
                error_code(&headers),
                *expected_code,
                "case {case:?}: expected {expected_code}, got {}",
                error_code(&headers)
            );
            if *expected_status == 400 {
                // The malformed refusal says what was wrong with the request and names the
                // value, which is the whole reason it is not the authorization refusal.
                let expected = mgmt::state::invalid_connection_id_message(connection_id);
                assert_eq!(
                    error_message(&body),
                    expected,
                    "case {case:?} ({malformed_form}): expected {expected:?}, got {}",
                    rendered(&body)
                );
            }

            // Nothing about the connection changed, which is what "before the store is
            // consulted" means on the wire.
            let stored = mgmt::state::connection(&*engine, live)
                .unwrap_or_else(|| panic!("case {case:?}: the connection must still exist"));
            assert_eq!(
                stored.delivered_count, 0,
                "case {case:?}: a refused call delivers nothing, got {} deliveries",
                stored.delivered_count
            );
            assert_eq!(
                stored.last_active_at, stored.connected_at,
                "case {case:?}: a refused call must not move LastActiveAt, got {:?}",
                stored.last_active_at
            );
        }
    }
}

/// The invoke endpoint host of `api_id` in `region`:
/// `{api-id}.execute-api.{region}.amazonaws.com`, the form a deployed api's own invoke
/// endpoint takes and the host an SDK client addressing `@connections` sends.
fn invoke_host(api_id: &str, region: &str) -> String {
    format!("{api_id}.execute-api.{region}.amazonaws.com")
}

/// A resolution that reads the addressing back the way the owner of apis and stages answers
/// it: the api id and the Region are the invoke endpoint's own labels, and the stage is the
/// invoke path's prefix. Installing it lets a row address ANY api, Region and stage, so the
/// rule below is driven over more than one of each.
fn the_addressed_invoke_url(
    _engine: &dyn Any,
    host: Option<&str>,
    prefix: &str,
) -> Option<AddressedStage> {
    let mut labels = host?.split('.');
    let api_id = labels.next()?;
    if labels.next()? != "execute-api" {
        return None;
    }
    let region = labels.next()?;
    let stage_name = prefix.strip_prefix('/')?;
    (!api_id.is_empty() && !stage_name.is_empty()).then(|| AddressedStage {
        api_id: api_id.to_string(),
        region: region.to_string(),
        stage_name: stage_name.to_string(),
    })
}

/// An engine resolving every invoke URL through [`the_addressed_invoke_url`], holding the
/// seeded session with NO document at all in force on it.
fn engine_addressing_any_invoke_url() -> Arc<InMemoryEngine> {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    mgmt::state::connections_endpoint_authority(&*engine)
        .expect("the shared cell lives in this engine")
        .lock()
        .install_addressed_stage(the_addressed_invoke_url);
    given_session(&engine);
    given_policy(&engine, None);
    engine
}

/// The mask an `execute-api` refusal writes in front of the account digits it leaves: a
/// 12-digit account id keeps its LAST FOUR, so the eight before them are replaced. Spelled
/// out here rather than taken from the code under test.
const MASKED_ACCOUNT_PREFIX: &str = "********";

/// How many trailing digits of the account a refusal reports unmasked.
const UNMASKED_ACCOUNT_DIGITS: usize = 4;

/// The resource a refusal names, read out of the sentence where the clause carrying it ends.
fn resource_clause_of(message: &str) -> Option<&str> {
    let (_, after) = message.split_once("on resource: ")?;
    let (resource, _) = after.split_once(" because")?;
    Some(resource)
}

/// One ADDRESSING a refusal is read back from: the api the endpoint serves, the Region that
/// api was created in, the stage the invoke path names, the method the operation is reached
/// by, the payload the call sends and the `ConnectionId` it carries.
struct Addressing {
    case: &'static str,
    api_id: &'static str,
    region: &'static str,
    stage: &'static str,
    method: &'static str,
    payload: &'static [u8],
    connection_id: &'static str,
}

/// The addressings span what the rule generalises over: the api the endpoint serves, the
/// Region it was created in, the stage the invoke path names (including a one-character one),
/// the method the operation is reached by, and the `ConnectionId` the refused call carried — a
/// live connection, a well-formed id nothing holds, and tokens of other character classes,
/// none of which may appear in the resource the refusal reports.
const ADDRESSINGS: &[Addressing] = &[
    Addressing {
        case: "POST on a live connection",
        api_id: "mp67t4dbd4",
        region: "us-east-1",
        stage: "test",
        method: "POST",
        payload: b"denied",
        connection_id: MINTED_IDS[0].1,
    },
    Addressing {
        case: "GET on a well-formed id nothing holds",
        api_id: "a1b2c3d4e5",
        region: "us-west-2",
        stage: "dev",
        method: "GET",
        payload: b"",
        connection_id: MINTED_IDS[1].1,
    },
    Addressing {
        case: "DELETE on a live connection of another Region",
        api_id: "zqx9plm3rk",
        region: "eu-west-1",
        stage: "prod",
        method: "DELETE",
        payload: b"",
        connection_id: MINTED_IDS[0].1,
    },
    Addressing {
        case: "POST on a one-character stage",
        api_id: "q9",
        region: "ap-southeast-2",
        stage: "v",
        method: "POST",
        payload: b"push",
        connection_id: MINTED_IDS[2].1,
    },
    Addressing {
        case: "GET on a token of digits and letters",
        api_id: "0123456789",
        region: "ca-central-1",
        stage: "staging",
        method: "GET",
        payload: b"",
        connection_id: MINTED_IDS[3].1,
    },
];

/// The refusal reports the resource with the account masked down to its last four digits —
/// whatever api, Region, stage, method or `ConnectionId` the refused call addressed — and the
/// whole account id appears nowhere in the resource it names, which is the point of masking
/// it: the endpoint does not publish the account to a caller it has just refused.
#[tokio::test]
async fn the_reported_resource_masks_all_but_the_last_four_account_digits() {
    let account = engine::emulator_account_id();
    let last_four = account
        .get(account.len().saturating_sub(UNMASKED_ACCOUNT_DIGITS)..)
        .expect("the account id's last four digits");
    for &Addressing {
        case,
        api_id,
        region,
        stage,
        method,
        payload,
        connection_id,
    } in ADDRESSINGS
    {
        let engine = engine_addressing_any_invoke_url();
        let live = MINTED_IDS[0].1;
        crate::support::register(&engine, live, "192.0.2.24", "pusher/5", connected_at());

        let host = invoke_host(api_id, region);
        let (status, headers, body) = call_signed_by(
            &engine,
            SESSION_KEY_ID,
            &host,
            method,
            &format!("/{stage}/@connections/{connection_id}"),
            payload.to_vec(),
        )
        .await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected HTTP 403, got {status} {}",
            rendered(&body)
        );
        assert_eq!(
            error_code(&headers),
            ACCESS_DENIED,
            "case {case:?}: expected {ACCESS_DENIED} in x-amzn-errortype, got {}",
            error_code(&headers)
        );

        let message = error_message(&body);
        let expected = format!(
            "User: {principal} is not authorized to perform: {MANAGE_CONNECTIONS} on resource: \
             arn:aws:execute-api:{region}:{MASKED_ACCOUNT_PREFIX}{last_four}:{api_id}/{stage}/\
             {method}/@connections/{{connectionId}} {reason}",
            principal = session_arn(),
            reason = no_grant(),
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        let reported = resource_clause_of(&message).unwrap_or_else(|| {
            panic!("case {case:?}: the refusal must name a resource, got {message:?}")
        });
        assert!(
            !reported.contains(account),
            "case {case:?}: the reported resource must not carry the whole account id \
             {account:?}, got {reported:?}"
        );
        assert!(
            !reported.contains(connection_id),
            "case {case:?}: the reported resource names the route's own parameter, not the \
             id {connection_id:?} the call carried, got {reported:?}"
        );
    }
}

/// The account's own published credential holds no IAM policies, so it is not bounded by this
/// gate — and neither is a caller of an addressing nothing resolves a stage for.
#[tokio::test]
async fn the_callers_this_gate_cannot_judge_are_served() {
    for (method, payload, served_status) in OPERATIONS {
        // The account's own credential, on the resolved stage.
        let engine = engine_with_a_session();
        given_policy(&engine, None);
        let live = MINTED_IDS[0].1;
        crate::support::register(&engine, live, "192.0.2.22", "pusher/3", connected_at());
        let case = format!("{method} signed with the account's own credential");
        let (status, _, body) = call_signed_by(
            &engine,
            ACCOUNT_KEY_ID,
            HOST,
            method,
            &path(live),
            payload.to_vec(),
        )
        .await;
        assert_eq!(
            status, served_status,
            "case {case:?}: expected {served_status}, got {status} {}",
            rendered(&body)
        );

        // The session's own credential, where no resolution is installed at all — an emulator
        // the owner of apis and stages is not mounted in resolves no resource, so there is
        // nothing a statement could be about.
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        given_session(&engine);
        given_policy(&engine, None);
        crate::support::register(&engine, live, "192.0.2.23", "pusher/4", connected_at());
        let case = format!("{method} where nothing resolves the stage");
        let (status, _, body) = call_signed_by(
            &engine,
            SESSION_KEY_ID,
            HOST,
            method,
            &path(live),
            payload.to_vec(),
        )
        .await;
        assert_eq!(
            status, served_status,
            "case {case:?}: expected {served_status}, got {status} {}",
            rendered(&body)
        );
    }
}
