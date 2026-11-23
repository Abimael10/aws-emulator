// SPDX-License-Identifier: CC0-1.0

//! Which COLLECTION a `nextToken` of this service names a page of, end to end over HTTP.
//!
//! The rule under test: a page token names a page of one collection of one app
//! environment, so an operation honours only the tokens issued over the collection IT
//! pages — its own and those of the other operation paging the same records — and refuses
//! a token issued over any other collection with the modelled `InvalidParameterException`
//! (HTTP 400), rather than resuming at a position of an unrelated collection and serving
//! the wrong page.
//!
//! The records of every collection draw their cursors from one creation order, so without
//! that discriminator a token of one collection resolves to a cursor of another: a page
//! served from it is silently truncated when the cursor sits inside this collection's
//! records, and empty when it sits past them.
//!
//! The table below drives every pair: each of the four collections this service pages
//! issues a token through its list, and each token is presented to every paginated
//! operation of the crate — the four lists and the three exports. Every case goes through
//! the router: routing, deserialization, the handler, the persisted state and
//! serialization.
//!
//! The second test drives the same rule from the other issuing side: a token an EXPORT
//! hands out is honoured by the list of the collection it was served over, and refused by
//! every operation paging another collection and by the same export addressing another app
//! environment. Which OPERATION issued a token is not the discriminator — which collection
//! of which app environment it was served over is.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::state::EXPORT_PAGE_LIMIT;
use service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service: it is what routes a request here.
const SIGNED_FOR_AMPLIFYUIBUILDER: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/amplifyuibuilder/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The app environment every case addresses. `AppId` is `@length(min: 1, max: 20)` with
/// pattern `^d[a-z0-9]+$`.
const APP_ID: &str = "d1234abcd";
const ENVIRONMENT: &str = "staging";

/// A second backend environment of the same app, whose pages are a different scope: a token
/// names a page of one collection of ONE app environment, so the cases below present a
/// token issued in `ENVIRONMENT` to the same operation addressing this one.
const OTHER_ENVIRONMENT: &str = "production";

/// How many records each collection holds: two, so a list of `maxResults=1` issues a token
/// and the page that token resumes carries exactly the second record.
const RECORDS: usize = 2;

