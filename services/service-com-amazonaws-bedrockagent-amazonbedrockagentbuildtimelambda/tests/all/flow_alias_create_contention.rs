// SPDX-License-Identifier: CC0-1.0

//! `CreateFlowAlias` under CONTENTION, end to end over HTTP and read back
//! through `GetFlowAlias` / `ListFlowAliases`.
//!
//! One rule, in three shapes:
//!
//! * an alias name is held by at most ONE alias of a flow, so however many
//!   callers ask for the same name at the same time, exactly one is accepted
//!   (201) and every other is refused with the modelled `ConflictException`
//!   (409) naming the name asked for and the id of the alias that holds it —
//!   and the flow afterwards holds exactly one alias, the winner's, whole;
//! * names that differ are not in each other's way, so concurrent creates under
//!   distinct names each mint their own alias;
//! * `clientToken` is an `@idempotencyToken`, so concurrent creates carrying one
//!   token mint ONE alias and every caller reports that same alias.
//!
//! The tables below differ in how many callers race — the recorded race has
//! five — and in the form of the name they race for, spanning what the modelled
//! `Name` (`pattern ^([0-9a-zA-Z][_-]?){1,100}$`) admits: a single character, a
//! name mixing both separators it allows, and the widest name it admits.
//!
//! Every assertion here goes through the router, so it exercises the real wire
//! path (route matching, model constraint validation, serialization) rather than
//! calling a handler directly.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::service_state;
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the
/// routing key this service claims on, and the source of the region its ARNs
/// carry.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A role ARN matching the modelled `FlowExecutionRoleArn` pattern
/// `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/(service-role/)?.+$`.
const EXECUTION_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForFlows";

/// The reserved alias id every flow carries an alias under, as the modelled
/// `FlowAliasId` (`pattern ^(TSTALIASID|[0-9a-zA-Z]{10})$`) admits it.
const TEST_ALIAS_ID: &str = "TSTALIASID";

/// How many numbered versions the flow every row below races against holds, so a
/// racer can route to a different version from its neighbour and still name one
/// the flow holds.
const VERSIONS_CUT: u32 = 2;

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's
    /// `__type`; `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    fn member(&self, path: &[&str]) -> &Value {
        let mut at = &self.body;
        for key in path {
            at = &at[*key];
        }
        at
    }

    fn string(&self, path: &[&str]) -> String {
        self.member(path)
            .as_str()
            .unwrap_or_else(|| panic!("{path:?} is not a string in {:?}", self.body))
            .to_string()
    }

    /// The `(name, id)` of every reported CALLER-created alias summary, in the
    /// order the response listed them: the flow's reserved test alias, which the
    /// service creates with the flow, is in no caller's race for a name, so
    /// these rows leave it out.
    fn listed(&self) -> Vec<(String, String)> {
        self.member(&["flowAliasSummaries"])
            .as_array()
            .unwrap_or_else(|| panic!("flowAliasSummaries is not an array in {:?}", self.body))
            .iter()
            .filter(|summary| summary["id"].as_str() != Some(TEST_ALIAS_ID))
            .map(|summary| {
                let member = |name: &str| {
                    summary[name]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("a summary carries no {name}: {summary:?}")
                        })
                        .to_string()
                };
                (member("name"), member("id"))
            })
            .collect()
    }
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
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

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-agent.eu-west-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|e| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        let error_type = header_error.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
        Answer {
            status,
            error_type,
            body,
        }
    }

    /// A flow prepared and cut into [`VERSIONS_CUT`] numbered versions — the
    /// state an alias is created against — and its `flowId`.
    async fn flow_with_versions(&self, case: &str) -> String {
        let created = self
            .call(
                "POST",
                "/flows/",
                Some(json!({
                    "name": "aliasContentionFlow",
                    "executionRoleArn": EXECUTION_ROLE,
                    "definition": passthrough_definition(),
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateFlow -> 201, got {created:?}"
        );
        let flow_id = created.string(&["id"]);
        self.prepare_and_settle(case, &flow_id).await;
        for expected in 1..=VERSIONS_CUT {
            let cut = self
                .call("POST", &format!("/flows/{flow_id}/versions"), Some(json!({})))
                .await;
            assert_eq!(
                cut.status, 201,
                "case {case}: expected CreateFlowVersion -> 201, got {cut:?}"
            );
            assert_eq!(
                cut.string(&["version"]),
                expected.to_string(),
                "case {case}: expected version {expected}, got {cut:?}"
            );
        }
        flow_id
    }

    /// `PrepareFlow`, with the outcome the clock owes moved into the past so the
    /// flow stands at `Prepared` without the test waiting for the span.
    async fn prepare_and_settle(&self, case: &str, flow_id: &str) {
        let prepared = self
            .call("POST", &format!("/flows/{flow_id}/"), Some(json!({})))
            .await;
        assert_eq!(
            prepared.status, 202,
            "case {case}: expected PrepareFlow -> 202, got {prepared:?}"
        );
        let state = service_state(&*self.engine).expect("the bedrock-agent store");
        let mut state = state.lock();
        let pending = state
            .flows
            .get_mut(flow_id)
            .expect("the stored flow")
            .pending_preparation
            .as_mut()
            .unwrap_or_else(|| panic!("case {case}: expected a preparation in flight"));
        pending.due = SystemTime::now() - Duration::from_secs(1);
    }

    async fn create_flow_alias(&self, flow_id: &str, request: Value) -> Answer {
        self.call("POST", &format!("/flows/{flow_id}/aliases"), Some(request))
            .await
    }

    async fn get_flow_alias(&self, flow_id: &str, alias_id: &str) -> Answer {
        self.call("GET", &format!("/flows/{flow_id}/aliases/{alias_id}"), None)
            .await
    }

    async fn list_flow_aliases(&self, flow_id: &str) -> Answer {
        self.call("GET", &format!("/flows/{flow_id}/aliases"), None)
            .await
    }
}

/// A passthrough flow definition: an `Input` node wired straight to an `Output`
/// node. The node types, the union keys and the IO data types are the modelled
/// `FlowNodeType` / `FlowNodeConfiguration` / `FlowNodeIODataType` values.
fn passthrough_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "String" }],
            },
            {
                "name": "FlowOutput",
                "type": "Output",
                "configuration": { "output": {} },
                "inputs": [{ "name": "document", "type": "String", "expression": "$.data" }],
            },
        ],
        "connections": [
            {
                "name": "InputToOutput",
                "type": "Data",
                "source": "FlowInput",
                "target": "FlowOutput",
                "configuration": { "data": { "sourceOutput": "document", "targetInput": "document" } },
            },
        ],
    })
}

