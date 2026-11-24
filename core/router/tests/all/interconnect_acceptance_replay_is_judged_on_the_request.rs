// SPDX-License-Identifier: CC0-1.0

//! An `@idempotent` retry is judged against the REQUEST that was made, not against the
//! record that request wrote.
//!
//! The rule under test: **a repeat of an `@idempotent` operation that carries the same
//! caller-owned members as the call already made replays that call's outcome, whatever
//! other operations have written to the resource in between — so the judgement is made
//! against the request the call carried, never against the resource's current shape.**
//! `AcceptConnectionProposal` is modelled `@idempotent` with an `@idempotencyToken`
//! `clientToken` (`smithy_query(query="operation",operation="AcceptConnectionProposal")`
//! -> `"clientToken": {"target": "String", "idempotencyToken": true}, … "idempotent":
//! true`), and the two caller-owned members it writes onto the resulting Connection —
//! its `description` and its `tags` — are members other operations of the same service
//! go on to rewrite: `UpdateConnection` reassigns the description and the tag operations
//! rewrite the tag set. A retry judged against the record would therefore stop matching
//! the very acceptance that succeeded as soon as any of them had run.
//!
//! The rows differ along what the rule generalises over: WHICH later writer moved the
//! record (none, the update, the tag removal, the tag overwrite, and two of them
//! together), which of the acceptance's members that writer moved, whether the
//! acceptance supplied the optional description and the optional tag set at all, whether
//! it carried an idempotency token, the Region the connection lives in, the Environment
//! and bandwidth the proposal was made on, and the gateway the acceptance selected.
//!
//! This suite lives here, beside the other suites of the assembled emulator, because the
//! owning crate roots its integration tests at a hand-declared `tests/all.rs`, which a
//! fix may not edit; the router's `tests/all/` picks a new suite up with nothing to
//! declare. Every call goes through a router with Interconnect mounted beside the service
//! that owns the account's Direct Connect gateways on one engine, exactly as the
//! production router mounts them, so each assertion exercises the real wire path.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The signing name the Interconnect model gives the service, which is the credential
/// scope the router picks the claiming service by.
const SIGNING_NAME: &str = "interconnect";

/// The signing name and `awsJson1_1` target prefix of the service that owns the
/// account's Direct Connect gateways, one of which is what an attach point IS.
const DIRECT_CONNECT_SIGNING_NAME: &str = "directconnect";
const DIRECT_CONNECT_TARGET_PREFIX: &str = "OvertureService";

/// The Regions the rows sign their requests for: a connection belongs to the Region it
/// was created in, so the rule is checked across that axis too.
const REGIONS: [&str; 2] = ["us-east-1", "eu-west-1"];

/// "pending: The connection has been accepted and is being provisioned between AWS and
/// the Partner" — the state an accepted proposal's connection is reported in.
const ACCEPTED_STATE: &str = "pending";

/// "available: The connection has been fully provisioned between AWS and the Partner."
const PROVISIONED_STATE: &str = "available";

/// The `DirectConnectGatewayState` that service documents as ready for use, which is the
/// only state a gateway is an attach point in.
const GATEWAY_READY_STATE: &str = "available";

/// The `Environment$state` that says an Environment carries no connections, so no row is
/// built on one.
const UNAVAILABLE_ENVIRONMENT_STATE: &str = "unavailable";

/// An email address of the shape an Environment declaring the `email`
/// `RemoteAccountIdentifierType` asks a create for.
const PARTNER_EMAIL: &str = "interconnect-replay@example.test";

/// How long a wait for a state a service OWES runs before the case fails. Generous on
/// purpose: every span here is a stored due instant that nothing sleeps for, so what a
/// row waits for is the STATE and a loaded host must not be what decides it.
const LOOK_BUDGET: Duration = Duration::from_secs(60);

/// How often such a wait looks again.
const LOOK_INTERVAL: Duration = Duration::from_millis(5);

/// The gateways the rows attach to, differing in name and in `amazonSideAsn`.
const GATEWAY_ROWS: [(&str, Option<i64>); 2] = [
    ("ic-acceptance-replay-a", None),
    ("ic-acceptance-replay-b", Some(64_516)),
];

/// The tag key an acceptance of these rows asks for, and the key the tag writers below
/// remove or overwrite.
const ACCEPTANCE_TAG_KEY: &str = "owner";

