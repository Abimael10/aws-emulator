// SPDX-License-Identifier: CC0-1.0

//! How the TOPIC RULE DESTINATION plane answers a request it cannot serve, what a
//! destination says about the status it is in, and what is left after one is deleted.
//!
//! Three rules, each asserted over a table that spans its class rather than one recorded
//! request:
//!
//! * a value bound where this service takes an `AwsArn` — the `{arn+}` label of
//!   `GET`/`DELETE /destinations/{arn+}` and `UpdateTopicRuleDestination`'s `arn` member —
//!   that addresses no resource this account could hold is refused before any lookup with
//!   `InvalidRequestException` (HTTP 400): a value that is not an ARN at all in the sentence
//!   every ARN is read by, and an ARN whose account segment is another account's in the
//!   sentence that names both accounts — so a client-side ARN bug is not reported as a
//!   resource that does not exist;
//! * a well-formed ARN that resolves to no destination this account can read is DENIED:
//!   `UnauthorizedException` (HTTP 401), with nothing of the account's state in the answer.
//!   None of these operations models a not-found error
//!   (`smithy_query(query="operation",operation="GetTopicRuleDestination")`), and an ARN
//!   names an account as well as a resource, so non-existence is not disclosed — which is
//!   also what a destination `DeleteTopicRuleDestination` has removed is refused with;
//! * `TopicRuleDestination$statusReason` ("Additional details or reason why the topic rule
//!   destination is in the current status") is populated for a destination that is
//!   `IN_PROGRESS` because its confirmation has not completed — "a topic rule destination
//!   was created but has not been confirmed" (`TopicRuleDestinationStatus`) — and is
//!   readable back through `GetTopicRuleDestination`.
//!
//! The `confirmationUrl` a create will not take is refused in the one sentence this service
//! states for a value that is no HTTPS URL, over a table of the ways a URL can fail it.
//!
//! Two more rules about what a create LEAVES BEHIND, each over its own table:
//!
//! * `CreateTopicRuleDestination` records the destination it answers with: the `arn` it
//!   mints, `status` `IN_PROGRESS` ("The destination must be confirmed prior to use"),
//!   `createdAt`/`lastUpdatedAt` and `httpUrlProperties.confirmationUrl` are read back
//!   member for member through `GetTopicRuleDestination`, and every destination the account
//!   creates is its own record under its own ARN — so a create persists a store rather than
//!   answering the request it was handed;
//! * a destination's identity is the endpoint it confirms, so a second create for a
//!   `confirmationUrl` this account already holds a destination for is refused
//!   `ResourceAlreadyExistsException` ("The resource already exists", HTTP 409) naming the
//!   record that exists, and mints and changes nothing.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every call below is signed for.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// `TopicRuleDestinationStatus$IN_PROGRESS`: "a topic rule destination was created but has
/// not been confirmed".
const IN_PROGRESS: &str = "IN_PROGRESS";

/// The reason a destination whose confirmation has not completed reports.
const AWAITING_CONFIRMATION: &str = "Awaiting confirmation. The confirmation message has not \
                                     been sent to the destination endpoint yet.";

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

    /// The reported destination's `statusReason`, or `None` when the member is absent.
    fn status_reason(&self) -> Option<String> {
        self.destination()
            .and_then(|destination| destination["statusReason"].as_str())
            .map(str::to_string)
    }

    /// The reported destination's `status`.
    fn status_value(&self) -> String {
        self.destination()
            .and_then(|destination| destination["status"].as_str())
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: the IoT control plane on its own engine.
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
}

/// Every way this plane addresses ONE destination by ARN, so each rule below is asserted on
/// all of them rather than on the one a recording happened to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressed {
    /// `GET /destinations/{arn+}`.
    Read,
    /// `DELETE /destinations/{arn+}`.
    Delete,
    /// `PATCH /destinations`, whose `arn` is a body member.
    Update,
}

impl Addressed {
    /// Every operation that addresses one destination by ARN.
    const ALL: [Addressed; 3] = [Addressed::Read, Addressed::Delete, Addressed::Update];

