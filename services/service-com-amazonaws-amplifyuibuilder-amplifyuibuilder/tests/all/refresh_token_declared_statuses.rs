// SPDX-License-Identifier: CC0-1.0

//! `RefreshToken` driven through every refusal a request can provoke from it, each answer
//! checked against the statuses its own model declares.
//!
//! The RULE: a refusal of a request carries an error shape the refusing operation's own
//! model declares for it, so the status is one of that operation's declared statuses —
//! including a refusal of a bound this EMULATOR holds one of its own stores under, which AWS
//! has no error of its own for. This service strips every error-type discriminator from a
//! refusal (`src/service/untyped_errors.rs`), so the status is all a client has to classify
//! the answer by, and a shape the model does not declare for the operation is therefore
//! never widened into the channel to carry one of these refusals.
//!
//! `RefreshToken` declares `InvalidParameterException` (400) and NO quota (402) or
//! throttling (429) error, though the service models both shapes for the operations that do
//! declare them — so a refusal of a refresh may carry neither. The expectation every row is
//! checked against is read off the shipped model by
//! [`super::modelled_refusal_statuses::declared_statuses`], never typed in here.
//!
//! The rows span what the rule generalises over: every CAUSE a refusal of a refresh can
//! have, and, at the emulator's own bound on the token-session store, every FORM a request
//! past that bound can take. [`super::emulator_store_bound_refusals`] reads the same model
//! for the same rule on every other bounded store of this crate.
//!
//! Each suite in this crate carries its own request harness, so this one does too.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::state::{
    service_state, PROVIDER_TOKEN_SESSION_QUOTA,
};
use service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::modelled_refusal_statuses::{declared_statuses, modelled_status};

/// A SigV4 credential scope naming this service: it is what routes a request here.
const SIGNED_FOR_AMPLIFYUIBUILDER: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/amplifyuibuilder/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The only `TokenProviders` value the model declares.
const PROVIDER: &str = "figma";

/// The response header restJson1 resolves an error shape from. It must be absent from every
/// refusal this service answers.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The shapes this service models for a store bound on the operations that DECLARE one.
/// Each is read back out of the model below, so what the suite shows is `RefreshToken` not
/// declaring them rather than the service not modelling them.
const BOUND_SHAPES_ELSEWHERE: [&str; 2] = ["ServiceQuotaExceededException", "ThrottlingException"];

/// What the bounded store holds, as the refusal of the bound names it: this service answers a
/// refusal with no error-type discriminator, so the sentence is the only machine-readable name
/// the class has, and naming the records it is a bound ON is part of it.
const WHAT_THE_STORE_HOLDS: &str = "live token sessions";

/// One emulator, plus the engine its state is read back through: a router carrying only this
/// service over one shared engine, exactly as `core/router` mounts it.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

/// What the emulator answered: the HTTP status, the JSON body, and the error-type header.
struct Answer {
    status: u16,
    body: Value,
    error_type_header: Option<String>,
}

impl Answer {
    /// Whether the answer names an error type anywhere a client would read one.
    fn names_an_error_type(&self) -> bool {
        self.error_type_header.is_some()
            || ["__type", "code", "Code"]
                .iter()
                .any(|member| self.body[*member].is_string())
    }

    /// The `message` the answer carries, in either spelling, or the empty string.
    fn message(&self) -> String {
        self.body["message"]
            .as_str()
            .or_else(|| self.body["Message"].as_str())
            .unwrap_or_default()
            .to_string()
    }
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// `POST /tokens/{provider}/refresh` with the body the caller wrote verbatim, so a row
    /// can leave a member out of the payload entirely.
    async fn refresh_body(&self, provider: &str, body: Value) -> Answer {
        let request = ::http::Request::builder()
            .method("POST")
            .uri(format!("/tokens/{provider}/refresh"))
            .header("host", "amplifyuibuilder.eu-west-1.amazonaws.com")
            .header("authorization", SIGNED_FOR_AMPLIFYUIBUILDER)
            .header("content-type", "application/json")
            .body(Bytes::from(body.to_string()))
            .expect("the test request is well formed");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type_header = response
            .headers()
            .get(ERROR_TYPE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "a refresh answered a body that is not JSON ({error}): {:?}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        Answer {
            status,
            body,
            error_type_header,
        }
    }

