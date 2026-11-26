// SPDX-License-Identifier: CC0-1.0

//! A `SendBulkEmail` call with NO message to compose, end to end over HTTP.
//!
//! The rule these cases pin: `SendBulkEmailOutput$BulkEmailEntryResults` is a REQUIRED
//! member, which is the operation's partial-success contract — a defect in one entry's own
//! destination is reported as that entry's `BulkEmailEntryResult` rather than as a failure of
//! the call. So a request whose EVERY entry carries such a defect composes no message at all
//! and is settled on those entries alone: the answer is the 200 whose results carry each
//! entry's own `INVALID_PARAMETER` and its error, and nothing of the ACCOUNT is weighed for
//! it — not its sending switch, not the configuration set or the template the request names,
//! and not the identity it would have sent from — because there is no message for any of
//! those to apply to — and neither is what an entry states for its OWN message beyond its
//! destination, its `ReplacementHeaders` and `ReplacementTags`, which are read for a call
//! that has a message to compose.
//!
//! Each case therefore drives the same all-entries-defective request against an account that
//! would refuse a REAL message: a sender it has not verified, a configuration set it does not
//! hold, a set whose sending is switched off, its own sending switch off, and a template name
//! it holds nothing under. The counter-cases in
//! `a_call_that_has_a_message_to_compose_is_still_weighed_whole` are what keep this from
//! disabling those gates: the same request with ONE well-formed entry beside the defective
//! ones is still refused whole by each of them.
//!
//! The per-entry wording itself is pinned beside the other per-entry results in
//! `tests/all/send_bulk_email.rs` and `tests/all/send_request_validation.rs`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::ComAmazonawsSesv2SimpleemailserviceV2State;
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for.
const HOME: &str = "us-east-1";

/// The verified sending domain, and the address inside it every call sends from unless the
/// account state under test replaces it.
const SENDER_DOMAIN: &str = "senders.example.com";
const SENDER: &str = "sender@senders.example.com";

/// A sender whose syntax is fine and whose identity the account has NOT verified.
const UNVERIFIED: &str = "stranger@example.com";

/// One of the mailbox SIMULATOR's own mailboxes: it belongs to no account and needs no
/// identity, so a counter-case that addresses it reads the sender's own gate alone.
const SIMULATOR: &str = "success@simulator.amazonses.com";

/// The stored template the calls render from, and its parts.
const TEMPLATE: &str = "receipt";
const TEMPLATE_SUBJECT: &str = "Your receipt, {{name}}";
const TEMPLATE_TEXT: &str = "Thanks {{name}}.";

/// The values that go with that template.
const DATA: &str = "{\"name\":\"Ada\"}";

/// A configuration set name the account holds nothing under, and a template name it holds
/// nothing under.
const ABSENT_SET: &str = "no-such-set";
const ABSENT_TEMPLATE: &str = "no-such-template";

/// The configuration set the account holds with its own sending switched off.
const PAUSED_SET: &str = "paused-set";

/// Recipients one message may address
/// (`service_com_amazonaws_sesv2_simpleemailservice_v2::state::send_validation::MAX_RECIPIENTS_PER_MESSAGE`).
const MAX_RECIPIENTS_PER_MESSAGE: usize = 50;

