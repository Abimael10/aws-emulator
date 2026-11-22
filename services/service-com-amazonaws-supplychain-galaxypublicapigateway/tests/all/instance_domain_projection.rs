// SPDX-License-Identifier: CC0-1.0

//! The `Instance` projection every instance operation answers from, driven over the wire
//! (restJson1, signing name `scn`) through the same `MultiServiceRouter` the emulator's
//! front door uses — so every assertion here is about what a real client observes, not
//! about a handler's internals.
//!
//! THE RULE UNDER TEST: an AWS Supply Chain instance HAS a WebApp domain label — the one
//! its create named, or one the service MINTED for a create that named none, which is a DNS
//! label of the member's own modelled form and never the instance's id — and every
//! operation whose output carries the `Instance` shape reports that label as
//! `webAppDnsDomain` beside the two hosts a client reaches the instance's web app
//! (`primaryDomain`) and its IAM Identity Center portal (`idcPortalDomain`) at, which are
//! the ADDRESS this emulator answers on — the one the request arrived on — and never a host
//! in a zone AWS publishes its own hosts in. The same projection reports `versionNumber`,
//! which is the instance's own version: it is identical across the five operations and no
//! mutation of the instance moves it.
//!
//! The table spans what the rule generalises over: the form of the domain label (a word, a
//! hyphenated label, digits, mixed case, both published length bounds) and whether the
//! create supplied one at all; and, per row, every operation that answers an `Instance`
//! (`CreateInstance`, `GetInstance`, `UpdateInstance`, `ListInstances`, `DeleteInstance`)
//! before and after a mutation. A second table spans the ADDRESSES a client reaches this
//! emulator at, which is what the announced hosts are read off.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The `InstanceState` a mutation of an instance is served in, and the one a readiness
/// poll converges on.
const INSTANCE_STATE_ACTIVE: &str = "Active";

/// How long a poll loop waits for the condition it is about before giving up, and how long
/// it waits between looks.
///
/// Generous on purpose: each loop below waits for a CONDITION the emulator has reached, so
/// the deadline is only there to end a case that can never satisfy it, and never a budget
/// the host has to beat.
const POLL_DEADLINE: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A SigV4 `Authorization` header whose credential scope names this service (`scn`), which
/// is how the router picks the service that claims the request. Signature verification is
/// switched off for the test router, so the signature itself is never read.
const SCN_CREDENTIAL: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/scn/aws4_request, SignedHeaders=host, Signature=00";

/// The address the requests of this suite reach the emulator at: the loopback socket a
/// client on this machine dials it on, carried as the `Host` header and as the authority of
/// the request target.
const REACHED_HOST: &str = "127.0.0.1:9297";

/// The host an instance's web app and Identity-Center portal are announced at for a request
/// that reached [`REACHED_HOST`], written out HERE rather than read from the crate: the
/// announced host is the requirement, so a drift in what the handler answers fails this
/// suite instead of travelling with it.
///
/// It is the address the request arrived on, because that is the one address a client
/// configured with this emulator can reach its web app at: no sub-name of it resolves, and a
/// host in AWS's own `on.aws` zone resolves off this machine entirely.
const ANNOUNCED_HOST: &str = REACHED_HOST;

/// The published length bounds of `InstanceWebAppDnsDomain`, whose `@pattern`
/// (`^(?![-])[a-zA-Z0-9-]{1,62}[a-zA-Z0-9]$`) admits 2 to 63 characters.
const LABEL_MIN_LENGTH: usize = 2;
const LABEL_MAX_LENGTH: usize = 63;

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
    /// The host every request of this emulator addresses, which is the address the hosts it
    /// announces are read off.
    host: String,
}

impl Emulator {
    fn new() -> Self {
        Self::reached_at(REACHED_HOST, None)
    }