    /// Address `arn` with this operation.
    async fn call(self, emulator: &Emulator, arn: &str) -> Reply {
        match self {
            Addressed::Read => emulator.get_destination(arn).await,
            Addressed::Delete => emulator.delete_destination(arn).await,
            Addressed::Update => emulator.update_destination(arn, IN_PROGRESS).await,
        }
    }
}

/// A value that is not an ARN at all is refused as a MALFORMED ARN, by every operation that
/// takes one, before the destinations store is consulted.
///
/// The rows differ along the way a value can fail to be an ARN: the form with its prefix
/// missing, a bare word, a resource path on its own, the prefix in the wrong case (an ARN's
/// opening is the lowercase `arn:`), and a value that merely contains it somewhere else.
#[tokio::test]
async fn a_value_that_is_not_an_arn_is_refused_as_a_malformed_arn() {
    let emulator = Emulator::new();
    let rows = [
        ("a bare word", "not-a-real-destination-arn"),
        ("a resource path on its own", "ruledestination/http/abc"),
        (
            "the ARN form with its prefix missing",
            "aws:iot:us-east-1:000000000000:ruledestination/http/abc",
        ),
        (
            "the prefix in the wrong case",
            "ARN:aws:iot:us-east-1:000000000000:ruledestination/http/abc",
        ),
        ("a value that merely contains the prefix", "my-arn:aws"),
        ("the prefix without its colon", "arn"),
    ];

    for (case, value) in rows {
        for addressed in Addressed::ALL {
            let refused = addressed.call(&emulator, value).await;
            assert_eq!(
                (refused.status, refused.error_code.as_deref()),
                (
                    400,
                    Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
                ),
                "case {case:?} on {addressed:?}: expected a malformed ARN to be refused \
                 400 InvalidRequestException, got {} {:?}",
                refused.status,
                refused.body
            );
            assert_eq!(
                refused.message(),
                format!("ARNs must start with 'arn:': {value}"),
                "case {case:?} on {addressed:?}: expected the ARN-syntax sentence, got {:?}",
                refused.message()
            );
        }
    }
}

/// An ARN whose account segment is another account's addresses no resource this account
/// could hold, so it is refused — by every operation that takes one, before the destinations
/// store is consulted — in the sentence that names the account the request is for and the
/// ARN it carried.
///
/// The rows differ along which account the ARN names and what the rest of it says: another
/// account in this region, in another region, in another partition, and for another
/// destination kind. A caller's own account is the one case this rule does NOT refuse, which
/// the denial table below covers.
#[tokio::test]
async fn an_arn_of_another_account_is_refused() {
    let emulator = Emulator::new();
    let rows = [
        (
            "another account, this region",
            format!("arn:aws:iot:{REGION}:111111111111:ruledestination/http/nobody"),
        ),
        (
            "another account, another region",
            "arn:aws:iot:eu-west-1:210987654321:ruledestination/http/nobody".to_string(),
        ),
        (
            "another account, another partition",
            "arn:aws-cn:iot:cn-north-1:999999999999:ruledestination/http/nobody".to_string(),
        ),
        (
            "another account, another destination kind",
            format!("arn:aws:iot:{REGION}:111111111111:ruledestination/vpc/nobody"),
        ),
        (
            "another account, another service's resource",
            format!("arn:aws:sqs:{REGION}:111111111111:a-queue"),
        ),
    ];

    for (case, arn) in rows {
        for addressed in Addressed::ALL {
            let refused = addressed.call(&emulator, &arn).await;
            assert_eq!(
                (refused.status, refused.error_code.as_deref()),
                (
                    400,
                    Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
                ),
                "case {case:?} on {addressed:?}: expected an ARN of another account to be \
                 refused 400 InvalidRequestException, got {} {:?}",
                refused.status,
                refused.body
            );
            assert_eq!(
                refused.message(),
                format!("Resource account ({ACCOUNT_ID}) doesn't match account from ARN: {arn}"),
                "case {case:?} on {addressed:?}: expected the account-mismatch sentence, got \
                 {:?}",
                refused.message()
            );
        }
    }
}