/// How a page token of this service is spelled: a literal prefix, then the scope digest,
/// then the cursor, both halves in hex. Spelled out here rather than imported, and checked
/// below against a token the service really issued, so the digest halves this suite
/// compares are the ones the service emits.
const TOKEN_PREFIX: &str = "c-";
const SCOPE_HEX_WIDTH: usize = 8;
const CURSOR_HEX_WIDTH: usize = 16;

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The error-type discriminator the body carries — `__type`, or the `code` / `Code`
    /// spellings the same JSON error parsers also accept — or the empty string when it
    /// carries none. AmplifyUIBuilder names no exception on the wire, so a refusal of one
    /// of this service's modelled exceptions must carry none of them.
    fn error_code(&self) -> &str {
        ["__type", "code", "Code"]
            .iter()
            .find_map(|member| self.body[*member].as_str())
            .unwrap_or_default()
    }

    /// The ids the `entities` of this answer carry, in the order they are reported.
    fn entity_ids(&self) -> Vec<String> {
        self.body["entities"]
            .as_array()
            .map(|entities| {
                entities
                    .iter()
                    .map(|entity| entity["id"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the answer carries an `entities` list at all.
    fn has_entities(&self) -> bool {
        self.body["entities"].is_array()
    }

    /// The `nextToken` the answer carries, if any.
    fn next_token(&self) -> Option<String> {
        self.body["nextToken"].as_str().map(str::to_string)
    }

    /// The `message` the answer carries, or the empty string.
    fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }

    /// The `id` a create answered.
    fn id(&self) -> String {
        self.body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("a created record must carry an id, got {:?}", self.body))
            .to_string()
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "amplifyuibuilder.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_AMPLIFYUIBUILDER);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// The collections this service pages, in the order the fixture creates them — so the
/// cursors of each collection sit both below and above those of another, which is what
/// makes a token of the wrong collection resolve inside one collection's records in one
/// direction and past them in the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Collection {
    Themes,
    Components,
    Forms,
    CodegenJobs,
}

/// Every collection, in creation order.
const COLLECTIONS: [Collection; 4] = [
    Collection::Themes,
    Collection::Components,
    Collection::Forms,
    Collection::CodegenJobs,
];

impl Collection {
    /// The last path segment of the collection, as the `@http` URIs of this service spell
    /// it.
    fn segment(self) -> &'static str {
        match self {
            Collection::Components => "components",
            Collection::Forms => "forms",
            Collection::Themes => "themes",
            Collection::CodegenJobs => "codegen-jobs",
        }
    }

    /// The smallest payload the create of this collection admits — the members its request
    /// shape requires, which is all a case about page tokens needs.
    fn create_payload(self, index: usize) -> Value {
        let name = format!("scope-{index}");
        match self {
            Collection::Components => json!({
                "name": name,
                "componentType": "View",
                "properties": {},
                "variants": [],
                "overrides": {},
                "bindingProperties": {},
            }),
            Collection::Forms => json!({
                "name": name,
                "formActionType": "create",
                "dataType": {"dataSourceType": "DataStore", "dataTypeName": "Post"},
                "fields": {},
                "style": {},
                "sectionalElements": {},
                "schemaVersion": "1.0",
            }),
            Collection::Themes => json!({"name": name, "values": []}),
            // `StartCodegenJobData` requires only `renderConfig`, a union with one variant.
            Collection::CodegenJobs => json!({"renderConfig": {"react": {}}}),
        }
    }

    /// The operation paging this collection that a request can ask for a page size, and so
    /// the one a case takes an issued token from: `maxResults=1` over two records issues a
    /// token without needing a whole export page of fixtures.
    fn list_operation(self) -> &'static str {
        match self {
            Collection::Components => "ListComponents",
            Collection::Forms => "ListForms",
            Collection::Themes => "ListThemes",
            Collection::CodegenJobs => "ListCodegenJobs",
        }
    }

    /// Where this collection is created and listed in one backend environment of the app.
    fn list_uri_in(self, environment: &str) -> String {
        format!(
            "/app/{APP_ID}/environment/{environment}/{segment}",
            segment = self.segment()
        )
    }

    /// Where this collection is created and listed in the app environment the cases address.
    fn list_uri(self) -> String {
        self.list_uri_in(ENVIRONMENT)
    }

    /// The operation exporting this collection, for the three collections this service
    /// models an export of: `ExportComponents`, `ExportForms` and `ExportThemes`. The
    /// codegen jobs are paged by their list alone.
    fn export_operation(self) -> Option<&'static str> {
        match self {
            Collection::Components => Some("ExportComponents"),
            Collection::Forms => Some("ExportForms"),
            Collection::Themes => Some("ExportThemes"),
            Collection::CodegenJobs => None,
        }
    }

    /// Where this collection is exported in one backend environment of the app.
    fn export_uri_in(self, environment: &str) -> String {
        format!(
            "/export/app/{APP_ID}/environment/{environment}/{segment}",
            segment = self.segment()
        )
    }

    /// The export of this collection and where it is bound in the app environment the cases
    /// address.
    fn export(self) -> Option<(&'static str, String)> {
        self.export_operation()
            .map(|operation| (operation, self.export_uri_in(ENVIRONMENT)))
    }

    /// The list paging this collection, as a receiver a token can be presented to.
    fn list_receiver(self) -> Receiver {
        Receiver {
            operation: self.list_operation(),
            uri: self.list_uri(),
            collection: self,
        }
    }
}

/// One operation a page token can be presented to: its name, where it is bound, and the
/// collection it pages.
struct Receiver {
    operation: &'static str,
    uri: String,
    collection: Collection,
}

