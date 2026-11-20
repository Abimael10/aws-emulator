// SPDX-License-Identifier: CC0-1.0

//! How Route 53 Resolver refuses a DNS Firewall domain-list change issued while
//! that list's last change is still running, across the one emulator.
//!
//! RULE under test: **a call that would change a DNS Firewall domain list whose
//! last change has not settled yet is refused with `ConflictException` at HTTP
//! 400, under the service's own code for that fault, in a sentence that names
//! the step the list stands in and quotes the `FirewallDomainListId` THE
//! REQUEST carried; and the refused call applies nothing — the change already
//! running still lands exactly its own domains, and once the list is idle a
//! further change is served.**
//!
//! Why it matters: a client keys its retry and backoff on the service code and
//! reads the list id out of the sentence to learn WHICH list is busy. A generic
//! state-transition sentence names no resource, so such retry logic never fires
//! and automation cannot tell an import still running apart from an unrelated
//! conflict.
//!
//! Two crates have to agree for the rule to hold — the domain file an import
//! reads is an object of the account's own Amazon S3 inventory, written through
//! that service's own `CreateBucket`/`PutObject`, while Route 53 Resolver owns
//! the domain list and refuses the overlapping call — so the suite lives here
//! rather than in either one. Every step goes over the wire each service
//! actually speaks: REST for Amazon S3, awsJson1_1 (`POST /` with
//! `X-Amz-Target: Route53Resolver.<OperationName>`) for Route 53 Resolver.
//!
//! The rows differ along what the rule generalises over: how many domains the
//! running import's file lists (one, a few hundred, a few thousand), which
//! addressing form of an Amazon S3 object the refused request names, and which
//! of several lists of one account the request addressed.
//!
//! The sentence a row expects is written out here rather than read from the
//! service: a check that imported the text it verifies would re-assert a
//! literal the implementation carries and would pass whatever that text became.
//! What each row computes from the case is the variable part alone — the step
//! and the id the request carried.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for, and the Region the bucket holding
/// the domain files is created in — "the file must be in an S3 bucket that's in
/// the same Region as your DNS Firewall"
/// (`ImportFirewallDomains$DomainFileUrl`).
const REGION: &str = "us-west-2";

/// The bucket every domain file of this suite is stored in.
const BUCKET: &str = "dns-firewall-import-conflict-files";

/// The service code Route 53 Resolver reports a refused overlapping domain-list
/// change under, as the recorded refusal of a second import words it.
const IN_PROGRESS_CODE: &str = "RSLVR-02305";

/// The `FirewallDomainListStatus` an import still running leaves on its list,
/// and the step a settled import leaves it in
/// (`smithy_query(query="shape",shape="FirewallDomainListStatus")`).
const IMPORTING: &str = "IMPORTING";
const COMPLETE: &str = "COMPLETE";

/// How long a look-again keeps looking for the outcome the clock owes an
/// accepted import. Deliberately generous: the span is the service's own, and
/// every wait below ends the moment the list reports a settled step rather than
/// after a fixed stretch of the clock.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again waits before asking the list again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many times a row attempts to issue its second call while the first
/// change is still running before it gives up.
///
/// A list is in flight only for the span the SERVICE gives its import, so on a
/// loaded host a second call can arrive after the first one has already landed
/// — which is a change of a SETTLED list and is served. Such a round proves
/// nothing either way and is discarded ([`Emulator::overlap_import`]), so no row
/// here depends on how fast the host runs.
const OVERLAP_ATTEMPTS: usize = 12;

/// The addressing forms of ONE Amazon S3 object a `DomainFileUrl` may name.
#[derive(Debug, Clone, Copy)]
enum Form {
    /// `s3://<bucket>/<key>`.
    S3Uri,
    /// `https://<bucket>.s3.<region>.amazonaws.com/<key>`.
    VirtualHosted,
    /// `https://s3.<region>.amazonaws.com/<bucket>/<key>`.
    PathStyle,
}

