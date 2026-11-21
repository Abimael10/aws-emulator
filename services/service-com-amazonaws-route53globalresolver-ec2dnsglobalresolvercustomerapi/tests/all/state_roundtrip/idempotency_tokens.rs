// SPDX-License-Identifier: CC0-1.0

//! The idempotency-record half of this service's state contract, driven
//! end-to-end over the service's own `restJson1` wire through the same
//! `MultiServiceRouter` the production emulator dispatches with.
//!
//! The RULE every test here asserts: a create this service REFUSED mints nothing,
//! so it records no idempotency token either — a later call carrying the same
//! `clientToken` is served on its own merits. A token a SUCCESSFUL create recorded
//! replays that one resource for an identical request, and is a conflict for a
//! different one.
//!
//! It holds for every `@idempotent` create this service serves, so the table below
//! has a row per create (`CreateGlobalResolver`, `CreateDNSView`,
//! `CreateFirewallDomainList` — the three whose input carries an
//! `@idempotencyToken`, from `smithy_query`) crossed with the ways each one is
//! refused: an unresolvable parent, and the bounds its own members declare.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_route53globalresolver_ec2dnsglobalresolvercustomerapi as resolver_service;
use smithy_server_runtime::MultiServiceRouter;

/// The Region the request's SigV4 credential scope names. Route 53 Global
/// Resolver's own docs tell callers to use US East (Ohio).
const REGION: &str = "us-east-2";

/// This service's SigV4 signing name, which is also the router's route key.
const SIGNING_NAME: &str = "route53globalresolver";

/// A Global Resolver id this store never minted, so every call that resolves it is
/// the modelled 404. Its form satisfies `ResourceId`'s own pattern
/// (`^[-.a-zA-Z0-9]+$`, from `smithy_query`), so the refusal is the LOOKUP's and
/// not the boundary validator's.
const ABSENT_RESOLVER_ID: &str = "gr-000000000000dead";

/// Which `@idempotent` create a row drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Create {
    GlobalResolver,
    DnsView,
    FirewallDomainList,
}

/// One emulator holding exactly this service, dispatched over HTTP.
struct Emulator {
    router: MultiServiceRouter,
}

/// A dispatched response: status, the `x-amzn-errortype` header (empty on success)
/// and the parsed JSON body.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answer {
    fn member(&self, name: &str) -> &Value {
        self.body.get(name).unwrap_or(&Value::Null)
    }

    fn text(&self, name: &str) -> String {
        self.member(name).as_str().unwrap_or_default().to_string()
    }
}

impl Emulator {
    fn new() -> Self {
        // The SINGLE shared engine, exactly as `router/src/lib.rs` builds it.
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(resolver_service::EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/{SIGNING_NAME}/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0000000000000000000000000000000000000000000000000000000000000000"
        );
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(&value).expect("a serializable request body")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = response.body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(bytes).unwrap_or(Value::Null)
        };
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// The create this row drives, at the URI the model binds it to
    /// (`smithy_query`: `POST /global-resolver`, `POST /dns-views/{globalResolverId}`,
    /// `POST /firewall-domain-lists/{globalResolverId}`). `parent` is the
    /// `globalResolverId` label, which `CreateGlobalResolver` has no member for.
    async fn create(&self, kind: Create, parent: &str, body: Value) -> Answer {
        let path = match kind {
            Create::GlobalResolver => "/global-resolver".to_string(),
            Create::DnsView => format!("/dns-views/{parent}"),
            Create::FirewallDomainList => format!("/firewall-domain-lists/{parent}"),
        };
        self.call("POST", &path, Some(body)).await
    }

    /// The matching read, so a create is proved by the state a later caller sees.
    async fn read(&self, kind: Create, id: &str) -> Answer {
        let path = match kind {
            Create::GlobalResolver => format!("/global-resolver/{id}"),
            Create::DnsView => format!("/dns-views/{id}"),
            Create::FirewallDomainList => format!("/firewall-domain-lists/{id}"),
        };
        self.call("GET", &path, None).await
    }