/// Every paginated operation of this crate: the four lists and the three exports.
fn receivers() -> Vec<Receiver> {
    let mut rows: Vec<Receiver> = Vec::new();
    for collection in COLLECTIONS {
        rows.push(collection.list_receiver());
        if let Some((operation, uri)) = collection.export() {
            rows.push(Receiver {
                operation,
                uri,
                collection,
            });
        }
    }
    rows
}

/// Create one record of `collection` in one backend environment of the app and report the
/// id it was minted under.
async fn create_in(
    router: &MultiServiceRouter,
    environment: &str,
    collection: Collection,
    index: usize,
) -> String {
    let answer = call(
        router,
        "POST",
        &collection.list_uri_in(environment),
        Some(collection.create_payload(index)),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "creating a {collection:?} record in {environment} must answer HTTP 200, got {} with \
         {:?}",
        answer.status, answer.body
    );
    answer.id()
}

/// Create one record of `collection` in the app environment the cases address and report the
/// id it was minted under.
async fn create(router: &MultiServiceRouter, collection: Collection, index: usize) -> String {
    create_in(router, ENVIRONMENT, collection, index).await
}

/// The token the list of `collection` issues after its first record: a token the service
/// really handed out over that collection.
async fn issued_token(router: &MultiServiceRouter, collection: Collection) -> String {
    let answer = call(
        router,
        "GET",
        &format!("{}?maxResults=1", collection.list_uri()),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {collection:?}: a first page of one record must be served, got {} with {:?}",
        answer.status, answer.body
    );
    answer.next_token().unwrap_or_else(|| {
        panic!(
            "case {collection:?}: a first page of {RECORDS} records must carry a nextToken, got \
             {:?}",
            answer.body
        )
    })
}

/// The scope half of a token: what says which app environment and which collection the
/// page it names was served over.
fn scope_of(token: &str) -> &str {
    assert_eq!(
        token.len(),
        TOKEN_PREFIX.len() + SCOPE_HEX_WIDTH + CURSOR_HEX_WIDTH,
        "a page token must be spelled as prefix, scope and cursor, got {token:?}"
    );
    &token[TOKEN_PREFIX.len()..TOKEN_PREFIX.len() + SCOPE_HEX_WIDTH]
}