/// A well-formed ARN that resolves to no destination this account can read is DENIED, by
/// every operation that addresses one, and the answer discloses nothing about what the
/// account holds.
///
/// The rows differ along what the ARN of this account names: its own destination namespace,
/// another partition, another region, another service's resource, another destination kind,
/// and the shortest value that still opens like an ARN.
#[tokio::test]
async fn a_well_formed_arn_no_destination_answers_to_is_denied() {
    let emulator = Emulator::new();
    // A destination this account DOES hold, so the cases below are about the ARN they name
    // and not about an empty store.
    let created = emulator
        .create_http_destination("https://example.test/iot/confirm")
        .await;
    assert_eq!(
        created.status, 200,
        "the destination the denial cases are about must be created, got {} {:?}",
        created.status, created.body
    );

    let rows = [
        (
            "this account, no such destination",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:ruledestination/http/nobody"),
        ),
        (
            "another partition",
            format!("arn:aws-cn:iot:cn-north-1:{ACCOUNT_ID}:ruledestination/http/nobody"),
        ),
        (
            "another region",
            format!("arn:aws:iot:eu-west-1:{ACCOUNT_ID}:ruledestination/http/nobody"),
        ),
        (
            "another service's resource",
            format!("arn:aws:sqs:{REGION}:{ACCOUNT_ID}:a-queue"),
        ),
        (
            "another destination kind",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:ruledestination/vpc/nobody"),
        ),
        (
            "the shortest value with an ARN's opening",
            "arn:".to_string(),
        ),
    ];

    for (case, arn) in rows {
        for addressed in Addressed::ALL {
            let refused = addressed.call(&emulator, &arn).await;
            assert_eq!(
                (refused.status, refused.error_code.as_deref()),
                (401, Some("UnauthorizedException")),
                "case {case:?} on {addressed:?}: expected 401 UnauthorizedException, got {} {:?}",
                refused.status,
                refused.body
            );
            assert_eq!(
                refused.message(),
                format!("Access to TopicRuleDestination '{arn}' was denied"),
                "case {case:?} on {addressed:?}: expected the denial sentence, got {:?}",
                refused.message()
            );
            assert!(
                refused.destination().is_none(),
                "case {case:?} on {addressed:?}: a denial must disclose no destination, got {:?}",
                refused.body
            );
        }
    }

    // Nothing above touched the destination the account does hold.
    let read = emulator.get_destination(&created.arn()).await;
    assert_eq!(
        read.status, 200,
        "the destination this account holds must still be readable, got {} {:?}",
        read.status, read.body
    );
}

/// A destination that has not been confirmed states WHY it is in the status it is in, and
/// that reason is readable back — on the create's own answer and on every later read.
///
/// The rows differ along the confirmation URL (a path-only URL, one carrying a query
/// string, one naming a port, a longer path), none of which the reason depends on: it is
/// fixed text about the state the destination is in.
#[tokio::test]
async fn an_unconfirmed_destination_states_why_and_reads_it_back() {
    let emulator = Emulator::new();
    let rows = [
        ("a path-only URL", "https://example.test/iot"),
        ("a URL with a query string", "https://example.test/iot?tenant=7"),
        ("a URL naming a port", "https://example.test:8443/iot"),
        (
            "a longer path",
            "https://example.test/iot/destinations/confirm/one",
        ),
    ];

    for (case, url) in rows {
        let created = emulator.create_http_destination(url).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTopicRuleDestination answered {} {:?}",
            created.status, created.body
        );
        assert_eq!(
            created.status_value(),
            IN_PROGRESS,
            "case {case:?}: expected a created destination to be {IN_PROGRESS}, got {:?}",
            created.destination()
        );
        assert_eq!(
            created.status_reason(),
            Some(AWAITING_CONFIRMATION.to_string()),
            "case {case:?}: expected the create to state why the destination is \
             {IN_PROGRESS}, got {:?}",
            created.status_reason()
        );

        let arn = created.arn();
        let read = emulator.get_destination(&arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetTopicRuleDestination answered {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.status_reason(),
            created.status_reason(),
            "case {case:?}: expected the reader to report the stored reason, got {:?}",
            read.status_reason()
        );

        // A requested fresh challenge puts the destination back in the same unconfirmed
        // state, so the same reason is what a reader then finds.
        let challenged = emulator.update_destination(&arn, IN_PROGRESS).await;
        assert_eq!(
            challenged.status, 200,
            "case {case:?}: UpdateTopicRuleDestination({IN_PROGRESS}) answered {} {:?}",
            challenged.status, challenged.body
        );
        let reread = emulator.get_destination(&arn).await;
        assert_eq!(
            (reread.status_value(), reread.status_reason()),
            (IN_PROGRESS.to_string(), Some(AWAITING_CONFIRMATION.to_string())),
            "case {case:?}: expected a re-challenged destination to state why it is \
             {IN_PROGRESS}, got {:?}",
            reread.destination()
        );
    }
}