/// The refusal a create answers when the alias name it asks for is one an alias
/// of the same flow already holds. The fixed text is this error class's; the
/// operation, the name asked for and the id of the alias holding it are its only
/// variable parts.
fn name_conflict_message(operation: &str, name: &str, alias_id: &str) -> String {
    format!(
        "Couldn't perform {operation} operation. The name {name} already exists for id \
         {alias_id}. Retry your request with a different name."
    )
}

/// The most groups the modelled `Name` (`pattern ^([0-9a-zA-Z][_-]?){1,100}$`)
/// admits. A group is one alphanumeric character and an optional separator, so a
/// name of this many CHARACTERS is always inside the pattern.
const NAME_MAX_GROUPS: usize = 100;

/// The widest alias name the modelled `Name` admits: [`NAME_MAX_GROUPS`] groups
/// of one character each.
fn widest_name() -> String {
    "w".repeat(NAME_MAX_GROUPS)
}

/// One row: how many callers race, and the name they race for.
struct Row {
    case: &'static str,
    racers: usize,
    name: String,
}

impl Row {
    /// The name caller `index` asks for when the callers are NOT contending:
    /// this row's own name form with the caller's number appended, and the base
    /// trimmed so the result still fits the modelled `Name` — so the widest row
    /// races at the widest name there too.
    fn distinct_name(&self, index: usize) -> String {
        let suffix = index.to_string();
        let base: String = self
            .name
            .chars()
            .take(NAME_MAX_GROUPS - suffix.len())
            .collect();
        format!("{base}{suffix}")
    }
}

/// Rows spanning the two things the rule generalises over: the NUMBER of callers
/// racing (two, the smallest race there is; five, as many as the recorded race;
/// and eight) and the FORM of the name they race for, over what the modelled
/// `Name` admits.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "two callers, a single-character name",
            racers: 2,
            name: "A".to_string(),
        },
        Row {
            case: "three callers, a name mixing both separators",
            racers: 3,
            name: "blue-green_alias".to_string(),
        },
        Row {
            case: "five callers, as many as the recorded race",
            racers: 5,
            name: "prod".to_string(),
        },
        Row {
            case: "eight callers, the widest name there is",
            racers: 8,
            name: widest_name(),
        },
    ]
}

/// One racer's `CreateFlowAlias` request: the members that must come back
/// together once it has won.
struct Racer {
    description: String,
    flow_version: String,
    client_token: String,
}