    /// `CreateGlobalResolver` — the parent a view and a domain list hang off.
    async fn create_parent(&self, name: &str) -> String {
        let answer = self
            .create(
                Create::GlobalResolver,
                "",
                json!({ "name": name, "regions": [REGION] }),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "setup: CreateGlobalResolver({name:?}) must succeed, got {} with {}",
            answer.status, answer.body
        );
        answer.text("id")
    }
}

/// One row: a create this service refuses, the refusal it answers with, and the
/// well-formed create the SAME `clientToken` goes on to make.
struct Row {
    case: &'static str,
    kind: Create,
    /// The `globalResolverId` label the refused call carries, or `None` for the
    /// resolver the test created.
    refused_parent: Option<&'static str>,
    refused_body: Value,
    status: u16,
    error_type: &'static str,
    accepted_body: Value,
    /// This row's `clientToken`, spent first on the refusal and then on the create.
    token: &'static str,
}

/// The table: every implemented `@idempotent` create, crossed with the refusal
/// classes its own input declares (`name` length 1..64, `description` length
/// 1..256, `regions` required, `globalResolverId` resolved — from `smithy_query`
/// on each operation).
fn rows() -> Vec<Row> {
    let long_name = "n".repeat(65);
    let long_description = "d".repeat(257);
    vec![
        Row {
            case: "CreateGlobalResolver, a name past its 64-character bound",
            kind: Create::GlobalResolver,
            refused_parent: None,
            refused_body: json!({ "name": long_name, "regions": [REGION] }),
            status: 400,
            error_type: "ValidationException",
            accepted_body: json!({ "name": "resolver-after-a-long-name", "regions": [REGION] }),
            token: "token-resolver-long-name",
        },
        Row {
            case: "CreateGlobalResolver, no regions at all, which the model requires",
            kind: Create::GlobalResolver,
            refused_parent: None,
            refused_body: json!({ "name": "regionless" }),
            status: 400,
            error_type: "ValidationException",
            accepted_body: json!({
                "name": "resolver-after-a-missing-member",
                "regions": [REGION, "eu-west-1"],
                "description": "the token's second call"
            }),
            token: "token-resolver-missing-regions",
        },
        Row {
            case: "CreateDNSView, a parent id the store does not hold",
            kind: Create::DnsView,
            refused_parent: Some(ABSENT_RESOLVER_ID),
            refused_body: json!({ "name": "orphan-view" }),
            status: 404,
            error_type: "ResourceNotFoundException",
            accepted_body: json!({ "name": "view-after-a-404" }),
            token: "token-view-absent-parent",
        },
        Row {
            case: "CreateDNSView, an empty name, under its 1-character bound",
            kind: Create::DnsView,
            refused_parent: None,
            refused_body: json!({ "name": "" }),
            status: 400,
            error_type: "ValidationException",
            accepted_body: json!({
                "name": "view-after-an-empty-name",
                "dnssecValidation": "ENABLED"
            }),
            token: "token-view-empty-name",
        },
        Row {
            case: "CreateFirewallDomainList, a parent id the store does not hold",
            kind: Create::FirewallDomainList,
            refused_parent: Some(ABSENT_RESOLVER_ID),
            refused_body: json!({ "name": "orphan-list" }),
            status: 404,
            error_type: "ResourceNotFoundException",
            accepted_body: json!({ "name": "list-after-a-404" }),
            token: "token-list-absent-parent",
        },
        Row {
            case: "CreateFirewallDomainList, a description past its 256-character bound",
            kind: Create::FirewallDomainList,
            refused_parent: None,
            refused_body: json!({ "name": "long-description", "description": long_description }),
            status: 400,
            error_type: "ValidationException",
            accepted_body: json!({
                "name": "list-after-a-long-description",
                "description": "the token's second call"
            }),
            token: "token-list-long-description",
        },
    ]
}

/// The name a row's conflicting replay carries: the same token, one different
/// parameter. Within `ResourceName`'s length and pattern
/// (`^(?!^[0-9]+$)([a-zA-Z0-9-_/' ']+)$`, from `smithy_query`).
const RENAMED: &str = "renamed-under-the-same-token";