/// The value a later `TagResource` writes over the acceptance's own.
const RETAGGED_VALUE: &str = "someone-else";

/// The wire error code this service answers its own client faults with.
const REFUSAL_CODE: &str = "InterconnectClientException";

/// What writes the record between an acceptance and the byte-for-byte retry of it.
///
/// Each variant is a writer of one of the two caller-owned members an acceptance sets:
/// `UpdateConnection` ("we support modifications to the connection's description and/or
/// bandwidth") reassigns the description, and the tag operations rewrite the tag set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterveningWrite {
    /// Nothing wrote the record: the baseline, where a replay must work as well.
    Nothing,
    /// `UpdateConnection` reassigns the description the acceptance set.
    Renamed,
    /// `UntagResource` removes a key the acceptance's tag set carried.
    Untagged,
    /// `TagResource` writes another value over a key the acceptance carried.
    Retagged,
    /// Both the rename and the tag removal.
    RenamedAndUntagged,
}

/// One row per writer, so every member an acceptance owns is covered by a row whose
/// later write moved it.
const WRITES: [InterveningWrite; 5] = [
    InterveningWrite::Nothing,
    InterveningWrite::Renamed,
    InterveningWrite::Untagged,
    InterveningWrite::Retagged,
    InterveningWrite::RenamedAndUntagged,
];

impl InterveningWrite {
    fn renames(self) -> bool {
        matches!(
            self,
            InterveningWrite::Renamed | InterveningWrite::RenamedAndUntagged
        )
    }

    fn untags(self) -> bool {
        matches!(
            self,
            InterveningWrite::Untagged | InterveningWrite::RenamedAndUntagged
        )
    }

    fn retags(self) -> bool {
        matches!(self, InterveningWrite::Retagged)
    }

    /// The description this row's acceptance supplies: left OUT on the row whose write
    /// rewrites the tag set, so the table spans an acceptance that omits the member.
    fn acceptance_description(self, seed: usize) -> Option<String> {
        match self {
            InterveningWrite::Untagged => None,
            _ => Some(format!("accepted proposal {seed}")),
        }
    }

    /// The tag set this row's acceptance asks for: none at all on the row whose write
    /// only renames, so the table spans an acceptance that omits the member; two entries
    /// on the row whose write removes one of them.
    fn acceptance_tags(self, seed: usize) -> BTreeMap<String, String> {
        match self {
            InterveningWrite::Renamed => BTreeMap::new(),
            InterveningWrite::Untagged => BTreeMap::from([
                (ACCEPTANCE_TAG_KEY.to_string(), format!("team-{seed}")),
                ("stage".to_string(), "accepted".to_string()),
            ]),
            _ => BTreeMap::from([(ACCEPTANCE_TAG_KEY.to_string(), format!("team-{seed}"))]),
        }
    }
}

/// One row of the table.
#[derive(Debug, Clone)]
struct Row {
    region: &'static str,
    environment: Value,
    bandwidth: String,
    /// The gateway the proposal's connection was created on.
    created_on: String,
    /// The gateway the acceptance selects.
    accept_on: String,
    /// The description the create gave the proposal, which is what the record holds when
    /// the acceptance supplies none of its own.
    proposal_description: String,
    /// The description the acceptance supplies, absent on one row.
    description: Option<String>,
    /// The tags the acceptance asks to associate with the resulting Connection.
    tags: BTreeMap<String, String>,
    /// The idempotency token the acceptance carries, absent on some rows: the member is
    /// optional, so a retry of a request that carried none is as much a retry.
    client_token: Option<String>,
    /// The description a later `UpdateConnection` reassigns on the rows that rename.
    renamed: String,
    write: InterveningWrite,
}

impl Row {
    fn environment_id(&self) -> String {
        text(&self.environment, "environmentId")
    }

    fn label(&self) -> String {
        format!(
            "{:?} after an acceptance in {} on {} / {} / created on {} accepted on {} / \
             description {:?} / tags {:?} / token {:?}",
            self.write,
            self.region,
            self.environment_id(),
            self.bandwidth,
            self.created_on,
            self.accept_on,
            self.description,
            self.tags,
            self.client_token
        )
    }

    /// The description the record holds once the acceptance has been made: the one the
    /// acceptance supplied, or the proposal's own where it supplied none.
    fn accepted_description(&self) -> String {
        self.description
            .clone()
            .unwrap_or_else(|| self.proposal_description.clone())
    }
}

