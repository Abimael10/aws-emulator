// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `UpdateDataAutomationLibrary`.
//!
//! Every case drives the real `restJson1` wire path — `MultiServiceRouter::dispatch`
//! with a signed-looking request — so routing, HTTP binding, constraint validation, the
//! handler and response serialization are all exercised, exactly as an AWS SDK
//! `bedrock-data-automation` client would.
//!
//! The rule under test is the one the operation's model states: the update is an
//! `@idempotent` PUT over the one library member its request carries, so the addressed
//! library's `libraryDescription` takes exactly the value the request carries and is
//! CLEARED when the request omits it, every other member of the library (its ARN, name,
//! `creationTime`, entity types, encryption configuration and tags) is left where it
//! was, and the answer reports the library that is now stored — a later
//! `GetDataAutomationLibrary` agrees with it. A `libraryArn` that resolves to no stored
//! library is the modelled `ResourceNotFoundException`. The suites are TABLE-driven:
//! the rows differ along what the rule generalises over (the description a library
//! starts with, the description the update carries — absent, empty, punctuation, at the
//! modelled bound, outside ASCII — whether an idempotency token rides along, and how
//! many updates are chained), not over one recorded request.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state;
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state::ComAmazonawsBedrockdataautomationAmazonbedrockkeystonebuildtimeserviceState as ServiceState;
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{encode_label, error_code, string_at, timestamp_at};

/// The region every case signs for.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header for this service's signing name (`bedrock`, per
/// `OPERATIONS.md`): `can_handle` resolves the route key from it, and the create reads
/// the region out of its credential scope when minting an ARN.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTKEY/20240101/{REGION}/bedrock/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    )
}