    /// An emulator a client reaches at `host`, served on `served_base` (`None` for a router
    /// on no port at all, which reports the unserved base).
    fn reached_at(host: &str, served_base: Option<&str>) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Keep the test independent of how this process was launched: no authenticator,
        // so a request routes on its credential scope alone.
        router.set_request_authentication(false);
        if let Some(base) = served_base {
            router.set_url_base(
                smithy_server_runtime::EndpointUrl::parse(base)
                    .unwrap_or_else(|| panic!("{base} is not an endpoint URL")),
            );
        }
        router.register_service(EmulatedService::new(engine));
        Self {
            router,
            host: host.to_string(),
        }
    }

    /// Send one request and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path_and_query: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{}{path_and_query}", self.host))
            .header("host", &self.host)
            .header("authorization", SCN_CREDENTIAL)
            .header("content-type", "application/json")
            .body(body_bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                panic!(
                    "{method} {path_and_query}: body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, json)
    }

    /// One operation that answers an `Instance`, asserting the 200 and returning that
    /// member.
    async fn instance_call(
        &self,
        case: &str,
        what: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Value {
        let (status, answer) = self.send(method, path, body).await;
        assert_eq!(
            status, 200,
            "case {case}: {what} expected 200, got {status} with {answer}"
        );
        answer.get("instance").cloned().unwrap_or_else(|| {
            panic!("case {case}: {what} carries no `instance` member: {answer}")
        })
    }

    async fn create(&self, case: &str, request: Value) -> Value {
        self.instance_call(case, "CreateInstance", "POST", "/api/instance", Some(request))
            .await
    }

    async fn get(&self, case: &str, id: &str) -> Value {
        self.instance_call(
            case,
            "GetInstance",
            "GET",
            &format!("/api/instance/{id}"),
            None,
        )
        .await
    }

    async fn update(&self, case: &str, id: &str, patch: Value) -> Value {
        self.instance_call(
            case,
            "UpdateInstance",
            "PATCH",
            &format!("/api/instance/{id}"),
            Some(patch),
        )
        .await
    }

    async fn delete(&self, case: &str, id: &str) -> Value {
        self.instance_call(
            case,
            "DeleteInstance",
            "DELETE",
            &format!("/api/instance/{id}"),
            None,
        )
        .await
    }

    /// One `ListInstances` page, as the `instances` list it carries.
    async fn list(&self, case: &str, query: &str) -> Vec<Value> {
        let (status, body) = self.send("GET", &format!("/api/instance{query}"), None).await;
        assert_eq!(
            status, 200,
            "case {case}: ListInstances expected 200, got {status} with {body}"
        );
        body.get("instances")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("case {case}: the page carries no `instances` list: {body}"))
    }

    /// Look again until `id` reports `Active`, or until [`POLL_DEADLINE`].
    ///
    /// `CreateInstance` is asynchronous: the instance is `Initializing` until the clock
    /// owes it the transition, and a MUTATION of it is served only once it is `Active`. A
    /// case that goes on to mutate waits for that CONDITION rather than for a span.
    async fn wait_until_active(&self, case: &str, id: &str) {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let instance = self.get(case, id).await;
            if instance.get("state").and_then(Value::as_str) == Some(INSTANCE_STATE_ACTIVE) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case}: {id} never became Active, last read {instance}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Look again until the deletion of `id` has SETTLED — until `GetInstance` answers 404
    /// — or until [`POLL_DEADLINE`].
    ///
    /// `DeleteInstance` ACCEPTS a deletion: the instance moves to `Deleting` and stays
    /// addressable, holding its own domain label, while the deletion settles; only then
    /// does the record leave the store. A case about what the store holds AFTERWARDS waits
    /// for that condition.
    async fn wait_until_deleted(&self, case: &str, id: &str) {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let (status, body) = self
                .send("GET", &format!("/api/instance/{id}"), None)
                .await;
            if status == 404 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case}: the deletion of {id} never settled; GetInstance still answers \
                 {status} with {body}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// The `instanceId` of an `instance` payload.
fn id_of(case: &str, instance: &Value) -> String {
    instance
        .get("instanceId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: instance carries no `instanceId`: {instance}"))
        .to_string()
}