/// A token issued over one collection names a page of that collection only: the operations
/// paging it serve the page that resumes where it points, and every operation paging
/// another collection refuses it with the modelled `InvalidParameterException` rather than
/// resuming at a position of records it does not page.
///
/// The rows differ in which collection issued the token and which operation it is
/// presented to — every one of the four collections against every one of the seven
/// paginated operations, so each pair of collections is covered in both directions.
#[tokio::test]
async fn a_page_token_names_a_page_of_the_collection_that_issued_it() {
    let router = emulator();

    // Every collection holds the same number of records, created one collection after the
    // other out of the one creation order: so the cursors of a given collection are above
    // those of the collections created before it and below those created after it.
    let mut created: Vec<(Collection, Vec<String>)> = Vec::new();
    for collection in COLLECTIONS {
        let mut ids: Vec<String> = Vec::new();
        for index in 0..RECORDS {
            ids.push(create(&router, collection, index).await);
        }
        created.push((collection, ids));
    }

    // One token per collection, taken from the list that pages it.
    let mut tokens: Vec<(Collection, String)> = Vec::new();
    for collection in COLLECTIONS {
        tokens.push((collection, issued_token(&router, collection).await));
    }

    // The discriminator is in the token: two collections of the same app environment are
    // issued tokens of different scope, so neither can be mistaken for the other.
    for (first, first_token) in &tokens {
        for (second, second_token) in &tokens {
            if first == second {
                continue;
            }
            assert_ne!(
                scope_of(first_token),
                scope_of(second_token),
                "case {first:?} vs {second:?}: the tokens of two collections must carry \
                 different scopes, got {first_token:?} and {second_token:?}"
            );
        }
    }

    for Receiver {
        operation,
        uri,
        collection,
    } in receivers()
    {
        for (issuer, token) in &tokens {
            let case = format!("a {issuer:?} token presented to {operation}");
            let answer = call(&router, "GET", &format!("{uri}?nextToken={token}"), None).await;

            if *issuer == collection {
                // The operation pages the records the token was issued over, so it resumes
                // exactly where the token points: at the records after the first one.
                let expected: Vec<String> = created
                    .iter()
                    .find(|(kind, _)| kind == issuer)
                    .map(|(_, ids)| ids[1..].to_vec())
                    .unwrap_or_default();
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: a token of the collection this operation pages must be \
                     served, got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.entity_ids(),
                    expected,
                    "case {case:?}: the page must resume at the record the token names, got {:?}",
                    answer.entity_ids()
                );
                continue;
            }

            assert_eq!(
                answer.status, 400,
                "case {case:?}: a token of another collection names no page of this one and \
                 must be refused, got {} with {:?}",
                answer.status, answer.body
            );
            assert!(
                !answer.has_entities(),
                "case {case:?}: a refusal must carry no entities, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.error_code(),
                "",
                "case {case:?}: a refusal must name no error type, got {:?}",
                answer.body
            );
            let message = answer.message();
            assert!(
                message.contains(operation) && message.contains(token.as_str()),
                "case {case:?}: the refusal must name the operation and the token it refuses, \
                 got {message:?}"
            );
        }
    }

    // None of those refusals changed what the operations serve: every collection still
    // reports exactly its own records, in creation order, when asked without a token.
    for Receiver {
        operation,
        uri,
        collection,
    } in receivers()
    {
        let answer = call(&router, "GET", &uri, None).await;
        let expected: Vec<String> = created
            .iter()
            .find(|(kind, _)| *kind == collection)
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default();
        assert_eq!(
            answer.status, 200,
            "case {operation} after the refusals: a page with no token must be served, got {} \
             with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.entity_ids(),
            expected,
            "case {operation} after the refusals: the page must report this collection's own \
             records, got {:?}",
            answer.entity_ids()
        );
    }
}

/// How many records of one collection it takes for an EXPORT to issue a token: one past the
/// bound an export page serves, so the first page ends before the last record and reports
/// the token that resumes at it.
const EXPORTED_RECORDS: usize = EXPORT_PAGE_LIMIT + 1;

/// How many records each collection other than the one under test holds: enough for a page
/// of its own, created AFTER the records under test, so their cursors sit above the one an
/// export token carries — without the collection in the token's scope such a token would
/// resume inside them and serve a page of records the export never paged.
const OTHER_RECORDS: usize = 3;