impl Racer {
    /// `racers` racers, each identifiable by its own description and routing
    /// target, and each carrying its OWN `clientToken` — the shape an SDK gives
    /// a burst of independent calls, since `clientToken` is an
    /// `@idempotencyToken` the client fills per request.
    fn burst(racers: usize) -> Vec<Self> {
        // `DRAFT` is the version every flow holds and `1..=VERSIONS_CUT` are the
        // numbers cut on the flow these racers run against, so every racer names
        // a version the flow holds whatever [`VERSIONS_CUT`] is.
        let mut targets = vec!["DRAFT".to_string()];
        targets.extend((1..=VERSIONS_CUT).map(|version| version.to_string()));
        (0..racers)
            .map(|index| Self {
                description: format!("the alias asked for by caller {index}"),
                flow_version: targets[index % targets.len()].clone(),
                // Inside the modelled `ClientToken` (len 33..256,
                // `pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`).
                client_token: format!("concurrent-alias-client-token-{index:04}"),
            })
            .collect()
    }

    fn request(&self, name: &str) -> Value {
        json!({
            "name": name,
            "description": self.description,
            "routingConfiguration": [{ "flowVersion": self.flow_version }],
            "clientToken": self.client_token,
        })
    }
}

/// The rule, first shape: an alias name belongs to at most one alias of a flow,
/// so a burst of creates under one name leaves exactly one winner (201) and
/// refuses every other caller with the modelled `ConflictException` (409) naming
/// the name asked for and the id already holding it — and the flow afterwards
/// holds exactly that one alias, carrying the winner's members whole.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_under_one_name_mint_exactly_one_alias() {
    for row in rows() {
        let case = row.case;
        let emulator = Arc::new(Emulator::new());
        let flow_id = emulator.flow_with_versions(case).await;

        let racers = Racer::burst(row.racers);
        let mut running = Vec::new();
        for racer in &racers {
            let emulator = Arc::clone(&emulator);
            let flow_id = flow_id.clone();
            let request = racer.request(&row.name);
            running.push(tokio::spawn(async move {
                emulator.create_flow_alias(&flow_id, request).await
            }));
        }

        let mut answers = Vec::new();
        for (index, handle) in running.into_iter().enumerate() {
            answers.push((
                index,
                handle.await.expect("the racing create ran to completion"),
            ));
        }

        let winners: Vec<&(usize, Answer)> = answers
            .iter()
            .filter(|(_, answer)| answer.status == 201)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "case {case}: exactly one create may win the name, {} did: {answers:?}",
            winners.len()
        );
        let (winner_index, winner) = winners[0];
        let winner_index = *winner_index;
        let alias_id = winner.string(&["id"]);

        for (index, answer) in answers.iter().filter(|(index, _)| *index != winner_index) {
            assert_eq!(
                answer.status, 409,
                "case {case}, caller {index}: expected ConflictException's 409, got {answer:?}"
            );
            assert_eq!(
                answer.error_type.as_deref(),
                Some("ConflictException"),
                "case {case}, caller {index}: expected ConflictException, got {answer:?}"
            );
            assert_eq!(
                answer.member(&["message"]),
                &json!(name_conflict_message("CreateFlowAlias", &row.name, &alias_id)),
                "case {case}, caller {index}: expected the conflict to name the name asked \
                 for and the alias holding it, got {answer:?}"
            );
        }

        // The winner's alias is readable back whole: the refused callers wrote
        // nothing, neither their description nor their routing target.
        let winner_racer = &racers[winner_index];
        let read = emulator.get_flow_alias(&flow_id, &alias_id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetFlowAlias -> 200, got {read:?}"
        );
        assert_eq!(
            read.string(&["name"]),
            row.name,
            "case {case}: expected the stored alias to carry the name raced for, got {read:?}"
        );
        assert_eq!(
            read.member(&["description"]),
            &json!(winner_racer.description),
            "case {case}: expected the winner's description, got {read:?}"
        );
        assert_eq!(
            read.member(&["routingConfiguration"]),
            &json!([{ "flowVersion": winner_racer.flow_version }]),
            "case {case}: expected the winner's routing configuration, got {read:?}"
        );
        assert_eq!(
            read.string(&["flowId"]),
            flow_id,
            "case {case}: expected the alias to belong to the flow it was created on, \
             got {read:?}"
        );

        let listed = emulator.list_flow_aliases(&flow_id).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListFlowAliases -> 200, got {listed:?}"
        );
        assert_eq!(
            listed.listed(),
            vec![(row.name.clone(), alias_id.clone())],
            "case {case}: expected the flow to hold exactly the one alias the race minted, \
             got {listed:?}"
        );
    }
}

