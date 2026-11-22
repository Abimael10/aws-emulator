// SPDX-License-Identifier: CC0-1.0

//! What `ConfirmTopicRuleDestination` answers a confirmation token that no outstanding
//! destination challenge carries.
//!
//! The rule in one sentence: a confirmation token no outstanding challenge of this account
//! holds confirms nothing, and the call is refused with `InvalidRequestException` (HTTP
//! 400, the one client error this operation models for a request it cannot serve —
//! `smithy_query(query="operation",operation="ConfirmTopicRuleDestination")`) carrying the
//! family's whole sentence, "Invalid confirmation token.", terminating period included and
//! with nothing of the request interpolated into it — the token is never echoed.
//!
//! The tables span what the rule generalises over rather than one token a recording
//! happened to carry:
//!
//! * the FORM and LENGTH of the token — a value never issued, near misses of the issued one
//!   (a character added, its last character dropped, its case flipped), a token written as
//!   several path segments (the label is the greedy `{confirmationToken+}`), and the
//!   shortest and longest value `ConfirmationToken` admits (`length 1..2048`), plus one
//!   character past that ceiling, which the modelled constraint refuses before the handler
//!   runs;
//! * the ORIGIN of the token — one a fresh challenge superseded ("Calling
//!   `UpdateTopicRuleDestination` causes a new confirmation challenge to be sent to your
//!   confirmation endpoint", `TopicRuleDestinationStatus$IN_PROGRESS`) and one whose
//!   destination has been deleted: both were issued by this service and neither is
//!   outstanding any more.
//!
//! Each case also asserts what the refusal LEAVES: every destination the account holds is
//! still `IN_PROGRESS`/unconfirmed, and the token that IS outstanding still confirms its
//! destination afterwards — so the refusal is a lookup that found nothing and not a
//! confirmation spent on the wrong record.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::service_state;
use service_com_amazonaws_iot_awsiotservice::state::topic_rule_destinations::{
    DESTINATION_IN_PROGRESS, INVALID_CONFIRMATION_TOKEN_MESSAGE,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every call below is signed for.
const REGION: &str = "us-east-1";

/// `InvalidRequestException` as this service NAMES it in `x-amzn-errortype`: the shape
/// name, a `:`, and the service's API doc namespace.
const QUALIFIED_INVALID_REQUEST: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The longest value `ConfirmationToken` admits: the member is modelled `length 1..2048`
/// (`smithy_query(query="operation",operation="ConfirmTopicRuleDestination")`).
const LONGEST_TOKEN: usize = 2048;

/// How long a token has to be before finding it inside the refusal's own sentence means it
/// was ECHOED: a shorter value can sit in that text by coincidence.
const INCIDENTAL_SUBSTRING_LENGTH: usize = 8;

/// One HTTP answer, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The error message a modelled failure carries.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `topicRuleDestination` member a create or a read reports, or `None` when the
    /// answer carries none.
    fn destination(&self) -> Option<&Value> {
        self.body.get("topicRuleDestination")
    }

    /// The reported destination's `arn`.
    fn arn(&self) -> String {
        self.destination()
            .and_then(|destination| destination["arn"].as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// The reported destination's `status`.
    fn status_value(&self) -> String {
        self.destination()
            .and_then(|destination| destination["status"].as_str())
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: the IoT control plane on its own engine. The engine is kept so
/// a case can read the token the service ISSUED, which no response hands back — it goes to
/// the destination's confirmation endpoint.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// One IoT control-plane call (restJson1 over the `iot` signing name).
    async fn iot(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("host", format!("iot.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
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
            error_code,
            body,
        }
    }

    /// `CreateTopicRuleDestination` of an HTTP destination for `confirmation_url`.
    async fn create_http_destination(&self, confirmation_url: &str) -> Reply {
        self.iot(
            "POST",
            "/destinations",
            Some(&json!({
                "destinationConfiguration": {
                    "httpUrlConfiguration": { "confirmationUrl": confirmation_url },
                },
            })),
        )
        .await
    }

    /// `GetTopicRuleDestination`: `GET /destinations/{arn+}`.
    async fn get_destination(&self, arn: &str) -> Reply {
        self.iot("GET", &format!("/destinations/{arn}"), None).await
    }

    /// `DeleteTopicRuleDestination`: `DELETE /destinations/{arn+}`.
    async fn delete_destination(&self, arn: &str) -> Reply {
        self.iot("DELETE", &format!("/destinations/{arn}"), None)
            .await
    }

    /// `UpdateTopicRuleDestination`: `PATCH /destinations`.
    async fn update_destination(&self, arn: &str, status: &str) -> Reply {
        self.iot(
            "PATCH",
            "/destinations",
            Some(&json!({ "arn": arn, "status": status })),
        )
        .await
    }

    /// `ConfirmTopicRuleDestination`: `GET /confirmdestination/{confirmationToken+}`.
    async fn confirm_destination(&self, token: &str) -> Reply {
        self.iot("GET", &format!("/confirmdestination/{token}"), None)
            .await
    }

    /// The token the destination's OUTSTANDING challenge carries, read off the store the
    /// way its confirmation endpoint reads it off the challenge message.
    fn outstanding_token(&self, arn: &str) -> String {
        let state = service_state(&*self.engine).expect("the IoT service store");
        let store = state.lock();
        store
            .topic_rule_destinations
            .get(arn)
            .unwrap_or_else(|| panic!("the store holds the destination {arn:?}"))
            .confirmation_token
            .clone()
    }

    /// Create an HTTP destination for `confirmation_url` and answer its ARN and the token
    /// its challenge carries.
    async fn destination_with_token(&self, case: &str, confirmation_url: &str) -> (String, String) {
        let created = self.create_http_destination(confirmation_url).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTopicRuleDestination({confirmation_url}) answered {} {:?}",
            created.status, created.body
        );
        let arn = created.arn();
        let token = self.outstanding_token(&arn);
        assert!(
            !token.is_empty(),
            "case {case:?}: the created destination {arn:?} must carry an outstanding token, \
             got {token:?}"
        );
        (arn, token)
    }
}

/// Assert that `refused` is the one refusal this operation answers an unknown token with:
/// HTTP 400, `InvalidRequestException`, and the family's whole sentence, with the token
/// nowhere in it.
fn assert_refused_as_an_unknown_token(case: &str, token: &str, refused: &Reply) {
    assert_eq!(
        (refused.status, refused.error_code.as_deref()),
        (400, Some(QUALIFIED_INVALID_REQUEST)),
        "case {case:?}: expected 400 InvalidRequestException, got {} {:?} {:?}",
        refused.status,
        refused.error_code,
        refused.body
    );
    assert_eq!(
        refused.message(),
        "Invalid confirmation token.",
        "case {case:?}: the sentence must be AWS's whole one, terminating period included, \
         got {:?}",
        refused.message()
    );
    // A value short enough to sit inside the sentence by coincidence (a single letter) says
    // nothing about echoing, so the check is made on the values long enough to be the
    // request's own.
    if token.len() >= INCIDENTAL_SUBSTRING_LENGTH {
        assert!(
            !refused.message().contains(token),
            "case {case:?}: the refusal must not echo the token, got {:?}",
            refused.message()
        );
    }
}

/// Assert the destination `arn` is still unconfirmed: `IN_PROGRESS`, which is what a refused
/// confirmation must leave.
async fn assert_still_in_progress(emulator: &Emulator, case: &str, arn: &str) {
    let read = emulator.get_destination(arn).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetTopicRuleDestination({arn}) answered {} {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.status_value(),
        DESTINATION_IN_PROGRESS,
        "case {case:?}: a refused confirmation must leave {arn:?} unconfirmed, got {:?}",
        read.status_value()
    );
}

/// The sentence the state layer holds for this refusal, which every caller of the handler's
/// token lookup is answered with, is AWS's whole one — the terminating period is part of it.
#[test]
fn the_refusal_sentence_is_aws_s_whole_sentence() {
    assert_eq!(
        INVALID_CONFIRMATION_TOKEN_MESSAGE, "Invalid confirmation token.",
        "the shared refusal sentence must be AWS's, got {INVALID_CONFIRMATION_TOKEN_MESSAGE:?}"
    );
}

/// A token no outstanding challenge carries is refused in ONE sentence, whatever form or
/// length it takes — and the account's destinations are left exactly as they were.
///
/// The rows differ along the form and length of the value: never issued, three near misses
/// of the token that IS outstanding (a character added, the last one dropped, the case
/// flipped), a value written as several path segments, and each bound of the modelled
/// `ConfirmationToken` range (`length 1..2048`).
#[tokio::test]
async fn a_token_no_outstanding_challenge_carries_is_refused_in_one_sentence() {
    let emulator = Emulator::new();
    // Two destinations, so the lookup runs over a store holding more than one record.
    let (first_arn, outstanding) = emulator
        .destination_with_token("the store's first destination", "https://example.test/iot/one")
        .await;
    let (second_arn, other_outstanding) = emulator
        .destination_with_token(
            "the store's second destination",
            "https://example.test/iot/two",
        )
        .await;

    let rows = [
        (
            "a value no challenge ever carried",
            "unissued-confirmation-token".to_string(),
        ),
        (
            "the outstanding token with one more character",
            format!("{outstanding}z"),
        ),
        (
            "the outstanding token with its last character dropped",
            outstanding[..outstanding.len() - 1].to_string(),
        ),
        (
            "the outstanding token in upper case",
            outstanding.to_uppercase(),
        ),
        (
            "a value written as several path segments",
            "no/such/confirmation/token".to_string(),
        ),
        // The bounds of the modelled `ConfirmationToken` range, `length 1..2048`.
        ("the shortest value the model admits", "t".to_string()),
        (
            "the longest value the model admits",
            "t".repeat(LONGEST_TOKEN),
        ),
    ];

    for (case, token) in &rows {
        let refused = emulator.confirm_destination(token).await;
        assert_refused_as_an_unknown_token(case, token, &refused);
        assert_still_in_progress(&emulator, case, &first_arn).await;
        assert_still_in_progress(&emulator, case, &second_arn).await;
    }

    // One character past the modelled ceiling is refused by the constraint rather than by
    // the lookup, so only that it is NOT served is asserted here.
    let too_long = "t".repeat(LONGEST_TOKEN + 1);
    let past_the_ceiling = emulator.confirm_destination(&too_long).await;
    assert_ne!(
        past_the_ceiling.status, 200,
        "case \"one character past the modelled ceiling\": a token of {} characters must not \
         be served, got {} {:?}",
        too_long.len(),
        past_the_ceiling.status,
        past_the_ceiling.body
    );
    assert_still_in_progress(
        &emulator,
        "one character past the modelled ceiling",
        &first_arn,
    )
    .await;

    // The refusals spent nothing: each token that IS outstanding still confirms its own
    // destination.
    for (case, arn, token) in [
        ("the first destination", &first_arn, &outstanding),
        ("the second destination", &second_arn, &other_outstanding),
    ] {
        let confirmed = emulator.confirm_destination(token).await;
        assert_eq!(
            confirmed.status, 200,
            "case {case:?}: the outstanding token must still confirm {arn:?}, got {} {:?}",
            confirmed.status, confirmed.body
        );
    }
}

/// A token this service ISSUED but no longer holds outstanding is refused the same way: the
/// rule is about what is outstanding, not about what was ever minted.
///
/// The rows differ along how the token stopped being outstanding — a fresh confirmation
/// challenge superseded it ("Calling `UpdateTopicRuleDestination` causes a new confirmation
/// challenge to be sent to your confirmation endpoint"), and its destination was deleted.
#[tokio::test]
async fn a_token_that_is_no_longer_outstanding_is_refused_the_same_way() {
    let emulator = Emulator::new();

    // Superseded: asking for `IN_PROGRESS` again mints a fresh challenge, so the token the
    // previous one carried confirms nothing.
    let superseded_case = "a token a fresh challenge superseded";
    let (superseded_arn, superseded) = emulator
        .destination_with_token(superseded_case, "https://example.test/iot/superseded")
        .await;
    let rechallenged = emulator
        .update_destination(&superseded_arn, DESTINATION_IN_PROGRESS)
        .await;
    assert_eq!(
        rechallenged.status, 200,
        "case {superseded_case:?}: asking for a fresh challenge answered {} {:?}",
        rechallenged.status, rechallenged.body
    );
    let fresh = emulator.outstanding_token(&superseded_arn);
    assert_ne!(
        fresh, superseded,
        "case {superseded_case:?}: a fresh challenge must mint a token of its own, got \
         {fresh:?} twice"
    );

    // Deleted: the destination the token belonged to is gone, so nothing answers to it.
    let deleted_case = "a token whose destination was deleted";
    let (deleted_arn, deleted) = emulator
        .destination_with_token(deleted_case, "https://example.test/iot/deleted")
        .await;
    let removed = emulator.delete_destination(&deleted_arn).await;
    assert_eq!(
        removed.status, 200,
        "case {deleted_case:?}: DeleteTopicRuleDestination answered {} {:?}",
        removed.status, removed.body
    );

    for (case, token) in [(superseded_case, &superseded), (deleted_case, &deleted)] {
        let refused = emulator.confirm_destination(token).await;
        assert_refused_as_an_unknown_token(case, token, &refused);
    }

    // The re-challenged destination is still unconfirmed, and its FRESH token confirms it.
    assert_still_in_progress(&emulator, superseded_case, &superseded_arn).await;
    let confirmed = emulator.confirm_destination(&fresh).await;
    assert_eq!(
        confirmed.status, 200,
        "case {superseded_case:?}: the fresh token must confirm {superseded_arn:?}, got {} {:?}",
        confirmed.status, confirmed.body
    );
}
