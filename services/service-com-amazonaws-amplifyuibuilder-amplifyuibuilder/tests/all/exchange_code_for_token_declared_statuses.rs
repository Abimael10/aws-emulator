// SPDX-License-Identifier: CC0-1.0

//! `ExchangeCodeForToken` driven through every refusal a request can provoke from it, each
//! answer checked against the statuses its own model declares.
//!
//! The RULE: a refusal of a request carries an error shape the refusing operation's own
//! model declares for it, so the status is one of that operation's declared statuses —
//! including a refusal of a bound this EMULATOR holds one of its own stores under, which AWS
//! has no error of its own for. This service strips every error-type discriminator from a
//! refusal (`src/service/untyped_errors.rs`), so the status is all a client has to classify
//! the answer by, and a shape the model does not declare for the operation is therefore
//! never widened into the channel to carry one of these refusals.
//!
//! `ExchangeCodeForToken` declares `InvalidParameterException` (400) and NO quota (402) or
//! throttling (429) error, though the service models both shapes for the operations that do
//! declare them — so a refusal of an exchange may carry neither. The expectation every row
//! is checked against is read off the shipped model by
//! [`super::modelled_refusal_statuses::declared_statuses`], never typed in here.
//!
//! The rows span what the rule generalises over: every CAUSE a refusal of an exchange can
//! have, and, on one store filled to the emulator's own bound, the FORMS a request past that
//! bound can take plus the two refusal CLASSES that bound keeps apart, since a code refused
//! for the bound is not a code the store has spent.
//! [`super::refresh_token_declared_statuses`] reads the same model for the same rule on the
//! refresh side of this store, and [`super::emulator_store_bound_refusals`] on every other
//! bounded store of this crate.
//!
//! Each suite in this crate carries its own request harness, so this one does too.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::state::{
    service_state, ACCESS_CODE_NOT_VALID_MESSAGE, PROVIDER_TOKEN_SESSION_QUOTA,
};
use service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::modelled_refusal_statuses::{declared_statuses, modelled_status};

/// A SigV4 credential scope naming this service: it is what routes a request here.
const SIGNED_FOR_AMPLIFYUIBUILDER: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/amplifyuibuilder/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The only `TokenProviders` value the model declares
/// (`smithy_query(query="shape",shape="TokenProviders")`).
const PROVIDER: &str = "figma";

/// `ExchangeCodeForTokenRequestBody$redirectUri` is `@required`, so every well-formed row
/// carries one.
const REDIRECT_URI: &str = "https://app.example.com/cb";