/// The same rule read the other way: the name is what two creates contend for,
/// so names that DIFFER are not in each other's way — a burst of creates under
/// distinct names mints one alias each, every one with its own id, and the flow
/// reports them all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_under_distinct_names_each_mint_their_own_alias() {
    for row in rows() {
        let case = row.case;
        let emulator = Arc::new(Emulator::new());
        let flow_id = emulator.flow_with_versions(case).await;

        let racers = Racer::burst(row.racers);
        // One name per caller: the row's own name form carrying the caller's
        // number, so the names are distinct and each is inside the modelled
        // `Name`.
        let names: Vec<String> = (0..row.racers).map(|index| row.distinct_name(index)).collect();

        let mut running = Vec::new();
        for (racer, name) in racers.iter().zip(&names) {
            let emulator = Arc::clone(&emulator);
            let flow_id = flow_id.clone();
            let request = racer.request(name);
            running.push(tokio::spawn(async move {
                emulator.create_flow_alias(&flow_id, request).await
            }));
        }

        let mut minted: Vec<(String, String)> = Vec::new();
        for (index, handle) in running.into_iter().enumerate() {
            let answer = handle.await.expect("the racing create ran to completion");
            assert_eq!(
                answer.status, 201,
                "case {case}, caller {index}: a create under its own name must be accepted, \
                 got {answer:?}"
            );
            assert_eq!(
                answer.string(&["name"]),
                names[index],
                "case {case}, caller {index}: expected the name asked for, got {answer:?}"
            );
            minted.push((answer.string(&["name"]), answer.string(&["id"])));
        }

        let ids: BTreeSet<&String> = minted.iter().map(|(_, id)| id).collect();
        assert_eq!(
            ids.len(),
            row.racers,
            "case {case}: expected {} distinct alias ids, got {minted:?}",
            row.racers
        );

        let listed = emulator.list_flow_aliases(&flow_id).await;
        let mut reported = listed.listed();
        reported.sort();
        minted.sort();
        assert_eq!(
            reported, minted,
            "case {case}: expected the flow to report every alias the burst minted, \
             got {listed:?}"
        );
    }
}

/// `clientToken` is an `@idempotencyToken`: a repeat of one request reports the
/// alias it already created rather than creating a second one — under contention
/// too. A burst of creates carrying ONE token mints one alias, every caller
/// reports that same alias, and the flow holds it once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_sharing_one_client_token_mint_one_alias() {
    // Inside the modelled `ClientToken` (len 33..256,
    // `pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`).
    const SHARED_TOKEN: &str = "one-idempotency-token-shared-by-every-racing-create";

    for row in rows() {
        let case = row.case;
        let emulator = Arc::new(Emulator::new());
        let flow_id = emulator.flow_with_versions(case).await;

        let racers = Racer::burst(row.racers);
        let mut running = Vec::new();
        for racer in &racers {
            let emulator = Arc::clone(&emulator);
            let flow_id = flow_id.clone();
            let mut request = racer.request(&row.name);
            request["clientToken"] = json!(SHARED_TOKEN);
            running.push(tokio::spawn(async move {
                emulator.create_flow_alias(&flow_id, request).await
            }));
        }

        let mut answers = Vec::new();
        for (index, handle) in running.into_iter().enumerate() {
            answers.push((
                index,
                handle.await.expect("the racing create ran to completion"),
            ));
        }

        let listed = emulator.list_flow_aliases(&flow_id).await;
        let reported = listed.listed();
        assert_eq!(
            reported.len(),
            1,
            "case {case}: one token may mint one alias, the flow holds {} : {listed:?}",
            reported.len()
        );
        let (stored_name, alias_id) = reported[0].clone();
        assert_eq!(
            stored_name, row.name,
            "case {case}: expected the stored alias to carry the name asked for, got {listed:?}"
        );

        // Every caller — the one that created the alias and every replay of its
        // token — is answered that one alias, as it stands in the store.
        let stored = emulator.get_flow_alias(&flow_id, &alias_id).await;
        assert_eq!(
            stored.status, 200,
            "case {case}: expected GetFlowAlias -> 200, got {stored:?}"
        );
        for (index, answer) in &answers {
            assert_eq!(
                answer.status, 201,
                "case {case}, caller {index}: a token replay is served, not refused, \
                 got {answer:?}"
            );
            for member in ["id", "arn", "name", "description", "createdAt"] {
                assert_eq!(
                    answer.member(&[member]),
                    stored.member(&[member]),
                    "case {case}, caller {index}: expected {member} to be the stored \
                     alias's {:?}, got {:?}",
                    stored.member(&[member]),
                    answer.member(&[member])
                );
            }
            assert_eq!(
                answer.member(&["routingConfiguration"]),
                stored.member(&["routingConfiguration"]),
                "case {case}, caller {index}: expected the stored alias's routing \
                 configuration {:?}, got {:?}",
                stored.member(&["routingConfiguration"]),
                answer.member(&["routingConfiguration"])
            );
        }
    }
}