/// A member of an `instance` payload as a string.
fn member_of(case: &str, what: &str, instance: &Value, member: &str) -> String {
    instance
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case}: {what} reports no `{member}`: {instance}")
        })
        .to_string()
}

/// The `versionNumber` of an `instance` payload.
fn version_of(case: &str, what: &str, instance: &Value) -> f64 {
    instance
        .get("versionNumber")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case}: {what} reports no `versionNumber`: {instance}"))
}

/// Whether `label` satisfies `InstanceWebAppDnsDomain`'s modelled `@pattern`
/// (`^(?![-])[a-zA-Z0-9-]{1,62}[a-zA-Z0-9]$`), spelled out here rather than read from the
/// crate: 2 to 63 characters of ASCII alphanumerics and hyphens, not starting with a hyphen
/// and ending on an alphanumeric.
fn is_modelled_label(label: &str) -> bool {
    (LABEL_MIN_LENGTH..=LABEL_MAX_LENGTH).contains(&label.len())
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !label.starts_with('-')
        && label.ends_with(|c: char| c.is_ascii_alphanumeric())
}

/// Assert the whole domain set of one `instance` payload: the stored label, and the host
/// both announced members carry.
fn assert_domain_set_at(case: &str, what: &str, instance: &Value, label: &str, host: &str) {
    assert_eq!(
        member_of(case, what, instance, "webAppDnsDomain"),
        label,
        "case {case}: {what} must report the instance's domain label {label:?}, got {instance}"
    );
    for member in ["primaryDomain", "idcPortalDomain"] {
        assert_eq!(
            member_of(case, what, instance, member),
            host,
            "case {case}: {what} must report {member} as {host:?}, got {instance}"
        );
    }
}

/// [`assert_domain_set_at`] for the host the requests of this suite are announced at
/// ([`ANNOUNCED_HOST`]).
fn assert_domain_set(case: &str, what: &str, instance: &Value, label: &str) {
    assert_domain_set_at(case, what, instance, label, ANNOUNCED_HOST);
}

/// The DNS zones AWS publishes its OWN endpoints and short-form hosts in, written out here:
/// a hostname this emulator announces may never sit in one, because a client that resolves
/// such a name is sent off this machine (or at genuine AWS infrastructure) instead of back
/// to the emulator it is driving. `scn.global.on.aws`, the zone AWS's own WebApp hostnames
/// sit in, is under the last of these.
const AWS_ENDPOINT_ZONES: [&str; 4] = [".amazonaws.com", ".amazonaws.com.cn", ".api.aws", ".on.aws"];