/// The same rule from the issuing side an export reaches: a token an EXPORT hands out names
/// a page of the collection and app environment it was served over, so the list of that
/// collection resumes at it exactly as the export does, and every operation paging another
/// collection — or the same collection of another app environment — refuses it with the
/// modelled `InvalidParameterException` (HTTP 400).
///
/// The rows differ in which collection issued the token (each of the three this service
/// models an export of) and in which operation it is presented to (the four lists, the
/// three exports, and the export of the same collection in a second app environment), so
/// both operations paging one collection are covered as issuer and as receiver.
#[tokio::test]
async fn an_export_issued_page_token_is_honoured_by_the_list_of_the_same_collection() {
    for subject in COLLECTIONS {
        let Some((export_operation, export_uri)) = subject.export() else {
            // Only a collection this service models an export of can issue a token from
            // one: the codegen jobs are paged by their list alone.
            continue;
        };
        // A fresh store per row, so the cursors of the records under test are the first
        // ones the store hands out and the ones created after them sit above.
        let router = emulator();
        let mut created: Vec<String> = Vec::new();
        for index in 0..EXPORTED_RECORDS {
            created.push(create(&router, subject, index).await);
        }
        let mut elsewhere: Vec<(Collection, Vec<String>)> = Vec::new();
        for collection in COLLECTIONS.into_iter().filter(|kind| *kind != subject) {
            let mut ids: Vec<String> = Vec::new();
            for index in 0..OTHER_RECORDS {
                ids.push(create(&router, collection, index).await);
            }
            elsewhere.push((collection, ids));
        }
        for index in 0..OTHER_RECORDS {
            create_in(&router, OTHER_ENVIRONMENT, subject, index).await;
        }

        // The export serves its own page bound and issues the token that resumes after it.
        let first = call(&router, "GET", &export_uri, None).await;
        assert_eq!(
            first.status, 200,
            "case {export_operation}: the first export page must be served, got {} with {:?}",
            first.status, first.body
        );
        assert_eq!(
            first.entity_ids(),
            created[..EXPORT_PAGE_LIMIT].to_vec(),
            "case {export_operation}: the first page must carry the first {EXPORT_PAGE_LIMIT} \
             records in creation order, got {:?}",
            first.entity_ids()
        );
        let token = first.next_token().unwrap_or_else(|| {
            panic!(
                "case {export_operation}: a first page of {EXPORTED_RECORDS} records must carry \
                 a nextToken, got {:?}",
                first.body
            )
        });

        // The rest of the collection, which is what the token names: every operation that
        // pages this collection must resume there — the export that issued it and the list
        // beside it alike.
        let rest: Vec<String> = created[EXPORT_PAGE_LIMIT..].to_vec();
        for Receiver {
            operation,
            uri,
            collection: _,
        } in [
            Receiver {
                operation: export_operation,
                uri: export_uri.clone(),
                collection: subject,
            },
            subject.list_receiver(),
        ] {
            let case = format!("an {export_operation} token presented to {operation}");
            let answer = call(&router, "GET", &format!("{uri}?nextToken={token}"), None).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: a token issued over the collection this operation pages must be \
                 served, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.entity_ids(),
                rest,
                "case {case:?}: the page must resume at the record the token names, got {:?}",
                answer.entity_ids()
            );
            assert_eq!(
                answer.next_token(),
                None,
                "case {case:?}: a page that ends the collection must carry no nextToken, got {:?}",
                answer.body
            );
        }

        // Every operation paging another collection, and the export of this collection in
        // another app environment, names no page of that token and must refuse it rather
        // than resume at a position of records it does not page.
        let other_environment = Receiver {
            operation: export_operation,
            uri: subject.export_uri_in(OTHER_ENVIRONMENT),
            collection: subject,
        };
        let refusing = receivers()
            .into_iter()
            .filter(|row| row.collection != subject)
            .chain([other_environment]);
        for Receiver { operation, uri, .. } in refusing {
            let case = format!("an {export_operation} token presented to {uri}");
            let answer = call(&router, "GET", &format!("{uri}?nextToken={token}"), None).await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: a token this operation's collection never issued must be refused, \
                 got {} with {:?}",
                answer.status, answer.body
            );
            assert!(
                !answer.has_entities(),
                "case {case:?}: a refusal must carry no entities, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.error_code(),
                "",
                "case {case:?}: a refusal must name no error type, got {:?}",
                answer.body
            );
            let message = answer.message();
            assert!(
                message.contains(operation) && message.contains(token.as_str()),
                "case {case:?}: the refusal must name the operation and the token it refuses, \
                 got {message:?}"
            );
        }

        // None of those refusals served or hid a page: each other collection still reports
        // exactly its own records, in creation order.
        for (collection, ids) in &elsewhere {
            let Receiver { operation, uri, .. } = collection.list_receiver();
            let answer = call(&router, "GET", &uri, None).await;
            assert_eq!(
                answer.status, 200,
                "case {operation} after the refusals: a page with no token must be served, got {} \
                 with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                &answer.entity_ids(),
                ids,
                "case {operation} after the refusals: the page must report this collection's own \
                 records, got {:?}",
                answer.entity_ids()
            );
        }
    }
}