/// A destination `DeleteTopicRuleDestination` has removed is gone: nothing reads it, a
/// second delete has nothing to remove, and the confirmation URL it held is free again.
///
/// The rows differ along the confirmation URL, so the removal is asserted on more than the
/// one URL a recording used.
#[tokio::test]
async fn a_deleted_destination_is_no_longer_readable() {
    let emulator = Emulator::new();
    let rows = [
        ("a path-only URL", "https://example.test/iot/one"),
        ("a URL with a query string", "https://example.test/iot?two=1"),
        ("a URL naming a port", "https://example.test:8443/iot/three"),
    ];

    for (case, url) in rows {
        let created = emulator.create_http_destination(url).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTopicRuleDestination answered {} {:?}",
            created.status, created.body
        );
        let arn = created.arn();
        assert_eq!(
            emulator.get_destination(&arn).await.status, 200,
            "case {case:?}: the created destination must be readable before the delete"
        );

        let deleted = emulator.delete_destination(&arn).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: DeleteTopicRuleDestination answered {} {:?}",
            deleted.status, deleted.body
        );

        for addressed in Addressed::ALL {
            let refused = addressed.call(&emulator, &arn).await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_code.as_deref(),
                    refused.message()
                ),
                (
                    401,
                    Some("UnauthorizedException"),
                    format!("Access to TopicRuleDestination '{arn}' was denied")
                ),
                "case {case:?} on {addressed:?}: expected a deleted destination to be \
                 denied like one that never existed, got {} {:?}",
                refused.status,
                refused.body
            );
        }

        // The record is really gone, not hidden: the URL it confirmed can be created again,
        // under an ARN of its own.
        let recreated = emulator.create_http_destination(url).await;
        assert_eq!(
            recreated.status, 200,
            "case {case:?}: expected the confirmation URL to be free after the delete, got \
             {} {:?}",
            recreated.status, recreated.body
        );
        assert_ne!(
            recreated.arn(),
            arn,
            "case {case:?}: expected a re-created destination to be a new resource, got {:?}",
            recreated.destination()
        );
    }
}

/// A `confirmationUrl` that is not an HTTPS URL a confirmation request could be sent to is
/// refused, quoting the value the request carried.
///
/// The rows differ along the way the value fails: cleartext, another scheme entirely, a
/// bare word, the empty string the model's `length 0..2000` admits, a URL naming no host,
/// and one whose token would end up after a fragment.
#[tokio::test]
async fn a_confirmation_url_that_is_no_https_url_is_refused() {
    let emulator = Emulator::new();
    let rows = [
        ("cleartext", "http://example.com/insecure"),
        ("another scheme", "ftp://example.test/iot"),
        ("a bare word", "not-a-url"),
        ("the empty string", ""),
        ("a relative path", "/iot/confirm"),
        ("a URL carrying a fragment", "https://example.test/iot#confirm"),
    ];

    for (case, url) in rows {
        let refused = emulator.create_http_destination(url).await;
        assert_eq!(
            (refused.status, refused.error_code.as_deref()),
            (
                400,
                Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            ),
            "case {case:?}: expected the create to be refused 400 InvalidRequestException, \
             got {} {:?}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.message(),
            format!("Invalid confirmationUrl: '{url}' is not a valid HTTPS URL"),
            "case {case:?}: expected the confirmationUrl sentence quoting the value sent, \
             got {:?}",
            refused.message()
        );
    }

    // The same create with an HTTPS URL is taken, so the table above is about the value and
    // not about the operation.
    let created = emulator
        .create_http_destination("https://example.test/iot/confirm")
        .await;
    assert_eq!(
        created.status, 200,
        "an HTTPS confirmationUrl must be taken, got {} {:?}",
        created.status, created.body
    );
}