    /// A refresh of the session named by one refresh token and, optionally, one client.
    async fn refresh(&self, token: &str, client_id: Option<&str>) -> Answer {
        let mut body = json!({"token": token});
        if let Some(client_id) = client_id {
            body["clientId"] = json!(client_id);
        }
        self.refresh_body(PROVIDER, body).await
    }

    /// The `accessToken` a served refresh reports.
    async fn access_token(&self, token: &str, client_id: Option<&str>) -> String {
        let answer = self.refresh(token, client_id).await;
        assert_eq!(
            answer.status, 200,
            "a refresh of a held session must be served, got {} with {:?}",
            answer.status, answer.body
        );
        answer.body["accessToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "a served refresh must report an accessToken, got {:?}",
                    answer.body
                )
            })
            .to_string()
    }

    /// How many token sessions the store holds.
    fn session_count(&self) -> usize {
        service_state(&*self.engine)
            .expect("the shared engine holds this service's state")
            .lock()
            .provider_token_sessions
            .len()
    }
}

/// The statuses every refusal below is checked against: the ones `RefreshToken`'s own model
/// declares. The shapes the OTHER bounded operations of this crate refuse with are read out
/// of the model here as well, so a row that passes shows this operation does not declare
/// them rather than the service not modelling them at all.
fn declared_for_a_refresh() -> BTreeSet<u16> {
    let declared = declared_statuses("RefreshToken");
    assert!(
        !declared.is_empty(),
        "RefreshToken's model must declare an error for a refusal to name, got none"
    );
    for shape in BOUND_SHAPES_ELSEWHERE {
        let modelled = modelled_status(shape);
        let modelled = modelled.unwrap_or_else(|| {
            panic!("case {shape:?}: the service must model the shape with an @httpError status")
        });
        assert!(
            !declared.contains(&modelled),
            "case {shape:?}: RefreshToken must not declare the HTTP {modelled} this shape \
             carries, got {declared:?}"
        );
    }
    declared
}

/// Whichever cause refuses a refresh, the answer carries a status `RefreshToken`'s model
/// declares, names no error type, and leaves the session store empty. The rows differ in the
/// CAUSE: a token of no characters, a token of whitespace only, a token member left out of
/// the payload, and a provider outside the enum the label targets.
#[tokio::test]
async fn every_refusal_of_a_refresh_carries_a_status_its_model_declares() {
    let declared = declared_for_a_refresh();
    let emulator = Emulator::new();

    let rows: [(&str, &str, Value); 6] = [
        ("a token of no characters", PROVIDER, json!({"token": ""})),
        (
            "a token of whitespace only",
            PROVIDER,
            json!({"token": " \t"}),
        ),
        (
            "a token of whitespace only, for a client",
            PROVIDER,
            json!({"token": "  ", "clientId": "amplify-studio"}),
        ),
        (
            "no token member at all",
            PROVIDER,
            json!({"clientId": "amplify-studio"}),
        ),
        (
            "a provider outside TokenProviders",
            "github",
            json!({"token": "figd_token_1"}),
        ),
        (
            "the declared provider in another case",
            "FIGMA",
            json!({"token": "figd_token_1"}),
        ),
    ];

    for (case, provider, body) in rows {
        let refused = emulator.refresh_body(provider, body).await;
        assert!(
            declared.contains(&refused.status),
            "case {case:?}: expected a status RefreshToken's model declares ({declared:?}), got \
             {} with {:?}",
            refused.status,
            refused.body
        );
        assert!(
            !refused.names_an_error_type(),
            "case {case:?}: expected a refusal naming no error type, got header {:?} and body \
             {:?}",
            refused.error_type_header,
            refused.body
        );
        assert_eq!(
            emulator.session_count(),
            0,
            "case {case:?}: a refused refresh must store nothing, got {} session(s)",
            emulator.session_count()
        );
    }
}

