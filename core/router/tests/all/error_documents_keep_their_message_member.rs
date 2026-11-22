// SPDX-License-Identifier: CC0-1.0

//! The ONE `restJson1` error document that keeps its MESSAGE member, shared by the three
//! services that each used to spell it.
//!
//! RULE under test: **an error document carries the error shape's own message member
//! whatever the service put in it — the message it supplied as a JSON string, or JSON
//! `null` where it supplied none — so a client can tell "no message was supplied" from
//! "an empty message was supplied".** An SDK substitutes the empty string for a message
//! key a JSON error document does not carry and reports "none" only for a key whose value
//! is `null`, so a dropped key states the opposite of what a message-less error class
//! means.
//!
//! The codec that implements it is `service-common`'s
//! (`services/common/src/error_message_member.rs`), held through
//! `Service::override_protocol`; Amazon SES v2, Amazon Polly and Amazon EFS each used to
//! carry their own copy of the same three types and now take that one. Amazon Polly's own
//! protocol wraps it, because that service applies a second rule — the error namespace its
//! `x-amzn-errortype` header is qualified with — to the answer the shared codec rendered.
//!
//! The rows drive the shared path through an operation of EACH of those services, over
//! what the rule generalises over: which service answered, which member spelling its model
//! declares the message under (`message` and `Message`), whether the class supplies a
//! message at all, the status the failure is rendered under (400, 404 and 409), and the
//! form of the identifier the request named. Every row is driven over the wire, through
//! `MultiServiceRouter::dispatch` with the three services mounted on one engine, so what
//! is asserted is what a signed AWS SDK request observes.
//!
//! Why it lives beside the router's suites rather than in one service crate: the invariant
//! spans four crates — the shared codec and the three services whose operations expose it
//! — and this is the test target that may observe all of them at once. Each service's own
//! suite (`error_document_message.rs` in the Polly and EFS crates,
//! `multi_region_endpoints.rs` in the SES v2 crate) keeps its own, per-operation cover.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// How long a poll loop waits before looking again. It bounds nothing: the loop runs
/// until its CONDITION holds or its own generous deadline passes.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A deadline far longer than any transition this suite waits on.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How a service's requests are addressed and signed: the host its endpoint prefix forms,
/// the SigV4 signing name the router routes on, and an access key id of the form the
/// service's own suites use.
struct Surface {
    endpoint_prefix: &'static str,
    signing_name: &'static str,
    access_key: &'static str,
}

/// Amazon SES v2 (`smithy_query(query="service_info")`: endpoint prefix `email`, signing
/// name `ses`).
const SES: Surface = Surface {
    endpoint_prefix: "email",
    signing_name: "ses",
    access_key: "AKIAEMULATOR0000000",
};

/// Amazon Polly.
const POLLY: Surface = Surface {
    endpoint_prefix: "polly",
    signing_name: "polly",
    access_key: "AKIDEXAMPLE",
};

/// Amazon EFS.
const EFS: Surface = Surface {
    endpoint_prefix: "elasticfilesystem",
    signing_name: "elasticfilesystem",
    access_key: "AKIDEXAMPLE",
};

/// What an error document's message member has to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The class supplies no message: the member is present and JSON `null`.
    Null,
    /// The class words a message from the request: the member is present and a non-empty
    /// string.
    Sentence,
}

/// One answered request, reduced to what the assertions read.
struct Reply {
    status: u16,
    /// `x-amzn-errortype`, the header `restJson1` resolves an error shape from.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the answer names, read off the header the protocol carries
    /// it in. Polly qualifies it with its error namespace and a `__type` may carry the
    /// shape's namespace, so the bare shape name is what is compared.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .or_else(|| self.body.get("__type").and_then(Value::as_str))
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// The value the document carries under `member`, or `None` when the document carries
    /// NO such key at all — which is the difference this suite is about, and which the
    /// usual `body["message"].as_str()` reader cannot see.
    fn member(&self, member: &str) -> Option<&Value> {
        self.body.get(member)
    }
}