#[tokio::test]
async fn a_refused_create_records_no_idempotency_token() {
    let emulator = Emulator::new();
    let parent = emulator.create_parent("token-parent").await;
    let mut minted_ids: BTreeSet<String> = BTreeSet::new();

    for row in rows() {
        let token = row.token;

        // The refusal. It is the service's answer on this request's own merits,
        // and it mints nothing.
        let mut refused_body = row.refused_body.clone();
        refused_body["clientToken"] = json!(token);
        let refused = emulator
            .create(
                row.kind,
                row.refused_parent.unwrap_or(parent.as_str()),
                refused_body,
            )
            .await;
        assert_eq!(
            refused.status, row.status,
            "case {:?}: expected {}, got {} with {}",
            row.case, row.status, refused.status, refused.body
        );
        assert!(
            refused.error_type.contains(row.error_type),
            "case {:?}: expected a {}, got {:?} with {}",
            row.case,
            row.error_type,
            refused.error_type,
            refused.body
        );
        assert_eq!(
            refused.text("id"),
            String::new(),
            "case {:?}: a refused create answers no resource id, got {:?}",
            row.case,
            refused.member("id")
        );

        // The same token, on a well-formed request: nothing of the refusal was
        // recorded under it, so this call is served on its own merits rather than
        // replayed from — or masked by — the refusal.
        let mut accepted_body = row.accepted_body.clone();
        accepted_body["clientToken"] = json!(token);
        let accepted = emulator.create(row.kind, &parent, accepted_body.clone()).await;
        assert_eq!(
            accepted.status, 200,
            "case {:?}: a token spent on a refused create must still serve a \
             well-formed one, got {} with {}",
            row.case, accepted.status, accepted.body
        );
        let id = accepted.text("id");
        assert!(
            minted_ids.insert(id.clone()),
            "case {:?}: every accepted create mints its own resource, got {id:?} twice",
            row.case
        );

        // The record the create wrote, as the next reader sees it: the stored
        // resource carries the token of the call that minted it.
        let stored = emulator.read(row.kind, &id).await;
        assert_eq!(
            stored.status, 200,
            "case {:?}: expected 200 reading {id:?} back, got {} with {}",
            row.case, stored.status, stored.body
        );
        assert_eq!(
            stored.text("clientToken"),
            token,
            "case {:?}: expected the stored resource to carry {token:?}, got {:?}",
            row.case,
            stored.member("clientToken")
        );

        // And now the token IS recorded: the same request again replays the one
        // resource it minted rather than minting a second.
        let replayed = emulator.create(row.kind, &parent, accepted_body.clone()).await;
        assert_eq!(
            replayed.status, 200,
            "case {:?}: expected the replay to be served, got {} with {}",
            row.case, replayed.status, replayed.body
        );
        assert_eq!(
            replayed.text("id"),
            id,
            "case {:?}: expected the replay to answer {id:?}, got {:?}",
            row.case,
            replayed.member("id")
        );

        // The other half of the same record: the token reused for a DIFFERENT
        // resource is the modelled 409, not a second resource.
        let mut conflicting_body = accepted_body.clone();
        conflicting_body["name"] = json!(RENAMED);
        let conflict = emulator.create(row.kind, &parent, conflicting_body).await;
        assert_eq!(
            conflict.status, 409,
            "case {:?}: expected 409 for {token:?} reused with a different name, \
             got {} with {}",
            row.case, conflict.status, conflict.body
        );
        assert!(
            conflict.error_type.contains("ConflictException"),
            "case {:?}: expected a ConflictException, got {:?} with {}",
            row.case,
            conflict.error_type,
            conflict.body
        );
    }

    // Every row minted exactly one resource, so the store grew by the rows and by
    // the parent alone — a refused create left nothing behind for any of them.
    assert_eq!(
        minted_ids.len(),
        rows().len(),
        "expected one resource per row, got {minted_ids:?}"
    );
}