/// The ARN namespace an HTTP topic rule destination of this account is minted under, which
/// a create mints the destination's own identifier within.
fn http_destination_arn_prefix() -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:ruledestination/http/")
}

/// What a create RECORDED is what a later read reports: the destination the create answered
/// with is readable back member for member, and every destination the account creates is its
/// own record under its own ARN.
///
/// The rows differ along the form of the confirmation URL — a path-only URL, one carrying a
/// query string, one naming a port, a deeper path, and one on another host — none of which
/// the record's shape depends on. All of them are created against ONE emulator, so the final
/// pass over them reads a store holding every row rather than the last request's answer.
#[tokio::test]
async fn a_created_destination_is_persisted_and_read_back_member_for_member() {
    let emulator = Emulator::new();
    let rows = [
        ("a path-only URL", "https://example.test/iot/one"),
        (
            "a URL with a query string",
            "https://example.test/iot/two?tenant=7",
        ),
        ("a URL naming a port", "https://example.test:8443/iot/three"),
        (
            "a deeper path",
            "https://example.test/iot/four/destinations/confirm",
        ),
        ("another host", "https://endpoints.example.test/iot/five"),
    ];

    // (case, the URL the row created a destination for, the ARN the create minted)
    let mut recorded: Vec<(&str, &str, String)> = Vec::new();
    for (case, url) in rows {
        let created = emulator.create_http_destination(url).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTopicRuleDestination answered {} {:?}",
            created.status, created.body
        );
        let destination = created
            .destination()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the create reported no destination"));

        let arn = created.arn();
        let prefix = http_destination_arn_prefix();
        assert!(
            arn.starts_with(&prefix) && arn.len() > prefix.len(),
            "case {case:?}: expected an ARN under {prefix:?} carrying an identifier, got \
             {arn:?}"
        );
        assert_eq!(
            destination["httpUrlProperties"]["confirmationUrl"],
            json!(url),
            "case {case:?}: expected the created destination to report the URL the request \
             named, got {:?}",
            destination
        );
        assert_eq!(
            (
                destination["status"].clone(),
                destination["statusReason"].clone()
            ),
            (json!(IN_PROGRESS), json!(AWAITING_CONFIRMATION)),
            "case {case:?}: expected a created destination to be {IN_PROGRESS} and to say \
             why, got {destination:?}"
        );
        assert!(
            destination.get("vpcProperties").is_none()
                && destination.get("influxDBProperties").is_none(),
            "case {case:?}: an HTTP destination must report no other kind's properties, got \
             {destination:?}"
        );

        // `TopicRuleDestination$createdAt` and `$lastUpdatedAt` are timestamps of the
        // instant the record was written, so both are reported and a record nothing has
        // updated since reports one instant for the two.
        let created_at = destination["createdAt"].as_f64();
        let last_updated_at = destination["lastUpdatedAt"].as_f64();
        assert!(
            created_at.is_some_and(|instant| instant > 0.0),
            "case {case:?}: expected the create to report when the destination was created, \
             got {:?}",
            destination["createdAt"]
        );
        assert_eq!(
            created_at, last_updated_at,
            "case {case:?}: expected a destination nothing has updated to report its \
             creation instant as its last update, got {destination:?}"
        );

        let read = emulator.get_destination(&arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: GetTopicRuleDestination answered {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.destination(),
            Some(&destination),
            "case {case:?}: expected the reader to report the created destination member for \
             member, got {:?}",
            read.body
        );

        recorded.push((case, url, arn));
    }

    for (case, url, arn) in &recorded {
        assert_eq!(
            recorded
                .iter()
                .filter(|(_, _, other)| other == arn)
                .count(),
            1,
            "case {case:?}: expected every destination to be minted under an ARN of its own, \
             got {arn:?} more than once"
        );
        let read = emulator.get_destination(arn).await;
        assert_eq!(
            read.destination()
                .map(|destination| destination["httpUrlProperties"]["confirmationUrl"].clone()),
            Some(json!(url)),
            "case {case:?}: expected {arn} to still report the URL it was created for, got \
             {:?}",
            read.body
        );
    }
}