/// RULE: an instance's announced hosts are where a client reaches this emulator, so they
/// follow the address the request arrived on and never sit in a zone AWS publishes its own
/// hosts in.
///
/// The rows span the addressings a client reaches this emulator through: a loopback socket,
/// the loopback name, an IPv6 loopback socket, and one of AWS's own endpoint hosts on an
/// emulator that is served at a name of its own and on one that is served on no port at all.
#[tokio::test]
async fn the_announced_hosts_name_the_address_the_request_reached_the_emulator_at() {
    // (case, the host the request addresses, the base the router is served at, the host the
    // instance's web app is announced at). A request that addressed this emulator's own
    // endpoint is answered that address; a request addressed to one of AWS's own hosts is
    // answered the address this emulator IS served at, which is `emulator.invalid` while it
    // is served on no port at all (`smithy_server_runtime::UNSERVED_URL_BASE`).
    let rows: [(&str, &str, Option<&str>, &str); 5] = [
        ("a loopback socket", "127.0.0.1:9297", None, "127.0.0.1:9297"),
        ("the loopback name", "localhost:44285", None, "localhost:44285"),
        ("an IPv6 loopback socket", "[::1]:9297", None, "[::1]:9297"),
        (
            "an AWS endpoint host on an emulator served at a name",
            "scn.us-east-1.amazonaws.com",
            Some("https://emulator.test:8443"),
            "emulator.test:8443",
        ),
        (
            "an AWS endpoint host on an emulator served on no port",
            "scn.us-east-1.amazonaws.com",
            None,
            "emulator.invalid",
        ),
    ];

    for (case, host, served_base, announced) in rows {
        let emulator = Emulator::reached_at(host, served_base);
        let label = "announced-domain";
        let created = emulator
            .create(case, json!({ "webAppDnsDomain": label }))
            .await;
        let id = id_of(case, &created);
        assert_domain_set_at(case, "CreateInstance", &created, label, announced);

        let read = emulator.get(case, &id).await;
        assert_domain_set_at(case, "GetInstance", &read, label, announced);

        let page = emulator.list(case, "?maxResults=20").await;
        let listed = page
            .iter()
            .find(|instance| id_of(case, instance) == id)
            .unwrap_or_else(|| panic!("case {case}: the created instance is not listed: {page:?}"));
        assert_domain_set_at(case, "ListInstances", listed, label, announced);

        // Whatever the addressing, an announced host carries an address and nothing else —
        // no scheme and no path — and never one of AWS's own endpoint zones.
        for member in ["primaryDomain", "idcPortalDomain"] {
            let reported = member_of(case, "CreateInstance", &created, member);
            assert!(
                !reported.contains('/'),
                "case {case}: {member} must be a bare address, got {reported:?}"
            );
            for aws_zone in AWS_ENDPOINT_ZONES {
                assert!(
                    !reported.ends_with(aws_zone),
                    "case {case}: {member} must not sit in AWS's own {aws_zone} zone, got \
                     {reported:?}"
                );
            }
        }
    }
}

/// RULE: a create that names no `webAppDnsDomain` is answered a MINTED DNS label — a value
/// of the member's own modelled form, independent of the instance's id and of every label
/// another instance holds — and the instance's hostnames are built from that label.
#[tokio::test]
async fn a_create_naming_no_label_is_answered_a_minted_label_of_its_own() {
    let emulator = Emulator::new();
    let mut minted: Vec<(String, String)> = Vec::new();
    // Several creates that name no label, plus one that names a label of its own beside
    // them: each minted label must stand on its own, and none may collide with a label an
    // instance already holds.
    let taken = "a-label-of-its-own";
    emulator
        .create("a create that names a label", json!({ "webAppDnsDomain": taken }))
        .await;
    for draw in 0..4 {
        let case = format!("create {draw} naming no label");
        let created = emulator.create(&case, json!({})).await;
        let id = id_of(&case, &created);
        let label = member_of(&case, "CreateInstance", &created, "webAppDnsDomain");
        assert!(
            is_modelled_label(&label),
            "case {case}: the minted label {label:?} does not satisfy the modelled pattern, \
             got {created}"
        );
        assert_ne!(
            label, id,
            "case {case}: the minted label must not be the instanceId, got {created}"
        );
        assert_ne!(
            label, taken,
            "case {case}: the minted label must not be a label another instance holds, got \
             {created}"
        );
        assert_domain_set(&case, "CreateInstance", &created, &label);

        // The minted label is the instance's, and a later read reports it unchanged.
        let read = emulator.get(&case, &id).await;
        assert_domain_set(&case, "GetInstance", &read, &label);
        minted.push((id, label));
    }

    let labels: std::collections::BTreeSet<&String> =
        minted.iter().map(|(_, label)| label).collect();
    assert_eq!(
        labels.len(),
        minted.len(),
        "case \"every minted label\": each create must be answered its own label, got \
         {minted:?}"
    );
}