/// One answered request: the status, the error shape when there was one, and the parsed body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the response names, or an empty string on success.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .or_else(|| self.body.get("__type").and_then(Value::as_str))
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// The message the error carried.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `(Status, MessageId, Error)` of every `BulkEmailEntryResults` entry, in the order
    /// the response reported them. An absent member reads as the empty string.
    fn results(&self) -> Vec<(String, String, String)> {
        self.body
            .get("BulkEmailEntryResults")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        let member = |name: &str| {
                            row.get(name)
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string()
                        };
                        (member("Status"), member("MessageId"), member("Error"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The emulated service behind its router, plus the shared engine so a case can settle the
/// verification transition the clock owes an identity and read the outbound log back.
struct Ses {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Ses {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// One call, signed for `HOME`: the credential scope is what the handler reads the Region
    /// of the call from.
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://email.{HOME}.amazonaws.com{path}"))
            .header("host", format!("email.{HOME}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIAEMULATOR0000000/20260101/{HOME}/ses/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(payload)
            .expect("build request");
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

    /// `SendBulkEmail` with `body` as the whole request.
    async fn send_bulk(&self, body: Value) -> Reply {
        self.call("POST", "/v2/email/outbound-bulk-emails", Some(body))
            .await
    }

    /// `GetAccount`, which reports how much of the sending quota the window has charged.
    async fn account(&self) -> Reply {
        self.call("GET", "/v2/email/account", None).await
    }

    /// `CreateEmailIdentity`.
    async fn create_identity(&self, identity: &str) -> Reply {
        self.call(
            "POST",
            "/v2/email/identities",
            Some(json!({"EmailIdentity": identity})),
        )
        .await
    }

    /// `CreateEmailTemplate`.
    async fn create_template(&self) -> Reply {
        self.call(
            "POST",
            "/v2/email/templates",
            Some(json!({
                "TemplateName": TEMPLATE,
                "TemplateContent": {"Subject": TEMPLATE_SUBJECT, "Text": TEMPLATE_TEXT},
            })),
        )
        .await
    }

    /// `CreateConfigurationSet`.
    async fn create_configuration_set(&self, name: &str) -> Reply {
        self.call(
            "POST",
            "/v2/email/configuration-sets",
            Some(json!({"ConfigurationSetName": name})),
        )
        .await
    }

    /// `PutConfigurationSetSendingOptions`.
    async fn set_configuration_set_sending(&self, name: &str, enabled: bool) -> Reply {
        self.call(
            "PUT",
            &format!("/v2/email/configuration-sets/{name}/sending"),
            Some(json!({"SendingEnabled": enabled})),
        )
        .await
    }

    /// `PutAccountSendingAttributes`.
    async fn set_account_sending(&self, enabled: bool) -> Reply {
        self.call(
            "PUT",
            "/v2/email/account/sending",
            Some(json!({"SendingEnabled": enabled})),
        )
        .await
    }

    /// Bring every identity's verification transition into the past, so a case sending from
    /// an identity the account holds does not race the span the clock owes it.
    fn settle_identities(&self) {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let mut store = state.lock();
        let past = SystemTime::now()
            .checked_sub(Duration::from_secs(1))
            .expect("an instant before now");
        for record in store.identities.values_mut() {
            record.verification_due_at = past;
        }
        store.settle(SystemTime::now());
    }

    /// How many messages the outbound log holds: a settled call composed nothing, so it must
    /// append nothing.
    fn logged_messages(&self) -> usize {
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let store = state.lock();
        store.sent_messages.len()
    }
}

/// A service holding the verified sending domain and the stored template, plus the
/// configuration set whose sending is switched off.
async fn seeded() -> Ses {
    let ses = Ses::new();
    let identity = ses.create_identity(SENDER_DOMAIN).await;
    assert_eq!(
        identity.status, 200,
        "the sending domain must be created first, got {} {:?}",
        identity.status, identity.body
    );
    let template = ses.create_template().await;
    assert_eq!(
        template.status, 200,
        "the template must be created first, got {} {:?}",
        template.status, template.body
    );
    let created = ses.create_configuration_set(PAUSED_SET).await;
    assert_eq!(
        created.status, 200,
        "the configuration set must be created first, got {} {:?}",
        created.status, created.body
    );
    let paused = ses.set_configuration_set_sending(PAUSED_SET, false).await;
    assert_eq!(
        paused.status, 200,
        "the set's sending must be switched off, got {} {:?}",
        paused.status, paused.body
    );
    ses.settle_identities();
    ses
}

/// The state of the ACCOUNT a call is made against: what it would refuse a message for.
#[derive(Clone, Copy, Debug)]
enum Account {
    /// Everything a message needs: a verified sender and the stored template.
    AbleToSend,
    /// A sender whose identity the account has not verified.
    UnverifiedSender,
    /// A configuration set the account holds nothing under.
    AbsentConfigurationSet,
    /// A configuration set whose own sending switch is off.
    PausedConfigurationSet,
    /// The account's own sending switch, off.
    SendingPaused,
    /// A template name the account holds nothing under.
    AbsentTemplate,
    /// Every one of the above at once: an unverified sender, an absent set and an absent
    /// template.
    NothingResolves,
}

impl Account {
    /// The refusal a call that HAS a message to compose gets in this state, as
    /// `(status, error code, message)`, or `None` for the state that sends it.
    fn refusal(self) -> Option<(u16, &'static str, String)> {
        match self {
            Account::AbleToSend => None,
            Account::UnverifiedSender => Some((
                400,
                "MessageRejected",
                format!(
                    "Email address is not verified. The following identities failed the check \
                     in region {}: {UNVERIFIED}",
                    HOME.to_uppercase()
                ),
            )),
            // The configuration set the request names is resolved before the identity it
            // would send from, so a call that fails both is refused for the set.
            Account::AbsentConfigurationSet | Account::NothingResolves => Some((
                404,
                "NotFoundException",
                format!("Configuration set <{ABSENT_SET}> does not exist."),
            )),
            Account::PausedConfigurationSet => Some((
                400,
                "SendingPausedException",
                format!("Sending is paused for configuration set {PAUSED_SET}"),
            )),
            Account::SendingPaused => Some((
                400,
                "SendingPausedException",
                "The message can't be sent because the account's ability to send email is \
                 currently paused."
                    .to_string(),
            )),
            Account::AbsentTemplate => Some((
                404,
                "NotFoundException",
                format!("Template {ABSENT_TEMPLATE} does not exist."),
            )),
        }
    }

    /// A seeded service in this state, and the request members it implies.
    async fn service(self) -> Ses {
        let ses = seeded().await;
        if matches!(self, Account::SendingPaused) {
            let paused = ses.set_account_sending(false).await;
            assert_eq!(
                paused.status, 200,
                "case {self:?}: the account's sending must be switched off, got {} {:?}",
                paused.status, paused.body
            );
        }
        ses
    }

    /// The request this account state is exercised with: `entries` sent through the template
    /// and the sender this state names.
    fn request(self, entries: Vec<Value>) -> Value {
        let (sender, template_name) = match self {
            Account::UnverifiedSender => (UNVERIFIED, TEMPLATE),
            Account::AbsentTemplate => (SENDER, ABSENT_TEMPLATE),
            Account::NothingResolves => (UNVERIFIED, ABSENT_TEMPLATE),
            _ => (SENDER, TEMPLATE),
        };
        let mut body = json!({
            "FromEmailAddress": sender,
            "DefaultContent": {"Template": {"TemplateName": template_name, "TemplateData": DATA}},
            "BulkEmailEntries": entries,
        });
        match self {
            Account::AbsentConfigurationSet | Account::NothingResolves => {
                body["ConfigurationSetName"] = json!(ABSENT_SET);
            }
            Account::PausedConfigurationSet => {
                body["ConfigurationSetName"] = json!(PAUSED_SET);
            }
            _ => {}
        }
        body
    }
}

/// Every account state a call is weighed against.
const ACCOUNTS: &[Account] = &[
    Account::AbleToSend,
    Account::UnverifiedSender,
    Account::AbsentConfigurationSet,
    Account::PausedConfigurationSet,
    Account::SendingPaused,
    Account::AbsentTemplate,
    Account::NothingResolves,
];

/// One row of the entries table: entries that between them compose no message, and the
/// `(Status, Error)` each one's result must carry.
struct Entries {
    case: &'static str,
    /// The entries, as the request carries them.
    entries: fn() -> Vec<Value>,
    /// What each entry's result reports, in the order the request lists them.
    expected: fn() -> Vec<(&'static str, String)>,
}

/// The sentence a destination addressing nobody is reported with
/// (`service_com_amazonaws_sesv2_simpleemailservice_v2::state::send_validation::EMPTY_DESTINATION`).
const EMPTY_DESTINATION: &str = "Destination cannot be empty";

/// One `BulkEmailEntries` entry addressing `to`.
fn entry(to: &str) -> Value {
    json!({"Destination": {"ToAddresses": [to]}})
}

/// The rows: the entries differ along the SHAPE of the destination that addresses nobody, the
/// NUMBER of entries, and the KIND of per-entry defect — a destination with no recipient, a
/// recipient that names no mailbox, and more recipients than one message may address.
const ENTRY_ROWS: &[Entries] = &[
    Entries {
        case: "one entry whose destination has no member at all",
        entries: || vec![json!({"Destination": {}})],
        expected: || vec![("INVALID_PARAMETER", EMPTY_DESTINATION.to_string())],
    },
    Entries {
        case: "one entry whose three recipient lines are all empty",
        entries: || {
            vec![json!({
                "Destination": {"ToAddresses": [], "CcAddresses": [], "BccAddresses": []},
            })]
        },
        expected: || vec![("INVALID_PARAMETER", EMPTY_DESTINATION.to_string())],
    },
    Entries {
        case: "two entries addressing nobody, each shaped differently",
        entries: || {
            vec![
                json!({"Destination": {}}),
                json!({"Destination": {"CcAddresses": []}}),
            ]
        },
        expected: || {
            vec![
                ("INVALID_PARAMETER", EMPTY_DESTINATION.to_string()),
                ("INVALID_PARAMETER", EMPTY_DESTINATION.to_string()),
            ]
        },
    },
    Entries {
        case: "three entries addressing nobody",
        entries: || vec![json!({"Destination": {"BccAddresses": []}}); 3],
        expected: || vec![("INVALID_PARAMETER", EMPTY_DESTINATION.to_string()); 3],
    },
    Entries {
        case: "one entry whose recipient names no mailbox",
        entries: || vec![entry("not-an-address")],
        // `send_validation::address_defect`: a value carrying no `@` has no domain for the
        // mailbox it names.
        expected: || vec![("INVALID_PARAMETER", "Missing final '@domain'".to_string())],
    },
    Entries {
        case: "one entry addressing one recipient more than a message may carry",
        entries: || {
            let addresses: Vec<String> = (0..=MAX_RECIPIENTS_PER_MESSAGE)
                .map(|index| format!("reader-{index}@example.net"))
                .collect();
            vec![json!({"Destination": {"ToAddresses": addresses}})]
        },
        expected: || {
            vec![(
                "INVALID_PARAMETER",
                format!("Recipient count exceeds {MAX_RECIPIENTS_PER_MESSAGE}."),
            )]
        },
    },
    Entries {
        // What an entry states for its OWN message — the headers it adds, the tags its
        // events carry — is part of the request's form and refuses the whole call where
        // there IS a message to compose
        // (`tests/all/send_request_validation.rs`). For a call with no message it is not
        // weighed either: there is no message for those members to apply to, so the call is
        // settled on the destinations alone.
        case: "one entry that addresses nobody and states a header the service writes itself",
        entries: || {
            vec![json!({
                "Destination": {},
                "ReplacementHeaders": [{"Name": "From", "Value": "someone@example.net"}],
            })]
        },
        expected: || vec![("INVALID_PARAMETER", EMPTY_DESTINATION.to_string())],
    },
    Entries {
        case: "two entries addressing nobody, one stating a tag name no tag may take",
        entries: || {
            vec![
                json!({"Destination": {"ToAddresses": []}}),
                json!({
                    "Destination": {},
                    "ReplacementTags": [{"Name": "not a tag", "Value": "v"}],
                }),
            ]
        },
        expected: || {
            vec![
                ("INVALID_PARAMETER", EMPTY_DESTINATION.to_string()),
                ("INVALID_PARAMETER", EMPTY_DESTINATION.to_string()),
            ]
        },
    },
    Entries {
        case: "two entries failing for different reasons of their own",
        entries: || vec![json!({"Destination": {}}), entry("nobody@")],
        expected: || {
            vec![
                ("INVALID_PARAMETER", EMPTY_DESTINATION.to_string()),
                ("INVALID_PARAMETER", "Missing domain".to_string()),
            ]
        },
    },
];

#[tokio::test]
async fn a_call_with_no_message_to_compose_is_settled_on_its_entries() {
    for row in ENTRY_ROWS {
        for account in ACCOUNTS {
            let case = format!("{} against {account:?}", row.case);
            let ses = account.service().await;
            let reply = ses.send_bulk(account.request((row.entries)())).await;
            assert_eq!(
                reply.status,
                200,
                "case {case:?}: a call with no message to compose is settled on its entries, \
                 got {} {:?} ({})",
                reply.status,
                reply.body,
                reply.message()
            );
            let results = reply.results();
            let expected = (row.expected)();
            assert_eq!(
                results.len(),
                expected.len(),
                "case {case:?}: expected one result per entry, got {results:?}"
            );
            for (index, (status, error)) in expected.iter().enumerate() {
                assert_eq!(
                    (results[index].0.as_str(), results[index].2.as_str()),
                    (*status, error.as_str()),
                    "case {case:?}: expected entry {index} to carry what stopped it, got {:?}",
                    results[index]
                );
                assert!(
                    results[index].1.is_empty(),
                    "case {case:?}: a refused entry carries no MessageId, got {:?}",
                    results[index]
                );
            }
            // Nothing was composed, so nothing was logged and nothing was charged.
            assert_eq!(
                ses.logged_messages(),
                0,
                "case {case:?}: a settled call appends no message to the outbound log, got {}",
                ses.logged_messages()
            );
            let account_reply = ses.account().await;
            assert_eq!(
                account_reply.body.pointer("/SendQuota/SentLast24Hours"),
                Some(&json!(0.0)),
                "case {case:?}: a settled call charges nothing, got {:?}",
                account_reply.body.pointer("/SendQuota")
            );
        }
    }
}

#[tokio::test]
async fn a_call_that_has_a_message_to_compose_is_still_weighed_whole() {
    // The counter-case for every account state: ONE well-formed entry beside the defective
    // ones is a message to compose, so the gates on the REQUEST are weighed again and the
    // whole call is refused where the account cannot send it — and accepted, with the
    // defective entry reported as its own result, where it can.
    for account in ACCOUNTS {
        let ses = account.service().await;
        let entries = vec![
            json!({"Destination": {}}),
            entry(SIMULATOR),
            json!({"Destination": {"ToAddresses": []}}),
        ];
        let reply = ses.send_bulk(account.request(entries)).await;
        match account.refusal() {
            Some((status, code, message)) => {
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (status, code),
                    "case {account:?}: expected the call to be refused whole, got {} {:?}",
                    reply.status,
                    reply.body
                );
                assert_eq!(
                    reply.message(),
                    message,
                    "case {account:?}: expected the refusal {message:?}, got {:?}",
                    reply.message()
                );
                assert!(
                    reply.results().is_empty(),
                    "case {account:?}: a refused call reports no per-entry results, got {:?}",
                    reply.results()
                );
            }
            None => {
                assert_eq!(
                    reply.status,
                    200,
                    "case {account:?}: expected the send to be accepted, got {} {:?} ({})",
                    reply.status,
                    reply.body,
                    reply.message()
                );
                let results = reply.results();
                assert_eq!(
                    results.len(),
                    3,
                    "case {account:?}: expected one result per entry, got {results:?}"
                );
                assert_eq!(
                    results[1].0, "SUCCESS",
                    "case {account:?}: expected the well-formed entry to be sent, got \
                     {results:?}"
                );
                assert!(
                    !results[1].1.is_empty(),
                    "case {account:?}: expected the sent entry to answer a MessageId, got \
                     {results:?}"
                );
                for index in [0, 2] {
                    assert_eq!(
                        (results[index].0.as_str(), results[index].2.as_str()),
                        ("INVALID_PARAMETER", EMPTY_DESTINATION),
                        "case {account:?}: expected entry {index} to report its own defect, \
                         got {:?}",
                        results[index]
                    );
                }
            }
        }
    }
}