impl Form {
    /// How this form addresses `key` of [`BUCKET`].
    fn url(self, key: &str) -> String {
        match self {
            Self::S3Uri => format!("s3://{BUCKET}/{key}"),
            Self::VirtualHosted => format!("https://{BUCKET}.s3.{REGION}.amazonaws.com/{key}"),
            Self::PathStyle => format!("https://s3.{REGION}.amazonaws.com/{BUCKET}/{key}"),
        }
    }
}

/// The work each in-flight step of a domain list is carrying out, as the
/// sentence's remediation clause names it: the in-flight values of the model's
/// `FirewallDomainListStatus`
/// (`smithy_query(query="shape",shape="FirewallDomainListStatus")`) each beside
/// the change a caller has to wait out.
///
/// Every refusal of this suite is raised against a list that is IMPORTING,
/// because an import is the change a client can leave in flight over the wire;
/// the pairs for the other two steps are carried so the step a sentence names
/// cannot be paired with another step's work, not because a case here reaches
/// them.
const WORK_OF_STEP: &[(&str, &str)] = &[
    ("IMPORTING", "import"),
    ("UPDATING", "update"),
    ("DELETING", "delete"),
];

/// The two `FirewallDomainListStatus` values a list has REACHED rather than is
/// moving through: an import is over once the list reports either of them.
const SETTLED_STATUSES: &[&str] = &["COMPLETE", "COMPLETE_IMPORT_FAILED"];

/// The sentence DNS Firewall states when a call that would change the list `id`
/// arrives while that list stands in the in-flight `status`: fixed text under
/// the service's own code for the fault, whose variable parts are the list's
/// step, the work that step is carrying out and the id the request carried.
fn in_progress_message(id: &str, status: &str) -> String {
    let work = WORK_OF_STEP
        .iter()
        .find_map(|(step, work)| (*step == status).then_some(*work))
        .unwrap_or_else(|| panic!("{status:?} is an in-flight step of a domain list"));
    format!(
        "[{IN_PROGRESS_CODE}] The DNS Firewall domain list is in {step} status: \"{id}\". \
         Try again after the {work} completes.",
        step = status.to_lowercase()
    )
}

/// `count` distinct domain specifications under `label`, one per line, as the
/// text of a domain file: "the file must be a text file and must contain a
/// single domain per line".
fn domain_file(label: &str, count: usize) -> Vec<u8> {
    (0..count)
        .map(|index| format!("{label}-{index}.example.com\n"))
        .collect::<String>()
        .into_bytes()
}

/// The domains such a file makes a list hold, in the canonical rooted form every
/// reader of a domain list projects, ordered so two readings are compared as the
/// SETS they are: `ListFirewallDomains` models no order over "a list of the
/// domains in the firewall domain list", so what a row asserts is which domains
/// the list holds and not the sequence it reports them in.
fn expected_domains(label: &str, count: usize) -> Vec<String> {
    let mut domains: Vec<String> = (0..count)
        .map(|index| format!("{label}-{index}.example.com."))
        .collect();
    domains.sort();
    domains
}