/// The response header restJson1 resolves an error shape from. It must be absent from every
/// refusal this service answers.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The shapes this service models for a store bound on the operations that DECLARE one.
/// Each is read back out of the model below, so what the suite shows is `ExchangeCodeForToken`
/// not declaring them rather than the service not modelling them.
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

    /// Whether the answer grants a token pair at all.
    fn grants_a_pair(&self) -> bool {
        self.body["accessToken"].is_string() || self.body["refreshToken"].is_string()
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

    /// `POST /tokens/{provider}` with the body the caller wrote verbatim, so a row can leave
    /// a member out of the payload entirely.
    async fn exchange_body(&self, provider: &str, body: Value) -> Answer {
        let request = ::http::Request::builder()
            .method("POST")
            .uri(format!("/tokens/{provider}"))
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
                    "an exchange answered a body that is not JSON ({error}): {:?}",
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

    /// An exchange of one access code, optionally naming a client.
    async fn exchange(&self, code: &str, client_id: Option<&str>) -> Answer {
        let mut body = json!({"code": code, "redirectUri": REDIRECT_URI});
        if let Some(client_id) = client_id {
            body["clientId"] = json!(client_id);
        }
        self.exchange_body(PROVIDER, body).await
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

/// The statuses every refusal below is checked against: the ones `ExchangeCodeForToken`'s own
/// model declares. The shapes the OTHER bounded operations of this crate refuse with are read
/// out of the model here as well, so a row that passes shows this operation does not declare
/// them rather than the service not modelling them at all.
fn declared_for_an_exchange() -> BTreeSet<u16> {
    let declared = declared_statuses("ExchangeCodeForToken");
    assert!(
        !declared.is_empty(),
        "ExchangeCodeForToken's model must declare an error for a refusal to name, got none"
    );
    for shape in BOUND_SHAPES_ELSEWHERE {
        let modelled = modelled_status(shape).unwrap_or_else(|| {
            panic!("case {shape:?}: the service must model the shape with an @httpError status")
        });
        assert!(
            !declared.contains(&modelled),
            "case {shape:?}: ExchangeCodeForToken must not declare the HTTP {modelled} this \
             shape carries, got {declared:?}"
        );
    }
    declared
}

/// Whichever cause refuses an exchange, the answer carries a status
/// `ExchangeCodeForToken`'s model declares, names no error type, grants no token pair and
/// leaves the session store empty. The rows differ in the CAUSE: a code of no characters, a
/// code of whitespace only, a code member left out of the payload, the `@required`
/// `redirectUri` left out, and a provider outside the enum the label targets.
#[tokio::test]
async fn every_refusal_of_an_exchange_carries_a_status_its_model_declares() {
    let declared = declared_for_an_exchange();
    let emulator = Emulator::new();

    let rows: [(&str, &str, Value); 7] = [
        (
            "a code of no characters",
            PROVIDER,
            json!({"code": "", "redirectUri": REDIRECT_URI}),
        ),
        (
            "a code of whitespace only",
            PROVIDER,
            json!({"code": " \t", "redirectUri": REDIRECT_URI}),
        ),
        (
            "a code of whitespace only, for a client",
            PROVIDER,
            json!({"code": "  ", "redirectUri": REDIRECT_URI, "clientId": "amplify-studio"}),
        ),
        (
            "no code member at all",
            PROVIDER,
            json!({"redirectUri": REDIRECT_URI}),
        ),
        (
            "no redirectUri member at all",
            PROVIDER,
            json!({"code": "a-code-the-store-never-saw"}),
        ),
        (
            "a provider outside TokenProviders",
            "github",
            json!({"code": "a-code-for-another-provider", "redirectUri": REDIRECT_URI}),
        ),
        (
            "the declared provider in another case",
            "FIGMA",
            json!({"code": "a-code-in-another-case", "redirectUri": REDIRECT_URI}),
        ),
    ];

    for (case, provider, body) in rows {
        let refused = emulator.exchange_body(provider, body).await;
        assert!(
            declared.contains(&refused.status),
            "case {case:?}: expected a status ExchangeCodeForToken's model declares \
             ({declared:?}), got {} with {:?}",
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
        assert!(
            !refused.grants_a_pair(),
            "case {case:?}: expected a refusal granting no token pair, got {:?}",
            refused.body
        );
        assert_eq!(
            emulator.session_count(),
            0,
            "case {case:?}: a refused exchange must store nothing, got {} session(s)",
            emulator.session_count()
        );
    }

    // The control: a well-formed exchange of a code the store has not spent is served, so the
    // refusals above are not what this operation answers whatever the request.
    let granted = emulator.exchange("a-code-the-store-has-not-spent", None).await;
    assert_eq!(
        granted.status, 200,
        "a well-formed exchange must be served, got {} with {:?}",
        granted.status, granted.body
    );
}

/// The bound keeps the two refusal classes of an exchange apart, and both carry a status the
/// model declares: a code the store has SPENT is refused as an invalid access code, while a
/// code the bound refused was never spent — presenting it again is refused for the bound
/// again, which is what shows the refusal recorded no session for it.
///
/// The forms a request past the bound can take are read on the same filled store, so one fill
/// carries them all: each names a code the store has not spent, and they differ in HOW the
/// request differs from the exchanges held. (The two-form version of this bound, beside every
/// other store this emulator bounds, is [`super::emulator_store_bound_refusals`].)
#[tokio::test]
async fn an_exchange_the_bound_refused_spends_the_code_it_named_on_nothing() {
    let declared = declared_for_an_exchange();
    let emulator = Emulator::new();

    for index in 0..PROVIDER_TOKEN_SESSION_QUOTA {
        let granted = emulator.exchange(&format!("code-{index}"), None).await;
        assert_eq!(
            granted.status, 200,
            "case session {index}: an exchange within the bound must be granted, got {} with {:?}",
            granted.status, granted.body
        );
    }

    let fresh = "code-the-bound-refused";
    for attempt in ["first", "second"] {
        let refused = emulator.exchange(fresh, None).await;
        assert!(
            declared.contains(&refused.status),
            "case {attempt:?} attempt past the bound: expected a status the model declares \
             ({declared:?}), got {} with {:?}",
            refused.status,
            refused.body
        );
        let message = refused.message();
        assert!(
            message.contains(&PROVIDER_TOKEN_SESSION_QUOTA.to_string())
                && message.contains(WHAT_THE_STORE_HOLDS),
            "case {attempt:?} attempt past the bound: expected the refusal of the bound, got \
             {message:?}"
        );
        assert_ne!(
            message, ACCESS_CODE_NOT_VALID_MESSAGE,
            "case {attempt:?} attempt past the bound: a code the bound refused must not become \
             a spent one, got {message:?}"
        );
    }

    // The forms a request past the bound can take: a fresh code naming a client, a code far
    // longer than the held ones, a code of non-ASCII characters, and a fresh code in another
    // case. Each is refused for the bound, which quotes neither the code nor the client.
    let long_code = format!("code-{}", "L".repeat(400));
    let forms: [(&str, &str, Option<&str>); 4] = [
        (
            "a fresh code naming a client",
            "code-beyond-the-bound",
            Some("studio-client"),
        ),
        ("a code far longer than the held ones", &long_code, None),
        ("a code of non-ASCII characters", "ƒ-zeichen-Ω-换", None),
        ("a fresh code in another case", "CODE-BEYOND-THE-BOUND", None),
    ];
    for (case, code, client_id) in forms {
        let refused = emulator.exchange(code, client_id).await;
        assert!(
            declared.contains(&refused.status),
            "case {case:?}: expected a status ExchangeCodeForToken's model declares \
             ({declared:?}), got {} with {:?}",
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
        assert!(
            !refused.grants_a_pair(),
            "case {case:?}: expected a refusal granting no token pair, got {:?}",
            refused.body
        );
        // With no error type on the wire the sentence is the only machine-readable name this
        // refusal class has, so the row reads both of its parts — the bound, and what the store
        // holds that many of — and then every value the request supplied.
        let message = refused.message();
        assert!(
            message.contains(&PROVIDER_TOKEN_SESSION_QUOTA.to_string())
                && message.contains(WHAT_THE_STORE_HOLDS),
            "case {case:?}: expected the refusal to quote the bound \
             {PROVIDER_TOKEN_SESSION_QUOTA} and {WHAT_THE_STORE_HOLDS:?}, got {message:?}"
        );
        let mut supplied = vec![code, REDIRECT_URI, PROVIDER];
        supplied.extend(client_id);
        for value in supplied {
            assert!(
                !message.contains(value),
                "case {case:?}: expected the refusal to quote nothing the request supplied, got \
                 {value:?} in {message:?}"
            );
        }
        assert_eq!(
            emulator.session_count(),
            PROVIDER_TOKEN_SESSION_QUOTA,
            "case {case:?}: a refused exchange must grow the store by nothing, got {} session(s)",
            emulator.session_count()
        );
    }

    // The other class, at the same bound: a code the store DID spend is refused as an invalid
    // access code, with a status the model declares just the same.
    let spent = emulator.exchange("code-0", None).await;
    assert!(
        declared.contains(&spent.status),
        "case a spent code at the bound: expected a status the model declares ({declared:?}), \
         got {} with {:?}",
        spent.status,
        spent.body
    );
    assert_eq!(
        spent.message(),
        ACCESS_CODE_NOT_VALID_MESSAGE,
        "case a spent code at the bound: expected the invalid-access-code sentence, got {:?}",
        spent.message()
    );
    assert_eq!(
        emulator.session_count(),
        PROVIDER_TOKEN_SESSION_QUOTA,
        "neither refusal at the bound may grow the store, got {} session(s)",
        emulator.session_count()
    );
}