/// However a refresh past the emulator's own bound on the session store differs from the
/// sessions held, the refusal carries a status `RefreshToken`'s model declares — never the
/// quota or throttling status the service models for the operations that declare one — names
/// no error type, quotes the bound and nothing the request supplied, and grows the store by
/// nothing, while a session the store already holds still refreshes.
#[tokio::test]
async fn a_refresh_past_the_session_bound_is_refused_with_a_declared_status() {
    let declared = declared_for_a_refresh();
    let emulator = Emulator::new();

    for index in 0..PROVIDER_TOKEN_SESSION_QUOTA {
        let answer = emulator.refresh(&format!("figd_token_{index}"), None).await;
        assert_eq!(
            answer.status, 200,
            "case session {index}: a refresh within the bound must be served, got {} with {:?}",
            answer.status, answer.body
        );
    }

    // The forms a request past the bound can take: each names a session the store does not
    // hold, and the rows differ in HOW its name differs from the ones held — a fresh token, a
    // held token under another client, a held token in another case, a token far longer than
    // the ones held, and a token of non-ASCII characters.
    let long_token = format!("figd_{}", "L".repeat(400));
    let rows: [(&str, &str, Option<&str>); 5] = [
        (
            "a token the store holds no session for",
            "figd_token_beyond",
            None,
        ),
        (
            "a held token presented by another client",
            "figd_token_0",
            Some("studio-client"),
        ),
        ("a held token in another case", "FIGD_TOKEN_0", None),
        ("a token far longer than the held ones", &long_token, None),
        ("a token of non-ASCII characters", "figd_zeichen_Ω_刷新", None),
    ];

    for (case, token, client_id) in rows {
        let refused = emulator.refresh(token, client_id).await;
        assert!(
            declared.contains(&refused.status),
            "case {case:?}: expected a status RefreshToken's model declares ({declared:?}), got \
             {} with {:?}",
            refused.status,
            refused.body
        );
        assert!(
            !refused.names_an_error_type(),
            "case {case:?}: expected a refusal naming no error type, got header {:?} and body \
             {:?}",
            refused.error_type_header,
            refused.body
        );
        // With no error type on the wire the sentence is the only machine-readable name
        // this refusal class has, so the row reads both of its parts: the bound, and what
        // the store holds that many of.
        let message = refused.message();
        assert!(
            message.contains(&PROVIDER_TOKEN_SESSION_QUOTA.to_string())
                && message.contains(WHAT_THE_STORE_HOLDS),
            "case {case:?}: expected the refusal to quote the bound \
             {PROVIDER_TOKEN_SESSION_QUOTA} and {WHAT_THE_STORE_HOLDS:?}, got {message:?}"
        );
        for supplied in [token, client_id.unwrap_or(PROVIDER)] {
            assert!(
                !message.contains(supplied),
                "case {case:?}: expected the refusal to quote nothing the request supplied, got \
                 {supplied:?} in {message:?}"
            );
        }
        assert_eq!(
            emulator.session_count(),
            PROVIDER_TOKEN_SESSION_QUOTA,
            "case {case:?}: a refused refresh must grow the store by nothing, got {} session(s)",
            emulator.session_count()
        );
    }

    // The control: a session the store already holds still refreshes at the bound, and takes
    // a NEW access token, so the refusals above are not what this operation answers whatever
    // the request.
    let before = emulator.access_token("figd_token_0", None).await;
    let after = emulator.access_token("figd_token_0", None).await;
    assert_ne!(
        before, after,
        "a refresh at the bound must still mint a new access token, got {after:?} twice"
    );
    assert_eq!(
        emulator.session_count(),
        PROVIDER_TOKEN_SESSION_QUOTA,
        "refreshing a held session must not grow the store, got {} session(s)",
        emulator.session_count()
    );
}