/// Amazon S3 and Route 53 Resolver mounted on ONE engine, exactly as the router
/// mounts them, with request authentication off so each call carries the
/// credential scope that names its service and Region without a computed
/// signature.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_route53resolver_route53resolver::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// A credential scope naming `signing_name`, which is what routes the
    /// request and what each handler reads its Region from.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
             SignedHeaders=host, Signature=not-verified-in-this-test"
        )
    }

    /// One REST call of Amazon S3's own API, answering `(http status, body)`.
    async fn s3(&self, method: &str, path: &str, body: Vec<u8>) -> (u16, String) {
        let request = ::http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", Self::authorization("s3"))
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, String::from_utf8_lossy(response.body()).to_string())
    }

    /// Amazon S3 `CreateBucket` in [`REGION`], which is where the domain files
    /// an import reads have to be held.
    async fn create_bucket(&self) {
        let body = format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        );
        let (status, answered) = self
            .s3("PUT", &format!("/{BUCKET}"), body.into_bytes())
            .await;
        assert_eq!(
            status, 200,
            "setup: CreateBucket({BUCKET:?}) must succeed, got {status} with {answered:?}"
        );
    }

    /// Amazon S3 `PutObject`: the upload a client makes before it asks DNS
    /// Firewall to import the file.
    async fn put_object(&self, key: &str, body: Vec<u8>) {
        let bytes = body.len();
        let (status, answered) = self.s3("PUT", &format!("/{BUCKET}/{key}"), body).await;
        assert_eq!(
            status, 200,
            "setup: PutObject({key:?}, {bytes} byte(s)) must succeed, got {status} with \
             {answered:?}"
        );
    }

    /// One Route 53 Resolver operation over the awsJson1_1 wire its model
    /// declares, answering `(http status, parsed body)`.
    async fn resolver(&self, operation: &str, request: Value) -> (u16, Value) {
        let http_request = ::http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("route53resolver.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("Route53Resolver.{operation}"))
            .header("authorization", Self::authorization("route53resolver"))
            .body(Bytes::from(request.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(http_request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// The same call, asserting it was served.
    async fn served(&self, case: &str, operation: &str, request: Value) -> Value {
        let (status, answered) = self.resolver(operation, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must be served, got {status} with {answered}"
        );
        answered
    }

    /// `CreateFirewallDomainList`, answering the id DNS Firewall minted.
    async fn create_list(&self, case: &str, name: &str) -> String {
        let created = self
            .served(
                case,
                "CreateFirewallDomainList",
                json!({ "CreatorRequestId": format!("req-{name}"), "Name": name }),
            )
            .await;
        let id = created
            .get("FirewallDomainList")
            .and_then(|list| list.get("Id"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: the create answers an Id, got {created}"))
            .to_string();
        assert!(
            !id.is_empty(),
            "case {case:?}: the create must answer a non-empty Id, got {created}"
        );
        id
    }

    /// One `ImportFirewallDomains`, answering `(http status, parsed body)`.
    async fn import(&self, id: &str, url: &str) -> (u16, Value) {
        self.resolver(
            "ImportFirewallDomains",
            json!({
                "FirewallDomainListId": id,
                "Operation": "REPLACE",
                "DomainFileUrl": url,
            }),
        )
        .await
    }

    /// One `ImportFirewallDomains` that must be accepted and report the step an
    /// import still running leaves on its list.
    async fn start_import(&self, case: &str, id: &str, url: &str) {
        let (status, accepted) = self.import(id, url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the import of {url:?} must be accepted, got {status} with {accepted}"
        );
        assert_eq!(
            accepted.get("Status").and_then(Value::as_str),
            Some(IMPORTING),
            "case {case:?}: an accepted import is under way when it answers, got {accepted}"
        );
    }

    /// The `FirewallDomainList` a `GetFirewallDomainList` reports.
    async fn read_list(&self, case: &str, id: &str) -> Value {
        let read = self
            .served(
                case,
                "GetFirewallDomainList",
                json!({ "FirewallDomainListId": id }),
            )
            .await;
        read.get("FirewallDomainList")
            .unwrap_or_else(|| panic!("case {case:?}: the read answers a domain list, got {read}"))
            .clone()
    }

    /// Every domain the list holds, ordered so a row compares WHICH domains it
    /// holds rather than the sequence the listing happened to report.
    async fn list_domains(&self, case: &str, id: &str) -> Vec<String> {
        // `ListFirewallDomains$MaxResults` ranges 1..5000
        // (`smithy_query(query="shape",shape="ListDomainMaxResults")`), so one
        // page carries every domain a row of this suite imports.
        let listed = self
            .served(
                case,
                "ListFirewallDomains",
                json!({ "FirewallDomainListId": id, "MaxResults": 5_000 }),
            )
            .await;
        let mut domains: Vec<String> = listed
            .get("Domains")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: the listing answers Domains, got {listed}"))
            .iter()
            .map(|domain| {
                domain
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a domain is a string, got {domain}"))
                    .to_string()
            })
            .collect();
        domains.sort();
        domains
    }

    /// Start the import of `running_url` and issue the import of `refused_url`
    /// against the same list while that first import is still running,
    /// answering the body the overlapping call was refused with.
    ///
    /// A round in which the second call arrived too late to overlap anything is
    /// discarded rather than asserted on: the list is let settle and the
    /// overlap is attempted again, up to [`OVERLAP_ATTEMPTS`] times. Every
    /// attempt re-imports the SAME running file, so the list a caught round
    /// leaves behind is the one the row goes on to read.
    async fn overlap_import(
        &self,
        case: &str,
        id: &str,
        running_url: &str,
        refused_url: &str,
    ) -> Value {
        for attempt in 1..=OVERLAP_ATTEMPTS {
            self.start_import(case, id, running_url).await;
            let (status, answered) = self.import(id, refused_url).await;
            if status != 200 {
                assert_import_conflict(case, status, &answered, id);
                return answered;
            }
            // The running import had already landed when the second call
            // arrived, so that call is a change of a settled list and is
            // served: let it land too, and attempt the overlap again.
            self.poll_until_settled(case, id).await;
            assert!(
                attempt < OVERLAP_ATTEMPTS,
                "case {case:?}: after {OVERLAP_ATTEMPTS} attempts no second import reached the \
                 list while its first one was still running, so the refusal could not be \
                 exercised"
            );
        }
        unreachable!("each attempt either answers the refusal or asserts")
    }

    /// Read the list again until the import in flight has settled, and answer
    /// that projection: the CONDITION is waited for, never a span of the clock.
    async fn poll_until_settled(&self, case: &str, id: &str) -> Value {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let read = self.read_list(case, id).await;
            let status = read
                .get("Status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if status != IMPORTING {
                assert!(
                    SETTLED_STATUSES.contains(&status.as_str()),
                    "case {case:?}: a list that is no longer importing reports one of the \
                     settled steps {SETTLED_STATUSES:?}, it reports {status:?}: {read}"
                );
                return read;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the import must settle, the list still reports {status:?}: {read}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// Assert `refusal` is the coded conflict sentence naming `id` and the import
/// step.
fn assert_import_conflict(case: &str, status: u16, refusal: &Value, id: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: the overlapping import must be refused with HTTP 400, got {status} with \
         {refusal}"
    );
    let error_type = refusal
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        error_type.contains("ConflictException"),
        "case {case:?}: the refusal must name ConflictException, got {error_type:?} in {refusal}"
    );
    let message = refusal
        .get("Message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected = in_progress_message(id, IMPORTING);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the refusal to state {expected:?}, got {message:?}"
    );
}

/// A second `ImportFirewallDomains` issued while the first one is still running
/// is refused with the coded conflict sentence naming the list and its step
/// ([`Emulator::overlap_import`]), and applies nothing: the running import lands
/// exactly its own domains, and a further import against the now-idle list is
/// served and lands ITS domains.
///
/// The rows differ along how many domains the running import's file lists and
/// which addressing form of an Amazon S3 object the refused request named.
#[tokio::test]
async fn a_second_import_while_the_first_runs_is_refused_naming_the_list_and_its_step() {
    // (case, domains the running import's file lists, how the refused request
    // addressed its own file)
    let rows: &[(&str, usize, Form)] = &[
        ("a one-domain file and the s3 URI", 1, Form::S3Uri),
        (
            "a few hundred domains and the virtual-hosted URL",
            300,
            Form::VirtualHosted,
        ),
        (
            "a few thousand domains and the path-style URL",
            3_000,
            Form::PathStyle,
        ),
    ];

    for (index, &(case, running_count, form)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        emulator.create_bucket().await;

        let running_key = format!("running-{index}.txt");
        let refused_key = format!("refused-{index}.txt");
        let running_label = format!("running{index}");
        let refused_label = format!("refused{index}");
        emulator
            .put_object(&running_key, domain_file(&running_label, running_count))
            .await;
        emulator
            .put_object(&refused_key, domain_file(&refused_label, 2))
            .await;

        let id = emulator
            .create_list(case, &format!("import-conflict-{index}"))
            .await;
        emulator
            .overlap_import(
                case,
                &id,
                &format!("s3://{BUCKET}/{running_key}"),
                &form.url(&refused_key),
            )
            .await;

        // The refused call applied nothing: the import already running is the
        // one that lands, with exactly the domains ITS file lists.
        let settled = emulator.poll_until_settled(case, &id).await;
        assert_eq!(
            settled.get("Status").and_then(Value::as_str),
            Some(COMPLETE),
            "case {case:?}: the running import must still land, got {settled}"
        );
        assert_eq!(
            settled.get("DomainCount").and_then(Value::as_i64),
            Some(i64::try_from(running_count).expect("a count that fits")),
            "case {case:?}: expected DomainCount {running_count}, got {settled}"
        );
        let domains = emulator.list_domains(case, &id).await;
        assert_eq!(
            domains,
            expected_domains(&running_label, running_count),
            "case {case:?}: the list must hold exactly the running import's domains, it holds {} \
             domain(s) starting {:?}",
            domains.len(),
            domains.first()
        );

        // The list is idle again, so the change the conflict refused is served
        // when it is made again.
        emulator
            .start_import(case, &id, &form.url(&refused_key))
            .await;
        let after = emulator.poll_until_settled(case, &id).await;
        assert_eq!(
            after.get("Status").and_then(Value::as_str),
            Some(COMPLETE),
            "case {case:?}: an import against an idle list must land, got {after}"
        );
        let after_domains = emulator.list_domains(case, &id).await;
        assert_eq!(
            after_domains,
            expected_domains(&refused_label, 2),
            "case {case:?}: the later import must replace the list's domains, got \
             {after_domains:?}"
        );
    }
}

/// The id the sentence quotes is the one THE REQUEST carried: over several lists
/// of one account, each refusal names the list its own request addressed, and
/// each list still lands its own file.
///
/// Every refused request names ONE shared object, so the only thing that can
/// tell the refusals apart is which list the request addressed. The rows differ
/// along the file the refused list is importing and the addressing form its
/// refused request used.
#[tokio::test]
async fn each_refusal_quotes_the_list_its_own_request_named() {
    let emulator = Emulator::new();
    emulator.create_bucket().await;

    // (case, the label its running file's domains carry, how many it lists, the
    // form its refused request addressed its file with)
    let rows: &[(&str, &str, usize, Form)] = &[
        ("the first list", "first", 1, Form::S3Uri),
        ("the second list", "second", 4, Form::VirtualHosted),
        ("the third list", "third", 16, Form::PathStyle),
    ];

    // The one object every refused request names.
    let shared_key = "shared-refused.txt";
    emulator
        .put_object(shared_key, domain_file("shared", 2))
        .await;

    let mut busy: Vec<(&str, String, &str, usize)> = Vec::new();
    let mut stated: Vec<String> = Vec::new();
    for (index, &(case, label, count, form)) in rows.iter().enumerate() {
        let key = format!("busy-{label}.txt");
        emulator.put_object(&key, domain_file(label, count)).await;
        let id = emulator
            .create_list(case, &format!("busy-list-{index}"))
            .await;
        let refusal = emulator
            .overlap_import(
                case,
                &id,
                &format!("s3://{BUCKET}/{key}"),
                &form.url(shared_key),
            )
            .await;
        stated.push(
            refusal
                .get("Message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        );
        busy.push((case, id, label, count));
    }

    // No two refusals state the same sentence: each quotes its own list.
    for (position, first) in stated.iter().enumerate() {
        for second in stated.iter().skip(position + 1) {
            assert_ne!(
                first, second,
                "the refusals of two different busy lists must quote different ids, both state \
                 {first:?}"
            );
        }
    }

    // Each list still lands the file ITS own import named.
    for (case, id, label, count) in busy {
        let settled = emulator.poll_until_settled(case, &id).await;
        assert_eq!(
            settled.get("Status").and_then(Value::as_str),
            Some(COMPLETE),
            "case {case:?}: the running import must still land, got {settled}"
        );
        let domains = emulator.list_domains(case, &id).await;
        assert_eq!(
            domains,
            expected_domains(label, count),
            "case {case:?}: the list must hold exactly its own import's domains, got {domains:?}"
        );
    }
}