/// The three services mounted on one shared engine, behind the router the production
/// entrypoint serves.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_polly_parrot_v1::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_efs_magnolioapiservice_v20150201::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One call to `surface`, signed for [`REGION`] — the credential scope is what the
    /// router routes on and what a handler reads the Region from.
    async fn call(
        &self,
        surface: &Surface,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Reply {
        let host = format!("{}.{REGION}.amazonaws.com", surface.endpoint_prefix);
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={}/20260101/{REGION}/{}/aws4_request, \
                     SignedHeaders=host, Signature=0",
                    surface.access_key, surface.signing_name
                ),
            );
        let payload = match &body {
            Some(value) => {
                builder = builder.header("content-type", "application/json");
                bytes::Bytes::from(serde_json::to_vec(value).expect("a serializable body"))
            }
            None => bytes::Bytes::new(),
        };
        let request = builder
            .body(payload)
            .expect("a well-formed test request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let raw = response.into_body();
        Reply {
            status,
            error_type,
            body: serde_json::from_slice(&raw).unwrap_or(Value::Null),
        }
    }

    /// One EFS file system in the `available` state, which is what `CreateAccessPoint`
    /// needs before it can mint an access point for a client token.
    ///
    /// The CONDITION is polled rather than a fixed span slept for, so this does not depend
    /// on how fast the host runs.
    async fn available_file_system(&self, token: &str) -> String {
        let created = self
            .call(
                &EFS,
                "POST",
                "/2015-02-01/file-systems",
                Some(json!({ "CreationToken": token })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "creating the file system {token:?}: expected 201, got {} {:?}",
            created.status, created.body
        );
        let id = created
            .member("FileSystemId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("creating the file system {token:?}: no FileSystemId in {:?}", created.body))
            .to_string();

        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let read = self
                .call(
                    &EFS,
                    "GET",
                    &format!("/2015-02-01/file-systems?FileSystemId={id}"),
                    None,
                )
                .await;
            if read.body["FileSystems"][0]["LifeCycleState"].as_str() == Some("available") {
                return id;
            }
            assert!(
                Instant::now() < deadline,
                "file system {id}: never reached available, last read {:?}",
                read.body
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// One row: the answer one request got, and what the rule requires of its message member.
struct Row {
    case: String,
    reply: Reply,
    status: u16,
    code: &'static str,
    /// The member name the answering service's model declares the message under.
    member: &'static str,
    expected: Expected,
}

#[tokio::test]
async fn every_services_error_document_carries_its_own_message_member() {
    let emulator = Emulator::new();
    let mut rows: Vec<Row> = Vec::new();

    // Amazon SES v2, whose model declares the member `message`: the multi-region-endpoint
    // read answers the class that supplies NO message, over several names the model's
    // `EndpointName` pattern (`^[\w\-_]+$`, length 1..64) admits and no call created.
    for name in ["absent-endpoint", "Absent_Endpoint_2", "e"] {
        rows.push(Row {
            case: format!("SES GetMultiRegionEndpoint({name:?})"),
            reply: emulator
                .call(
                    &SES,
                    "GET",
                    &format!("/v2/email/multi-region-endpoints/{name}"),
                    None,
                )
                .await,
            status: 404,
            code: "NotFoundException",
            member: "message",
            expected: Expected::Null,
        });
    }
    // ... and the same service's configuration-set read answers one class that DOES word a
    // message, so the null above is the class's rendering and not the service's.
    for name in ["absent-set", "A"] {
        rows.push(Row {
            case: format!("SES GetConfigurationSet({name:?})"),
            reply: emulator
                .call(
                    &SES,
                    "GET",
                    &format!("/v2/email/configuration-sets/{name}"),
                    None,
                )
                .await,
            status: 404,
            code: "NotFoundException",
            member: "message",
            expected: Expected::Sentence,
        });
    }

    // Amazon Polly, whose model also declares `message`, and whose protocol wraps the
    // shared codec: the task read answers the class that supplies no message, over the
    // minted identifier form and one the service does not mint.
    for id in [
        "6f3a1b2c-9d4e-4f5a-8b6c-7d8e9f0a1b2c",
        "00000000-0000-0000-0000-000000000000",
        "unknown-task-id",
    ] {
        rows.push(Row {
            case: format!("Polly GetSpeechSynthesisTask({id:?})"),
            reply: emulator
                .call(&POLLY, "GET", &format!("/v1/synthesisTasks/{id}"), None)
                .await,
            status: 400,
            code: "SynthesisTaskNotFoundException",
            member: "message",
            expected: Expected::Null,
        });
    }
    for name in ["Missing", "X"] {
        rows.push(Row {
            case: format!("Polly GetLexicon({name:?})"),
            reply: emulator
                .call(&POLLY, "GET", &format!("/v1/lexicons/{name}"), None)
                .await,
            status: 404,
            code: "LexiconNotFoundException",
            member: "message",
            expected: Expected::Sentence,
        });
    }

    // Amazon EFS, whose model declares the member `Message`: a repeated client token is
    // the class that supplies no message, over the token forms the model's `ClientToken`
    // admits (length 1..64).
    let file_system_id = emulator.available_file_system("shared-codec-probe").await;
    let tokens = vec![
        "t".to_string(),
        "shared-codec-access-point".to_string(),
        "x".repeat(64),
    ];
    for token in &tokens {
        let created = emulator
            .call(
                &EFS,
                "POST",
                "/2015-02-01/access-points",
                Some(json!({ "ClientToken": token, "FileSystemId": &file_system_id })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "creating the access point for {token:?}: expected 200, got {} {:?}",
            created.status, created.body
        );
        rows.push(Row {
            case: format!("EFS CreateAccessPoint({token:?}) repeated"),
            reply: emulator
                .call(
                    &EFS,
                    "POST",
                    "/2015-02-01/access-points",
                    Some(json!({ "ClientToken": token, "FileSystemId": &file_system_id })),
                )
                .await,
            status: 409,
            code: "AccessPointAlreadyExists",
            member: "Message",
            expected: Expected::Null,
        });
    }
    // ... and the same operation's not-found class, which DOES word a message, over the id
    // forms `^fs-[0-9a-f]{8,40}$` admits and no call created.
    for absent in ["fs-00000000", "fs-0123456789abcdef"] {
        rows.push(Row {
            case: format!("EFS CreateAccessPoint on {absent:?}"),
            reply: emulator
                .call(
                    &EFS,
                    "POST",
                    "/2015-02-01/access-points",
                    Some(json!({
                        "ClientToken": format!("absent-{absent}"),
                        "FileSystemId": absent,
                    })),
                )
                .await,
            status: 404,
            code: "FileSystemNotFound",
            member: "Message",
            expected: Expected::Sentence,
        });
    }

    // The table spans the three services, both member spellings and both halves of the
    // rule, so a row quietly dropped cannot leave it passing on a narrower class.
    let services: Vec<&str> = {
        let mut names: Vec<&str> = rows
            .iter()
            .map(|row| row.case.split_whitespace().next().unwrap_or_default())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    };
    assert_eq!(
        services,
        vec!["EFS", "Polly", "SES"],
        "the table must drive an operation of each service that took the shared codec, got \
         {services:?}"
    );
    for expected in [Expected::Null, Expected::Sentence] {
        for member in ["message", "Message"] {
            assert!(
                rows.iter()
                    .any(|row| row.expected == expected && row.member == member),
                "the table must cover {expected:?} under the member {member:?}"
            );
        }
    }

    for row in &rows {
        let Row {
            case,
            reply,
            status,
            code,
            member,
            expected,
        } = row;
        assert_eq!(
            (reply.status, reply.error_code()),
            (*status, (*code).to_string()),
            "case {case:?}: expected a {status} {code}, got {} {:?}",
            reply.status,
            reply.body
        );
        let Some(value) = reply.member(member) else {
            panic!(
                "case {case:?}: the error document omits the {member:?} key entirely, got {:?}",
                reply.body
            )
        };
        match expected {
            Expected::Null => assert!(
                value.is_null(),
                "case {case:?}: this class supplies no message, so the member must be JSON \
                 null, got {value:?} in {:?}",
                reply.body
            ),
            Expected::Sentence => {
                let text = value.as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: expected a string message member, got {value:?}")
                });
                assert!(
                    !text.is_empty(),
                    "case {case:?}: this class words a message from the request, so the member \
                     must carry a non-empty sentence, got {text:?}"
                );
            }
        }
    }
}