/// RULE: every operation answering an `Instance` reports the instance's domain label and
/// the two hostnames derived from it, and reports one `versionNumber` that no mutation of
/// the instance moves.
#[tokio::test]
async fn every_instance_operation_reports_the_domain_set_and_a_stable_version() {
    let emulator = Emulator::new();
    let longest_label = format!("{}z", "l".repeat(LABEL_MAX_LENGTH - 1));
    let shortest_label = "a1".to_string();
    assert_eq!(
        (shortest_label.len(), longest_label.len()),
        (LABEL_MIN_LENGTH, LABEL_MAX_LENGTH),
        "the bound rows must sit exactly on the published bounds"
    );
    // One row per shape of label the model's pattern allows, plus the create that names
    // none and so is answered with a minted label.
    let rows: Vec<(&str, Option<String>)> = vec![
        ("the shortest label the model allows", Some(shortest_label)),
        ("a one-word label", Some("demo".to_string())),
        ("a hyphenated label", Some("demo-two-parts".to_string())),
        ("mixed case and digits", Some("MixedCase9".to_string())),
        ("the longest label the model allows", Some(longest_label)),
        ("no label at all", None),
    ];

    for (case, requested) in rows {
        let mut request = json!({
            "instanceName": "domain-projection",
            "instanceDescription": "the instance under test",
        });
        if let Some(label) = &requested {
            request["webAppDnsDomain"] = json!(label);
        }

        let created = emulator.create(case, request).await;
        let id = id_of(case, &created);
        let label = member_of(case, "CreateInstance", &created, "webAppDnsDomain");
        match &requested {
            // A create that named a label is answered with THAT label.
            Some(wanted) => assert_eq!(
                &label, wanted,
                "case {case}: the requested label must be echoed, got {created}"
            ),
            // A create that named none is answered with a minted one that still satisfies
            // the member's modelled pattern — never with no label at all, and never with
            // the instance's own id, which is a different identity of this API.
            None => {
                assert!(
                    is_modelled_label(&label),
                    "case {case}: the minted label {label:?} does not satisfy the modelled \
                     pattern, got {created}"
                );
                assert_ne!(
                    label, id,
                    "case {case}: the minted label must not be the instanceId, got {created}"
                );
            }
        }
        assert_domain_set(case, "CreateInstance", &created, &label);
        let version = version_of(case, "CreateInstance", &created);

        let read = emulator.get(case, &id).await;
        assert_domain_set(case, "GetInstance", &read, &label);
        assert_eq!(
            version_of(case, "GetInstance", &read),
            version,
            "case {case}: a read must report the version the create reported, got {read}"
        );

        // Two mutations, each changing a different member: neither moves the domain set
        // and neither moves the version. A mutation is served once the instance is
        // `Active`, so the case waits for the state the clock owes it first.
        emulator.wait_until_active(case, &id).await;
        let mutations: Vec<(&str, Value)> = vec![
            ("rename", json!({ "instanceName": "domain-projection-two" })),
            (
                "redescribe",
                json!({ "instanceDescription": "mutated twice" }),
            ),
        ];
        for (what, patch) in mutations {
            let updated = emulator.update(case, &id, patch).await;
            assert_domain_set(case, what, &updated, &label);
            assert_eq!(
                version_of(case, what, &updated),
                version,
                "case {case}: {what} must not move versionNumber, got {updated}"
            );

            let after = emulator.get(case, &id).await;
            assert_domain_set(case, "GetInstance after a mutation", &after, &label);
            assert_eq!(
                version_of(case, "GetInstance after a mutation", &after),
                version,
                "case {case}: the stored version must survive {what}, got {after}"
            );
        }

        // The listed element is the same projection the single-instance operations answer.
        let page = emulator.list(case, "?maxResults=20").await;
        let listed = page
            .iter()
            .find(|instance| id_of(case, instance) == id)
            .unwrap_or_else(|| panic!("case {case}: the created instance is not listed: {page:?}"));
        assert_domain_set(case, "ListInstances", listed, &label);
        assert_eq!(
            version_of(case, "ListInstances", listed),
            version,
            "case {case}: a listed element must report the same version, got {listed}"
        );

        // The terminal payload the delete echoes carries the domain set too.
        let deleted = emulator.delete(case, &id).await;
        assert_domain_set(case, "DeleteInstance", &deleted, &label);
        assert_eq!(
            version_of(case, "DeleteInstance", &deleted),
            version,
            "case {case}: the terminal payload must report the same version, got {deleted}"
        );
    }
}