/// The emulator under test, with a handle on the shared engine so a case can assert what
/// the store HOLDS and not only what it answers.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Dispatch one request and return its status and decoded JSON body (`Value::Null` for
/// an empty body).
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "bedrock-data-automation.amazonaws.com")
        .header("content-type", "application/json")
        .header("authorization", authorization())
        .body(Bytes::from(
            serde_json::to_vec(body).expect("serializable body"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let decoded = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// `CreateDataAutomationLibrary`, per its modelled `@http`
/// `{method: PUT, uri: /data-automation-libraries/, code: 201}`.
async fn create_library(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "PUT", "/data-automation-libraries/", body).await
}

/// `UpdateDataAutomationLibrary`, per its modelled `@http`
/// `{method: PUT, uri: /data-automation-libraries/{libraryArn}/, code: 200}`.
async fn update_library(router: &MultiServiceRouter, arn: &str, body: &Value) -> (u16, Value) {
    let uri = format!("/data-automation-libraries/{}/", encode_label(arn));
    call(router, "PUT", &uri, body).await
}

/// `GetDataAutomationLibrary`, per its modelled `@http`
/// `{method: POST, uri: /data-automation-libraries/{libraryArn}/}`.
async fn get_library(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    let uri = format!("/data-automation-libraries/{}/", encode_label(arn));
    call(router, "POST", &uri, &json!({})).await
}

/// The `library` structure a read answers, or a panic naming the case.
async fn read_library(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let (status, payload) = get_library(router, arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetDataAutomationLibrary 200, got {status} with {payload:?}"
    );
    payload.get("library").cloned().unwrap_or_else(|| {
        panic!("case {case:?}: GetDataAutomationLibrary returned no library: {payload:?}")
    })
}

/// A `clientToken` of the modelled form: `@length(33..256)` with
/// `@pattern ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`, so 33 characters at its lower bound.
fn client_token(seed: &str) -> String {
    let mut token = format!("tok-{seed}");
    while token.len() < 33 {
        token.push('a');
    }
    token
}

/// Seed one library and return the `libraryArn` the create answered.
async fn seeded_library(
    router: &MultiServiceRouter,
    name: &str,
    description: Option<&str>,
    case: &str,
) -> String {
    let mut body = json!({
        "libraryName": name,
        "encryptionConfiguration": {
            "kmsKeyId": "alias/bedrock-data-automation",
            "kmsEncryptionContext": { "tenant": "acme" },
        },
        "tags": [{ "key": "owner", "value": "ingest" }],
    });
    if let Some(text) = description {
        body["libraryDescription"] = json!(text);
    }
    let (status, created) = create_library(router, &body).await;
    assert_eq!(
        status, 201,
        "case {case:?}: the seed CreateDataAutomationLibrary must answer 201, \
         got {status} with {created:?}"
    );
    string_at(&created, "libraryArn", case)
}

/// The description member of a read, as `Option<&str>`: a cleared description is a
/// member the answer does not carry (or carries as null), never an empty string.
fn description_of(library: &Value) -> Option<&str> {
    library.get("libraryDescription").and_then(Value::as_str)
}

/// One update case: the description the library starts with, and the description the
/// update carries.
struct UpdateCase {
    /// The case name every assertion reports.
    case: &'static str,
    /// `libraryDescription` the create carried, or `None` to leave the member absent.
    before: Option<String>,
    /// `libraryDescription` the update carries, or `None` to leave the member absent —
    /// which, this being a PUT over that member, CLEARS it.
    after: Option<String>,
    /// Whether the update carries an `@idempotencyToken`.
    token: bool,
}

/// The update cases: the whole class a well-formed update spans.
///
/// The bounds and the character class the rows walk come from the model, not from a
/// recording: `UpdateDataAutomationLibraryRequest$libraryDescription` is
/// `@length(min: 0, max: 300)` over a `@pattern` whose class holds the ASCII letters,
/// digits, whitespace and punctuation plus a set of Latin-1 letters, currency signs and
/// symbols (source:
/// `smithy_query(query="operation",operation="UpdateDataAutomationLibrary")`), so the
/// rows below cover both published length bounds, the empty string, punctuation and the
/// non-ASCII half of the class, in both directions (set, replaced, cleared).
fn update_cases() -> Vec<UpdateCase> {
    vec![
        UpdateCase {
            case: "absent before, text after: the update sets a description",
            before: None,
            after: Some("entities for contract review".to_string()),
            token: false,
        },
        UpdateCase {
            case: "text before, other text after: the update replaces it",
            before: Some("the first description".to_string()),
            after: Some("invoices (2026) - Q4: #1 \"golden\" set!".to_string()),
            token: true,
        },
        UpdateCase {
            case: "text before, empty string after at the 0-character bound",
            before: Some("to be emptied".to_string()),
            after: Some(String::new()),
            token: false,
        },
        UpdateCase {
            case: "text before, absent after: the PUT clears the description",
            before: Some("to be cleared".to_string()),
            after: None,
            token: true,
        },
        UpdateCase {
            case: "absent before, absent after: the description stays absent",
            before: None,
            after: None,
            token: false,
        },
        UpdateCase {
            case: "text before, 300-character upper bound after",
            before: Some("short".to_string()),
            after: Some("d".repeat(300)),
            token: false,
        },
        UpdateCase {
            case: "text before, non-ASCII class members after",
            before: Some("ascii only".to_string()),
            after: Some("Révisé ✓ £100 ¡Œuvre! ⑆ ฿".to_string()),
            token: true,
        },
        UpdateCase {
            case: "single-character description after, at the 1-character length",
            before: Some("x".repeat(300)),
            after: Some("y".to_string()),
            token: false,
        },
    ]
}

impl UpdateCase {
    /// The update request body: a member this case leaves `None` is ABSENT from the
    /// JSON, not sent as null, so the row exercises the handler's own PUT semantics.
    fn body(&self, seed: &str) -> Value {
        let mut body = json!({});
        if let Some(text) = &self.after {
            body["libraryDescription"] = json!(text);
        }
        if self.token {
            body["clientToken"] = json!(client_token(seed));
        }
        body
    }
}

#[tokio::test]
async fn an_update_persists_the_description_the_request_carries() {
    let (router, engine) = emulator();

    for (index, spec) in update_cases().into_iter().enumerate() {
        let case = spec.case;
        let name = format!("updated_library_{index}");
        let arn = seeded_library(&router, &name, spec.before.as_deref(), case).await;
        let created = read_library(&router, &arn, case).await;
        let creation_time = timestamp_at(&created, "creationTime", case);

        let (status, updated) = update_library(&router, &arn, &spec.body(&format!("u{index}"))).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the modeled UpdateDataAutomationLibrary 200, \
             got {status} with {updated:?}"
        );
        assert_eq!(
            string_at(&updated, "libraryArn", case),
            arn,
            "case {case:?}: the update must answer about the library it addressed: {updated:?}"
        );
        // A library has nothing to provision, so it stays at the ready half of the
        // modelled pair `["ACTIVE", "DELETING"]`
        // (`smithy_query(shape="DataAutomationLibraryStatus")`).
        assert_eq!(
            string_at(&updated, "status", case),
            state::LIBRARY_ACTIVE,
            "case {case:?}: an updated library is {}: {updated:?}",
            state::LIBRARY_ACTIVE
        );

        // PERSISTED: the store holds the description the request carried, and nothing
        // else about the library moved.
        {
            let store = engine.state::<ServiceState>();
            let held = store.lock();
            let record = held
                .libraries
                .get(&arn)
                .unwrap_or_else(|| panic!("case {case:?}: the store holds no library at {arn}"));
            assert_eq!(
                record.library_description.as_deref(),
                spec.after.as_deref(),
                "case {case:?}: the stored libraryDescription is wrong"
            );
            assert_eq!(
                record.library_name, name,
                "case {case:?}: an update must not move the libraryName"
            );
            assert_eq!(
                record.tags.len(),
                1,
                "case {case:?}: an update must not move the library's tags, got {:?}",
                record.tags
            );
            assert_eq!(
                record.kms_key_id.as_deref(),
                Some("alias/bedrock-data-automation"),
                "case {case:?}: an update must not move the library's kmsKeyId"
            );
        }

        // ROUND TRIP: the read answers with the record the update left behind.
        let fetched = read_library(&router, &arn, case).await;
        assert_eq!(
            description_of(&fetched),
            spec.after.as_deref(),
            "case {case:?}: the wrong libraryDescription read back: {fetched:?}"
        );
        assert_eq!(
            string_at(&fetched, "libraryName", case),
            name,
            "case {case:?}: the wrong libraryName read back: {fetched:?}"
        );
        assert_eq!(
            timestamp_at(&fetched, "creationTime", case),
            creation_time,
            "case {case:?}: an update must not move creationTime: {fetched:?}"
        );
        assert_eq!(
            string_at(&fetched, "status", case),
            state::LIBRARY_ACTIVE,
            "case {case:?}: an updated library reads back {}: {fetched:?}",
            state::LIBRARY_ACTIVE
        );
        let held: Vec<String> = fetched
            .get("entityTypes")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no entityTypes in {fetched:?}"))
            .iter()
            .map(|item| string_at(item, "entityType", case))
            .collect();
        assert_eq!(
            held,
            vec![state::library_entity_types::PROVISIONED_ENTITY_TYPE.to_string()],
            "case {case:?}: an update must leave the entity types the library held: \
             {fetched:?}"
        );
        assert_eq!(
            fetched.get("kmsEncryptionContext"),
            Some(&json!({ "tenant": "acme" })),
            "case {case:?}: an update must not move kmsEncryptionContext: {fetched:?}"
        );
    }
}

#[tokio::test]
async fn chained_updates_each_take_effect_and_the_last_one_stands() {
    let (router, engine) = emulator();

    // The rows are a CHAIN over one library: every description form in turn, so each
    // row is applied on top of the previous row's result and the library ends at the
    // last row's value.
    let chain: Vec<Option<String>> = vec![
        Some("first".to_string()),
        Some(String::new()),
        None,
        Some("third - after a clear".to_string()),
        Some("e".repeat(300)),
        None,
    ];
    let arn = seeded_library(&router, "chained_library", None, "chain seed").await;

    for (index, description) in chain.iter().enumerate() {
        let case = format!("chain step {index}: {description:?}");
        let mut body = json!({});
        if let Some(text) = description {
            body["libraryDescription"] = json!(text);
        }
        let (status, updated) = update_library(&router, &arn, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {updated:?}"
        );
        let fetched = read_library(&router, &arn, &case).await;
        assert_eq!(
            description_of(&fetched),
            description.as_deref(),
            "case {case:?}: the step's description must be what a read reports: {fetched:?}"
        );
    }

    let store = engine.state::<ServiceState>();
    let held = store.lock();
    assert_eq!(
        held.libraries.len(),
        1,
        "the chain updated one library and created none, store holds {:?}",
        held.libraries.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        held.libraries
            .get(&arn)
            .and_then(|record| record.library_description.clone()),
        None,
        "the last step of the chain cleared the description"
    );
}

#[tokio::test]
async fn repeating_one_update_leaves_the_same_library_behind() {
    let (router, _engine) = emulator();

    // The rule is that the operation is `@idempotent`, so the rows differ in what the
    // repeated request carries: a description, no description at all, and a repeat that
    // changes only the idempotency token.
    let arn = seeded_library(&router, "idempotent_library", Some("start"), "seed").await;
    let repeats = [
        ("same description twice", json!({ "libraryDescription": "settled" })),
        ("no description twice", json!({})),
        (
            "same description, a fresh token each time",
            json!({ "libraryDescription": "settled again" }),
        ),
    ];

    for (case, body) in repeats {
        let mut first_body = body.clone();
        let mut second_body = body.clone();
        if case.contains("fresh token") {
            first_body["clientToken"] = json!(client_token("repeat-one"));
            second_body["clientToken"] = json!(client_token("repeat-two"));
        }

        let (status, first) = update_library(&router, &arn, &first_body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first update must answer 200, got {status} with {first:?}"
        );
        let after_first = read_library(&router, &arn, case).await;

        let (status, second) = update_library(&router, &arn, &second_body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the repeat must answer 200, got {status} with {second:?}"
        );
        let after_second = read_library(&router, &arn, case).await;

        assert_eq!(
            first, second,
            "case {case:?}: a repeated update must answer identically: \
             {first:?} then {second:?}"
        );
        assert_eq!(
            after_first, after_second,
            "case {case:?}: a repeated update must leave the same library behind: \
             {after_first:?} then {after_second:?}"
        );
    }
}

#[tokio::test]
async fn an_update_touches_only_the_library_it_addresses() {
    let (router, engine) = emulator();

    let first = seeded_library(&router, "neighbour_one", Some("one"), "first seed").await;
    let second = seeded_library(&router, "neighbour_two", Some("two"), "second seed").await;
    let before_second = read_library(&router, &second, "second untouched").await;

    let (status, updated) =
        update_library(&router, &first, &json!({ "libraryDescription": "one, revised" })).await;
    assert_eq!(
        status, 200,
        "the update must answer 200, got {status} with {updated:?}"
    );

    let after_first = read_library(&router, &first, "first updated").await;
    assert_eq!(
        description_of(&after_first),
        Some("one, revised"),
        "the addressed library must carry the new description: {after_first:?}"
    );
    let after_second = read_library(&router, &second, "second untouched").await;
    assert_eq!(
        before_second, after_second,
        "an update of one library must leave every other library untouched: \
         {before_second:?} then {after_second:?}"
    );

    let store = engine.state::<ServiceState>();
    let held = store.lock();
    assert_eq!(
        held.libraries.len(),
        2,
        "an update must not add a record, store holds {:?}",
        held.libraries.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn updating_a_library_arn_that_resolves_to_nothing_is_the_modeled_404() {
    let (router, engine) = emulator();

    // Every row is a well-formed `DataAutomationLibraryArn` (so the modelled pattern
    // admits it and the request reaches the handler) that names no stored library, in the
    // Region the request is signed for — a never-created id, an id carrying the pattern's
    // hyphen, and an id at each published bound of the modelled 12..36 id length. An ARN of
    // ANOTHER Region is not a lookup miss at all but an input the endpoint does not admit;
    // `tests/all/library_arn_region_admission.rs` covers that class.
    let unknown = [
        (
            "never created",
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/0000000a-00000000000000ff",
        ),
        (
            "an id carrying the pattern's hyphen",
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/dead-beef-1234",
        ),
        (
            "id at the 12-character lower bound",
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/abcdefabcdef",
        ),
        (
            "id at the 36-character upper bound",
            "arn:aws:bedrock:us-east-1:000000000000:data-automation-library/abcdefabcdefabcdefabcdefabcdefabcdef",
        ),
    ];
    for (case, arn) in unknown {
        let (status, payload) =
            update_library(&router, arn, &json!({ "libraryDescription": "no such library" })).await;
        assert_eq!(
            status, 404,
            "case {case:?}: an unknown libraryArn is the modeled ResourceNotFoundException \
             (404), got {status} with {payload:?}"
        );
        assert_eq!(
            error_code(&payload),
            "ResourceNotFoundException",
            "case {case:?}: wrong error code in {payload:?}"
        );
        assert_eq!(
            payload.get("message").and_then(Value::as_str),
            Some(state::LIBRARY_ARN_NOT_FOUND_MESSAGE),
            "case {case:?}: wrong refusal message in {payload:?}"
        );
    }

    let store = engine.state::<ServiceState>();
    let held = store.lock();
    assert!(
        held.libraries.is_empty(),
        "a refused update must not create a library, store holds {:?}",
        held.libraries.keys().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_description_past_the_modeled_bound_is_refused_and_changes_nothing() {
    let (router, _engine) = emulator();

    let arn = seeded_library(&router, "bounded_library", Some("kept"), "seed").await;

    // One past the published `@length(0..300)` upper bound of
    // `DataAutomationLibraryDescription`, and a value outside its `@pattern` character
    // class (`§`-style symbols are in the class; a tab-free control-free `…` is not).
    let refused = [
        ("301 characters, one past the modeled bound", "d".repeat(301)),
        ("a character outside the modeled class", "ellipsis … here".to_string()),
    ];
    for (case, description) in refused {
        let (status, payload) =
            update_library(&router, &arn, &json!({ "libraryDescription": description })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the modeled constraint is the modeled \
             ValidationException (400), got {status} with {payload:?}"
        );
        assert_eq!(
            error_code(&payload),
            "ValidationException",
            "case {case:?}: wrong error code in {payload:?}"
        );
        let fetched = read_library(&router, &arn, case).await;
        assert_eq!(
            description_of(&fetched),
            Some("kept"),
            "case {case:?}: a refused update must leave the description as it was: {fetched:?}"
        );
    }
}