/// A router with Interconnect and the service owning the account's Direct Connect
/// gateways mounted over ONE engine, as the assembled emulator mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The Authorization header below still carries the credential scope the router routes
    // by; the signature itself is not what this suite is about.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_interconnect_interconnect::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_directconnect_overtureservice::EmulatedService::new(engine),
    );
    router
}

/// `POST /` for one operation of `signing_name`, signed for `region`.
async fn post_in(
    router: &MultiServiceRouter,
    region: &str,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: Value,
) -> (u16, Value) {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date;x-amz-target, Signature=not-verified"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.localhost"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", target)
        .header("authorization", authorization)
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// One Interconnect operation (`awsJson1_0`) signed for `region`, as a `(status, body)`
/// pair.
async fn ic_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    post_in(
        router,
        region,
        SIGNING_NAME,
        &format!("Interconnect.{operation}"),
        "1.0",
        body,
    )
    .await
}

/// One Interconnect operation signed for `region`, required to succeed.
async fn ic_ok_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, decoded) = ic_in(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation:?} in {region:?}: expected HTTP 200, got {status} with {decoded:?}"
    );
    decoded
}

/// One Direct Connect operation (`awsJson1_1`), required to succeed.
async fn dx_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, decoded) = post_in(
        router,
        REGIONS[0],
        DIRECT_CONNECT_SIGNING_NAME,
        &format!("{DIRECT_CONNECT_TARGET_PREFIX}.{operation}"),
        "1.1",
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {operation:?}: expected HTTP 200, got {status} with {decoded:?}"
    );
    decoded
}