/// RULE: the domain LABEL is each instance's own — instances living side by side report
/// different labels, an instance that later takes a freed label reports it, and the
/// announced hosts are the one address this emulator serves them all at.
#[tokio::test]
async fn each_instance_holds_its_own_label_at_the_one_announced_host() {
    let emulator = Emulator::new();
    // Three labels of different form, plus a create that names none so the store also
    // holds an instance whose label the service minted.
    let requested = ["sibling-one", "Sibling2", "s3"];
    let mut labels_by_id: Vec<(String, String)> = Vec::new();
    for label in requested {
        let created = emulator.create(label, json!({ "webAppDnsDomain": label })).await;
        labels_by_id.push((id_of(label, &created), label.to_string()));
    }
    let minted = emulator.create("no label at all", json!({})).await;
    let minted_id = id_of("no label at all", &minted);
    let minted_label = member_of("no label at all", "CreateInstance", &minted, "webAppDnsDomain");
    labels_by_id.push((minted_id, minted_label));

    let page = emulator.list("side by side", "?maxResults=20").await;
    assert_eq!(
        page.len(),
        labels_by_id.len(),
        "case side by side: every created instance must be listed, got {page:?}"
    );
    for (id, label) in &labels_by_id {
        let listed = page
            .iter()
            .find(|instance| &id_of("side by side", instance) == id)
            .unwrap_or_else(|| panic!("case side by side: {id} is not listed: {page:?}"));
        assert_domain_set("side by side", "ListInstances", listed, label);
    }
    // Each instance holds a label of its own...
    let labels: std::collections::BTreeSet<String> = page
        .iter()
        .map(|instance| member_of("side by side", "ListInstances", instance, "webAppDnsDomain"))
        .collect();
    assert_eq!(
        labels.len(),
        page.len(),
        "case side by side: each instance must report its own label, got {labels:?}"
    );
    // ...while the host they are announced at is the one address this emulator answers on,
    // because that is where it serves every instance's web app.
    let hosts: std::collections::BTreeSet<String> = page
        .iter()
        .map(|instance| member_of("side by side", "ListInstances", instance, "primaryDomain"))
        .collect();
    assert_eq!(
        hosts,
        std::collections::BTreeSet::from([ANNOUNCED_HOST.to_string()]),
        "case side by side: every instance must be announced at the address the request \
         reached, got {hosts:?}"
    );

    // The label of a deleted instance is free again once the deletion has settled, and
    // whoever takes it reports that label under a different instanceId.
    let (first_id, first_label) = labels_by_id[0].clone();
    emulator.delete("free the label", &first_id).await;
    emulator.wait_until_deleted("free the label", &first_id).await;
    let reused = emulator
        .create("reuse the label", json!({ "webAppDnsDomain": &first_label }))
        .await;
    assert_ne!(
        id_of("reuse the label", &reused),
        first_id,
        "case reuse the label: a reused label must belong to a NEW instance, got {reused}"
    );
    assert_domain_set("reuse the label", "CreateInstance", &reused, &first_label);
}