/// A destination's identity is the endpoint it confirms, so a second create for a
/// `confirmationUrl` this account already holds a destination for is refused — and the
/// refusal names the record that exists, mints nothing and changes nothing.
///
/// The rows differ along the form of the URL the collision is on, and each row's second
/// create is made after the next row's destination exists too, so the refusal is about the
/// URL and not about the store holding exactly one record.
#[tokio::test]
async fn a_second_create_for_a_held_confirmation_url_is_refused_and_changes_nothing() {
    let emulator = Emulator::new();
    let rows = [
        ("a path-only URL", "https://example.test/iot/alpha"),
        (
            "a URL with a query string",
            "https://example.test/iot/beta?tenant=3",
        ),
        ("a URL naming a port", "https://example.test:9443/iot/gamma"),
        ("another host", "https://other.example.test/iot/delta"),
    ];

    // (case, the URL, the ARN the first create minted, the record it left)
    let mut held: Vec<(&str, &str, String, Value)> = Vec::new();
    for (case, url) in rows {
        let created = emulator.create_http_destination(url).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTopicRuleDestination answered {} {:?}",
            created.status, created.body
        );
        let arn = created.arn();
        let record = created
            .destination()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the create reported no destination"));
        held.push((case, url, arn, record));
    }

    for (case, url, arn, record) in &held {
        let duplicate = emulator.create_http_destination(url).await;
        assert_eq!(
            (duplicate.status, duplicate.error_code.as_deref()),
            (409, Some("ResourceAlreadyExistsException")),
            "case {case:?}: expected a second create for a held confirmation URL to be \
             refused 409 ResourceAlreadyExistsException, got {} {:?}",
            duplicate.status,
            duplicate.body
        );
        assert!(
            duplicate.message().contains(url) && duplicate.message().contains("already exists"),
            "case {case:?}: expected the refusal to say the destination for {url} already \
             exists, got {:?}",
            duplicate.message()
        );
        assert_eq!(
            (
                duplicate.body["resourceId"].clone(),
                duplicate.body["resourceArn"].clone()
            ),
            (json!(arn), json!(arn)),
            "case {case:?}: expected the refusal to name the destination that already \
             exists, got {:?}",
            duplicate.body
        );
        assert!(
            duplicate.destination().is_none(),
            "case {case:?}: a refused create must report no destination, got {:?}",
            duplicate.body
        );

        // The record the collision was on is exactly as the first create left it: nothing
        // was re-stamped, re-challenged or minted beside it.
        let read = emulator.get_destination(arn).await;
        assert_eq!(
            read.destination(),
            Some(record),
            "case {case:?}: expected a refused create to leave the held record as it stands, \
             got {:?}",
            read.body
        );
    }

    // Every URL the account holds a destination for still holds exactly the one it was
    // created with, so no refusal above moved a record onto another row's ARN.
    for (case, url, arn, _) in &held {
        let read = emulator.get_destination(arn).await;
        assert_eq!(
            read.destination()
                .map(|destination| destination["httpUrlProperties"]["confirmationUrl"].clone()),
            Some(json!(url)),
            "case {case:?}: expected {arn} to still report the URL it was created for, got \
             {:?}",
            read.body
        );
    }
}