/// Create the gateways of [`GATEWAY_ROWS`] and look until that service reports each one
/// ready for use, answering their server-minted ids in the order created.
async fn ready_gateways(router: &MultiServiceRouter) -> Vec<String> {
    let mut ids = Vec::new();
    for (name, asn) in GATEWAY_ROWS {
        let mut request = Map::new();
        request.insert("directConnectGatewayName".to_string(), json!(name));
        if let Some(asn) = asn {
            request.insert("amazonSideAsn".to_string(), json!(asn));
        }
        let created = dx_ok(router, "CreateDirectConnectGateway", Value::Object(request)).await;
        ids.push(text(
            &created["directConnectGateway"],
            "directConnectGatewayId",
        ));
    }
    for id in &ids {
        let started = Instant::now();
        loop {
            let described = dx_ok(
                router,
                "DescribeDirectConnectGateways",
                json!({ "directConnectGatewayId": id }),
            )
            .await;
            let state = described["directConnectGateways"]
                .as_array()
                .and_then(|gateways| gateways.first().cloned())
                .map(|gateway| text(&gateway, "directConnectGatewayState"))
                .unwrap_or_default();
            if state == GATEWAY_READY_STATE {
                break;
            }
            assert!(
                started.elapsed() < LOOK_BUDGET,
                "setup {id:?}: expected the gateway to be ready for use within {LOOK_BUDGET:?}, \
                 it is still {state:?}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }
    ids
}

/// A string member, or `""` when absent — so a missing member fails an assertion rather
/// than panicking the suite.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// The wire error code of an error body.
fn code(body: &Value) -> String {
    text(body, "__type")
}

/// The `message` an error body carries, under either casing the envelope may use.
fn message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// `Environment$bandwidths.available` ("the list of currently available bandwidths") for
/// one listed Environment.
fn available_bandwidths(environment: &Value) -> Vec<String> {
    environment["bandwidths"]["available"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|bandwidth| bandwidth.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The tag set of a `Connection`, with absent and empty read as the same "no tags".
fn tags_of(connection: &Value) -> BTreeMap<String, String> {
    connection["tags"]
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The identifying information an Environment that declares a `remoteIdentifierType`
/// asks a create for, or nothing at all for one that declares none.
fn remote_account_for(environment: &Value) -> Option<Value> {
    match environment["remoteIdentifierType"].as_str()? {
        "account" => Some(json!({ "identifier": engine::EMULATOR_ACCOUNT_ID })),
        "email" => Some(json!({ "identifier": PARTNER_EMAIL })),
        _ => None,
    }
}

/// Every Environment the endpoint of `region` offers that a connection can be created on.
async fn creatable_environments(router: &MultiServiceRouter, region: &str) -> Vec<Value> {
    let listed = ic_ok_in(router, region, "ListEnvironments", json!({})).await;
    listed["environments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|environment| {
            text(environment, "state") != UNAVAILABLE_ENVIRONMENT_STATE
                && !available_bandwidths(environment).is_empty()
        })
        .collect()
}

/// The table: one row per intervening writer, each on a different Region, Environment,
/// bandwidth and gateway pair of the live catalog, and each carrying a different shape of
/// acceptance.
async fn table(router: &MultiServiceRouter, gateways: &[String]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (seed, write) in WRITES.into_iter().enumerate() {
        let region = REGIONS[seed % REGIONS.len()];
        let catalog = creatable_environments(router, region).await;
        assert!(
            !catalog.is_empty(),
            "setup {region:?}: expected the Region to publish an Environment a connection can be \
             created on, got none"
        );
        let environment = catalog[seed % catalog.len()].clone();
        let offered = available_bandwidths(&environment);
        let tags = write.acceptance_tags(seed);
        // A row whose write rewrites the acceptance's tag needs the acceptance to have
        // asked for that tag, or the row would prove nothing.
        assert!(
            !(write.untags() || write.retags()) || tags.contains_key(ACCEPTANCE_TAG_KEY),
            "setup {write:?}: a row whose write rewrites the acceptance's tag must carry it, got \
             {tags:?}"
        );
        rows.push(Row {
            region,
            bandwidth: offered[seed % offered.len()].clone(),
            environment,
            created_on: gateways[seed % gateways.len()].clone(),
            accept_on: gateways[(seed + 1) % gateways.len()].clone(),
            proposal_description: format!("proposal {seed}"),
            description: write.acceptance_description(seed),
            tags,
            client_token: if seed.is_multiple_of(2) {
                Some(format!("acceptance-token-{seed}"))
            } else {
                None
            },
            renamed: format!("renamed by update {seed}"),
            write,
        });
    }
    rows
}

/// `CreateConnection` for one row, answering the stored `Connection` — the proposal whose
/// activation key the acceptance below is made with.
///
/// The proposal carries no tags of its own, so the tag set the record holds after the
/// acceptance is exactly the one the acceptance asked for and the tag writers below move
/// what the acceptance wrote.
async fn create_proposal(router: &MultiServiceRouter, row: &Row) -> Value {
    let mut request = Map::new();
    request.insert("environmentId".to_string(), json!(row.environment_id()));
    request.insert("bandwidth".to_string(), json!(row.bandwidth));
    request.insert(
        "attachPoint".to_string(),
        json!({ "directConnectGateway": row.created_on }),
    );
    request.insert("description".to_string(), json!(row.proposal_description));
    if let Some(remote_account) = remote_account_for(&row.environment) {
        request.insert("remoteAccount".to_string(), remote_account);
    }
    let created = ic_ok_in(router, row.region, "CreateConnection", Value::Object(request)).await;
    created["connection"].clone()
}

/// An `AcceptConnectionProposal` body, handed back rather than posted so a row can send
/// the SAME request twice — which is what judging a repeat takes.
fn accept_request(
    row: &Row,
    activation_key: &str,
    attach_point: &Value,
    description: Option<&String>,
    tags: &BTreeMap<String, String>,
) -> Value {
    let mut request = Map::new();
    request.insert("activationKey".to_string(), json!(activation_key));
    request.insert("attachPoint".to_string(), attach_point.clone());
    if let Some(description) = description {
        request.insert("description".to_string(), json!(description));
    }
    if !tags.is_empty() {
        request.insert("tags".to_string(), json!(tags));
    }
    if let Some(token) = &row.client_token {
        request.insert("clientToken".to_string(), json!(token));
    }
    Value::Object(request)
}

/// The acceptance this row makes — the request whose byte-for-byte repeat is the retry
/// under test.
fn acceptance_of(row: &Row, activation_key: &str) -> Value {
    accept_request(
        row,
        activation_key,
        &json!({ "directConnectGateway": row.accept_on }),
        row.description.as_ref(),
        &row.tags,
    )
}

/// `GetConnection` in the row's Region, answering the stored `Connection`.
async fn read_back(router: &MultiServiceRouter, row: &Row, identifier: &str) -> Value {
    let fetched = ic_ok_in(
        router,
        row.region,
        "GetConnection",
        json!({ "identifier": identifier }),
    )
    .await;
    fetched["connection"].clone()
}

/// Look until the connection reports the fully provisioned state, answering the record it
/// reported it as. That is the state every write below is made from — and a terminal one,
/// so a later comparison of two readings is not racing the clock.
async fn wait_until_provisioned(
    router: &MultiServiceRouter,
    row: &Row,
    identifier: &str,
) -> Value {
    let case = row.label();
    let started = Instant::now();
    loop {
        let connection = read_back(router, row, identifier).await;
        let state = text(&connection, "state");
        if state == PROVISIONED_STATE {
            return connection;
        }
        assert!(
            started.elapsed() < LOOK_BUDGET,
            "case {case:?}: expected the accepted connection to reach {PROVISIONED_STATE:?} \
             within {LOOK_BUDGET:?}, it is still {state:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Make the row's proposal and accept it, then look until the clock has carried the
/// accepted connection to the fully provisioned state. Answers the activation key, the
/// identifier, the ARN, and the record as the acceptance left it.
async fn accepted_proposal(
    router: &MultiServiceRouter,
    row: &Row,
) -> (String, String, String, Value) {
    let case = row.label();
    let proposal = create_proposal(router, row).await;
    let activation_key = text(&proposal, "activationKey");
    let accepted = ic_ok_in(
        router,
        row.region,
        "AcceptConnectionProposal",
        acceptance_of(row, &activation_key),
    )
    .await["connection"]
        .clone();
    assert_eq!(
        text(&accepted, "state"),
        ACCEPTED_STATE,
        "case {case:?}: expected an accepted proposal's connection to report {ACCEPTED_STATE:?}, \
         got {:?}",
        accepted["state"]
    );
    let identifier = text(&accepted, "id");
    let arn = text(&accepted, "arn");
    let provisioned = wait_until_provisioned(router, row, &identifier).await;

    // What the acceptance asked for is what the record holds before anything else writes
    // it, so the writes below really do move the acceptance's own members.
    assert_eq!(
        text(&provisioned, "description"),
        row.accepted_description(),
        "case {case:?}: expected the record to hold the description the acceptance asked for, got \
         {:?}",
        provisioned["description"]
    );
    assert_eq!(
        tags_of(&provisioned),
        row.tags,
        "case {case:?}: expected the record to hold the tags the acceptance asked for ({:?}), got \
         {:?}",
        row.tags,
        tags_of(&provisioned)
    );
    (activation_key, identifier, arn, provisioned)
}

/// Apply the row's intervening write(s) to the accepted record, and check each one landed
/// — a row whose write did not move the record would prove nothing.
async fn apply_write(router: &MultiServiceRouter, row: &Row, identifier: &str, arn: &str) -> Value {
    let case = row.label();
    if row.write.renames() {
        ic_ok_in(
            router,
            row.region,
            "UpdateConnection",
            json!({ "identifier": identifier, "description": row.renamed }),
        )
        .await;
    }
    if row.write.untags() {
        ic_ok_in(
            router,
            row.region,
            "UntagResource",
            json!({ "arn": arn, "tagKeys": [ACCEPTANCE_TAG_KEY] }),
        )
        .await;
    }
    if row.write.retags() {
        ic_ok_in(
            router,
            row.region,
            "TagResource",
            json!({ "arn": arn, "tags": { ACCEPTANCE_TAG_KEY: RETAGGED_VALUE } }),
        )
        .await;
    }

    let moved = read_back(router, row, identifier).await;
    if row.write.renames() {
        assert_eq!(
            text(&moved, "description"),
            row.renamed,
            "case {case:?}: expected the update to reassign the description, got {:?}",
            moved["description"]
        );
    }
    if row.write.untags() {
        assert!(
            !tags_of(&moved).contains_key(ACCEPTANCE_TAG_KEY),
            "case {case:?}: expected the untag to remove {ACCEPTANCE_TAG_KEY:?}, got {:?}",
            tags_of(&moved)
        );
    }
    if row.write.retags() {
        assert_eq!(
            tags_of(&moved).get(ACCEPTANCE_TAG_KEY).map(String::as_str),
            Some(RETAGGED_VALUE),
            "case {case:?}: expected the tag write to overwrite {ACCEPTANCE_TAG_KEY:?}, got {:?}",
            tags_of(&moved)
        );
    }
    moved
}

/// RULE: a repeat of the `@idempotent` acceptance that succeeded carries the same
/// caller-owned members as that acceptance, so it replays the connection that acceptance
/// made — whatever the sibling writers of those same members (the update that reassigns a
/// description, the tag operations that rewrite a tag set) have written to the record
/// since. The replay reports the record as those writes left it and writes nothing
/// itself.
#[tokio::test]
async fn a_repeated_acceptance_replays_after_a_later_write_to_what_it_set() {
    let router = emulator();
    let gateways = ready_gateways(&router).await;
    let rows = table(&router, &gateways).await;
    assert_eq!(
        rows.len(),
        WRITES.len(),
        "case \"the replay table\": expected one row per intervening write, got {} rows",
        rows.len()
    );

    for row in &rows {
        let case = row.label();
        let (activation_key, identifier, arn, _) = accepted_proposal(&router, row).await;
        let moved = apply_write(&router, row, &identifier, &arn).await;

        // The byte-for-byte retry of the acceptance that already succeeded.
        let (status, answered) = ic_in(
            &router,
            row.region,
            "AcceptConnectionProposal",
            acceptance_of(row, &activation_key),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the acceptance that succeeded to replay after the write, got \
             {status} with {answered:?}"
        );
        let replayed = answered["connection"].clone();
        assert_eq!(
            replayed, moved,
            "case {case:?}: expected the replay to report the record as the later write left it \
             ({moved:?}), got {replayed:?}"
        );

        // A replay changes nothing: it neither re-associates the connection nor hands it
        // back to the clock, so the record after it is the record before it.
        let after = read_back(&router, row, &identifier).await;
        assert_eq!(
            after, moved,
            "case {case:?}: expected the replay to write nothing, got {after:?}"
        );
    }
}

/// RULE: the judgement is made against the request the acceptance CARRIED, so a retry
/// asking for what the record holds now — the request a judgement made against the live
/// record would read as a replay — is a different acceptance of a proposal already
/// accepted, and is refused as this service's own client fault, naming the connection the
/// acceptance made and never the `@sensitive` key, with nothing written.
#[tokio::test]
async fn a_retry_asking_for_what_the_record_now_holds_is_not_the_acceptance_that_was_made() {
    let router = emulator();
    let gateways = ready_gateways(&router).await;
    let rows: Vec<Row> = table(&router, &gateways)
        .await
        .into_iter()
        .filter(|row| row.write != InterveningWrite::Nothing)
        .collect();
    assert!(
        !rows.is_empty(),
        "case \"the refusal rows\": expected the table to hold rows whose write moved the record, \
         got none"
    );

    for row in &rows {
        let case = row.label();
        let (activation_key, identifier, arn, _) = accepted_proposal(&router, row).await;
        let moved = apply_write(&router, row, &identifier, &arn).await;

        // The rows differ in what the differing retry asks for: the shape the record has
        // been given since, and an attach point the acceptance never selected.
        let other_gateway = gateways
            .iter()
            .find(|gateway| **gateway != row.accept_on)
            .expect("the account holds more than one gateway")
            .clone();
        let differing: Vec<(&str, Value)> = vec![
            (
                "what the record holds now",
                accept_request(
                    row,
                    &activation_key,
                    &json!({ "directConnectGateway": row.accept_on }),
                    Some(&text(&moved, "description")),
                    &tags_of(&moved),
                ),
            ),
            (
                "another attach point",
                accept_request(
                    row,
                    &activation_key,
                    &json!({ "directConnectGateway": other_gateway }),
                    row.description.as_ref(),
                    &row.tags,
                ),
            ),
        ];

        for (shape, request) in differing {
            let probe = format!("{case} / {shape}");
            let (status, answered) = ic_in(&router, row.region, "AcceptConnectionProposal", request)
                .await;
            assert_eq!(
                status, 400,
                "case {probe:?}: expected a different acceptance of an accepted proposal to be \
                 refused, got {status} with {answered:?}"
            );
            assert!(
                code(&answered).contains(REFUSAL_CODE),
                "case {probe:?}: expected {REFUSAL_CODE}, got {:?}",
                code(&answered)
            );
            assert!(
                message(&answered).contains(&identifier)
                    && !message(&answered).contains(&activation_key),
                "case {probe:?}: expected the refusal to name the connection and not the \
                 sensitive key, got {:?}",
                message(&answered)
            );
            let after = read_back(&router, row, &identifier).await;
            assert_eq!(
                after, moved,
                "case {probe:?}: expected the refused acceptance to change nothing, got {after:?}"
            );
        }
    }
}