/// RULE: a `nextToken` names the POSITION the previous page stopped at, not a record, so
/// deleting the instance that page stopped on does not hide the instances behind it — the
/// resumed page still reports every remaining instance.
#[tokio::test]
async fn a_resumed_page_survives_the_deletion_of_the_instance_it_stopped_on() {
    let emulator = Emulator::new();
    let mut creation_order = Vec::new();
    for position in 0..4 {
        let case = format!("create {position}");
        let created = emulator
            .create(
                &case,
                json!({ "webAppDnsDomain": format!("resume-{position}") }),
            )
            .await;
        creation_order.push(id_of(&case, &created));
    }
    let newest_first: Vec<String> = creation_order.iter().rev().cloned().collect();

    // A page of one, so its token stops on the second-newest instance.
    let (status, body) = emulator.send("GET", "/api/instance?maxResults=1", None).await;
    assert_eq!(
        status, 200,
        "case first page: expected 200, got {status} with {body}"
    );
    let page = body
        .get("instances")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case first page: no `instances` list in {body}"));
    assert_eq!(
        page.iter()
            .map(|instance| id_of("first page", instance))
            .collect::<Vec<String>>(),
        vec![newest_first[0].clone()],
        "case first page: the newest instance must lead the page, got {page:?}"
    );
    let token = body
        .get("nextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case first page: a page with more behind it must carry a nextToken: {body}")
        })
        .to_string();

    let case = "delete the instance the token stopped on";
    emulator.delete(case, &newest_first[1]).await;
    // The record leaves the store when the accepted deletion settles, which is what the
    // resumed page below is about: a token naming a POSITION still resumes correctly.
    emulator.wait_until_deleted(case, &newest_first[1]).await;

    let resumed = emulator
        .list(
            "resumed page",
            &format!("?maxResults=20&nextToken={token}"),
        )
        .await;
    let ids: Vec<String> = resumed
        .iter()
        .map(|instance| id_of("resumed page", instance))
        .collect();
    assert_eq!(
        ids,
        newest_first[2..].to_vec(),
        "case resumed page: the instances behind the deleted one must still be reported, got \
         {ids:?}"
    );
}

/// RULE: `ListInstances` reports the most recently created instance first, at every page
/// size, and the order is a property of the instances rather than of the store's key — so
/// removing the newest promotes the next-newest and a fresh create leads the page again.
#[tokio::test]
async fn list_instances_reports_the_most_recently_created_instance_first() {
    let emulator = Emulator::new();
    let mut creation_order = Vec::new();
    for position in 0..5 {
        let case = format!("create {position}");
        let created = emulator
            .create(
                &case,
                json!({
                    "instanceName": format!("order-{position}"),
                    "webAppDnsDomain": format!("order-{position}"),
                }),
            )
            .await;
        creation_order.push(id_of(&case, &created));
    }
    let newest_first: Vec<String> = creation_order.iter().rev().cloned().collect();

    // Paging at any size yields that one order, every instance exactly once.
    for page_size in [1usize, 2, 3, 5, 20] {
        let case = format!("maxResults={page_size}");
        let mut collected: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match &token {
                Some(t) => format!("?maxResults={page_size}&nextToken={t}"),
                None => format!("?maxResults={page_size}"),
            };
            let (status, body) = emulator.send("GET", &format!("/api/instance{query}"), None).await;
            assert_eq!(
                status, 200,
                "case {case}: expected 200, got {status} with {body}"
            );
            let page = body
                .get("instances")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| panic!("case {case}: no `instances` list in {body}"));
            collected.extend(page.iter().map(|instance| id_of(&case, instance)));
            pages += 1;
            assert!(
                pages <= creation_order.len() + 1,
                "case {case}: paging did not terminate after {pages} pages"
            );
            match body.get("nextToken").and_then(Value::as_str) {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            collected, newest_first,
            "case {case}: a page must report the newest instance first"
        );
    }

    // Removing the newest promotes the one created before it, once the deletion settles.
    emulator.delete("delete the newest", &newest_first[0]).await;
    emulator
        .wait_until_deleted("delete the newest", &newest_first[0])
        .await;
    let page = emulator.list("after deleting the newest", "?maxResults=20").await;
    let ids: Vec<String> = page
        .iter()
        .map(|instance| id_of("after deleting the newest", instance))
        .collect();
    assert_eq!(
        ids,
        newest_first[1..].to_vec(),
        "case after deleting the newest: the next-newest instance must lead the page"
    );

    // ...and a fresh create leads the page again.
    let fresh = emulator
        .create(
            "a fresh create",
            json!({ "instanceName": "order-fresh", "webAppDnsDomain": "order-fresh" }),
        )
        .await;
    let fresh_id = id_of("a fresh create", &fresh);
    let page = emulator.list("after a fresh create", "?maxResults=20").await;
    assert_eq!(
        page.first().map(|instance| id_of("after a fresh create", instance)),
        Some(fresh_id),
        "case after a fresh create: the newest instance must lead the page, got {page:?}"
    );
}
