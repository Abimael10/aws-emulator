// SPDX-License-Identifier: CC0-1.0

//! The HTTP harness the suites added for this fix drive the service through.
//!
//! Every assertion in `console_authorization_lifecycle.rs`, `oauth2_token_acceptance.rs`
//! and `operation_reachability.rs` goes through `dispatch`, so each case exercises
//! routing, deserialization, the handler, the persisted state and serialization — the
//! same path a client takes. The two older suites
//! (`permission_statement_lifecycle.rs`, `oauth2_token_lifecycle.rs`) still carry their
//! own equivalents, the second because it also holds the engine so a case can seed a
//! token. Nothing here asserts anything itself: it only builds the request and reads
//! the answer.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use service_com_amazonaws_signin_signin::state::PermissionStatement;
use service_com_amazonaws_signin_signin::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id the emulator answers for, and the target a console-authorization call
/// addresses when it names none.
pub const EMULATOR_ACCOUNT: &str = "000000000000";

/// Put one permission statement of the account's SignIn resource-based policy into this
/// service's store directly, and answer the id it was stored under. `request` carries
/// the condition members as a `PutResourcePermissionStatement` request names them.
///
/// A case about a READER of that policy (`GetResourcePolicy`,
/// `ListResourcePermissionStatements`, `DeleteResourcePermissionStatement`) establishes
/// its precondition here and then drives the reader over HTTP like any other case. It
/// cannot establish it through the API: writing a statement is an action AWS grants no
/// caller, so `PutResourcePermissionStatement` denies every request and there is no
/// operation on this surface that creates one.
pub fn seed_statement(engine: &engine::InMemoryEngine, request: &Value) -> String {
    let state = service_com_amazonaws_signin_signin::state::service_state(engine)
        .expect("the signin store is reachable over the shared engine");
    let mut store = state.lock();
    let statement_id = store.mint_statement_id();
    let member = |name: &str| request.get(name).and_then(Value::as_str).map(str::to_string);
    store.permission_statements.insert(
        statement_id.clone(),
        PermissionStatement {
            statement_id: statement_id.clone(),
            source_vpc: member("sourceVpc"),
            signin_source_vpce: member("signinSourceVpce"),
            console_source_vpce: member("consoleSourceVpce"),
            vpc_source_ip: member("vpcSourceIp"),
            source_ip: member("sourceIp"),
            requested_region: member("requestedRegion"),
            excluded_principal: member("excludedPrincipal"),
        },
    );
    statement_id
}

/// One emulator: a router carrying only this service, over one shared engine.
pub fn emulator() -> MultiServiceRouter {
    router_over(Arc::new(engine::InMemoryEngine::new()))
}

/// One emulator carrying this service over `engine`, so a caller that holds the engine
/// can read or seed a store this service's handlers answer from.
pub fn router_over(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One emulator carrying this service AND the AWS Organizations control plane over one
/// shared engine, together with the organization that control plane establishes for the
/// emulated account.
///
/// Mounting Organizations is what makes an `o-` target one the caller is authorized for,
/// and the id is MINTED by that control plane — so a test reads it back here instead of
/// choosing it, and no case can pass against an id this emulator would not have.
pub struct WithOrganization {
    pub router: MultiServiceRouter,
    pub engine: Arc<engine::InMemoryEngine>,
    /// `o-…`: the organization the emulated account is the management account of.
    pub organization_id: String,
}

/// Build a [`WithOrganization`] emulator.
pub fn emulator_with_organization() -> WithOrganization {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_over(Arc::clone(&engine));
    router.register_service(
        service_com_amazonaws_organizations_awsorganizationsv20161128::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    let organizations = service_com_amazonaws_organizations_awsorganizationsv20161128::state::service_state(&*engine)
        .expect("the Organizations store is reachable over the shared engine");
    let organization_id = {
        let store = organizations.lock();
        store
            .organization
            .as_ref()
            .expect("mounting the Organizations control plane establishes an organization")
            .id
            .clone()
    };
    WithOrganization {
        router,
        engine,
        organization_id,
    }
}

/// Every target this service's store holds a console-authorization configuration for.
///
/// Read straight off the store, so a case can tell "the refused call created nothing"
/// from "it created a record no reader of this service will serve the caller".
pub fn configured_targets(engine: &engine::InMemoryEngine) -> Vec<String> {
    let state = service_com_amazonaws_signin_signin::state::service_state(engine)
        .expect("the signin store is reachable over the shared engine");
    let mut targets: Vec<String> = state.lock().console_auth_configs.keys().cloned().collect();
    targets.sort();
    targets
}

/// A SigV4 `Authorization` header whose credential scope names this service in `region`
/// — the signal the router resolves a request's service from. The signature is never
/// checked: the harness runs with request authentication off.
pub fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/signin/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// What the emulator answered: the HTTP status, the response headers and the body.
pub struct Answer {
    pub status: u16,
    pub headers: ::http::HeaderMap,
    /// The body parsed as JSON, or `Value::Null` for an empty body.
    pub body: Value,
    /// The body exactly as it came off the wire.
    pub raw_body: Vec<u8>,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    pub fn error_code(&self) -> &str {
        let code = self.body["__type"].as_str().unwrap_or_default();
        code.rsplit('#').next().unwrap_or_default()
    }

    /// The `message` of an error payload, or the empty string.
    pub fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }

    /// The `error` member every modelled error of this service carries, or the empty
    /// string.
    pub fn error_member(&self) -> &str {
        self.body["error"].as_str().unwrap_or_default()
    }
}

/// `POST uri` with `body`, signed for `region` and addressed to `host`.
pub async fn dispatch(
    router: &MultiServiceRouter,
    uri: &str,
    body: Value,
    region: &str,
    host: &str,
) -> Answer {
    let payload = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let request = ::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", host)
        .header("authorization", authorization(region))
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = response.into_body().to_vec();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "POST {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer {
        status,
        headers,
        body,
        raw_body: bytes,
    }
}

/// `POST uri` with `body` from the default region and host the other suites use.
pub async fn post(router: &MultiServiceRouter, uri: &str, body: Value) -> Answer {
    dispatch(router, uri, body, "us-east-1", "signin.us-east-1.amazonaws.com").await
}
