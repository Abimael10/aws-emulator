// SPDX-License-Identifier: CC0-1.0

//! Every change set carries a `ChangeSetName`, whether or not the start named one.
//!
//! `DescribeChangeSetResponse$ChangeSetName` and `ChangeSetSummaryListItem$ChangeSetName`
//! are documented "the optional name provided in the `StartChangeSet` request. If you do
//! not provide a name, one is set by default"
//! (`smithy_query(query="doc", shape="DescribeChangeSetResponse", member="ChangeSetName")`),
//! so the name is the caller's when the request carried one and the SERVICE's when it did
//! not — never absent. The rule is pinned over every surface that reports or selects on a
//! change set name: `DescribeChangeSet`, the `ListChangeSets` summary, and the
//! `ChangeSetName` filter of `ListChangeSets`, whose values are judged against
//! `ChangeSetName`'s own form (`^[\w\s+=.:@-]+$`, `@length(min: 1, max: 100)`).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_marketplacecatalog_awsmpseymour::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The catalog every operation's `Catalog` member documents as its fixed value.
const CATALOG: &str = "AWSMarketplace";

/// A SigV4 credential scope naming this service's signing name (`aws-marketplace`), which
/// is what routes a request here.
const SIGNED_FOR_EU_WEST_1: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/aws-marketplace/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The longest a change set name may be: `ChangeSetName`'s `@length(min: 1, max: 100)`.
const CHANGE_SET_NAME_MAX: usize = 100;

/// `ChangeSetName`'s `^[\w\s+=.:@-]+$` with its `@length(min: 1, max: 100)` folded in,
/// spelled out here rather than taken from the crate's own predicate so a wrong
/// implementation cannot make this test pass.
fn is_change_set_name(value: &str) -> bool {
    (1..=CHANGE_SET_NAME_MAX).contains(&value.chars().count())
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || c.is_whitespace()
                || c == '_'
                || matches!(c, '+' | '=' | '.' | ':' | '@' | '-')
        })
}

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
    /// The string at `pointer`, or the empty string.
    fn text(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The array at `pointer`, or an empty one.
    fn list(&self, pointer: &str) -> Vec<Value> {
        self.body
            .pointer(pointer)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "catalog.marketplace.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_EU_WEST_1);
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

async fn start_change_set(router: &MultiServiceRouter, request: Value) -> Answer {
    call(router, "POST", "/StartChangeSet", Some(request)).await
}

async fn describe_change_set(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/DescribeChangeSet?catalog={CATALOG}&changeSetId={id}"),
        None,
    )
    .await
}

async fn list_change_sets(router: &MultiServiceRouter, request: Value) -> Answer {
    call(router, "POST", "/ListChangeSets", Some(request)).await
}

/// The change set ids `ListChangeSets` reports for `filters`.
async fn ids_matching(router: &MultiServiceRouter, filters: Value) -> Vec<String> {
    let listed = list_change_sets(router, json!({"Catalog": CATALOG, "FilterList": filters})).await;
    assert_eq!(
        listed.status, 200,
        "ListChangeSets must answer 200, got {} with {:?}",
        listed.status, listed.body
    );
    listed
        .list("/ChangeSetSummaryList")
        .iter()
        .map(|item| item["ChangeSetId"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// One row of the table: a `StartChangeSet` request, and the name it submitted (if any).
struct Case {
    /// What this row exercises, named in every assertion it makes.
    label: &'static str,
    /// The `ChangeSetName` the start carries, or `None` for a start that names none.
    submitted: Option<String>,
    /// The further members this row puts on the request (intent, tags, a token).
    extra: Value,
    /// The entity type and product title of each change this row submits.
    changes: Vec<(&'static str, &'static str)>,
}

impl Case {
    /// The `StartChangeSet` body this row submits.
    fn request(&self) -> Value {
        let mut body = json!({
            "Catalog": CATALOG,
            "ChangeSet": self
                .changes
                .iter()
                .map(|(entity_type, title)| json!({
                    "ChangeType": "CreateProduct",
                    "Entity": {"Type": format!("{entity_type}@1.0")},
                    "Details": json!({"ProductTitle": title}).to_string(),
                }))
                .collect::<Vec<Value>>(),
        });
        if let Some(name) = self.submitted.as_deref() {
            body["ChangeSetName"] = json!(name);
        }
        for (member, value) in self.extra.as_object().cloned().unwrap_or_default() {
            body[member] = value;
        }
        body
    }
}

/// A one-change `StartChangeSet` body naming no change set name, creating a product
/// titled `title`.
fn unnamed_request(title: &'static str) -> Value {
    Case {
        label: title,
        submitted: None,
        extra: json!({}),
        changes: vec![("AmiProduct", title)],
    }
    .request()
}

/// A change set reports the name it was started with, and a change set started with NO
/// name reports the name the service set for it: the member is never absent, it satisfies
/// `ChangeSetName`'s contract, it is the same on every read, `ListChangeSets` reports the
/// same name, and the `ChangeSetName` filter selects the change set by it.
///
/// The rows differ along what the rule generalises over: whether a name was submitted,
/// what form that name takes (ordinary, every punctuation the pattern admits, the
/// published maximum length), how many changes the change set carries, its intent, and
/// whether it carries tags or an idempotency token.
#[tokio::test]
async fn every_change_set_reports_a_name() {
    // The submitted names span `ChangeSetName`'s published form; the unnamed rows are the
    // ones the service has to name itself.
    let cases: Vec<Case> = vec![
        Case {
            label: "no name, one change",
            submitted: None,
            extra: json!({}),
            changes: vec![("AmiProduct", "nameless")],
        },
        Case {
            label: "no name, three changes, tagged",
            submitted: None,
            extra: json!({"ChangeSetTags": [{"Key": "env", "Value": "test"}]}),
            changes: vec![
                ("AmiProduct", "first"),
                ("SaaSProduct", "second"),
                ("DataProduct", "third"),
            ],
        },
        Case {
            label: "no name, a VALIDATE change set",
            submitted: None,
            extra: json!({"Intent": "VALIDATE"}),
            changes: vec![("ContainerProduct", "validated")],
        },
        Case {
            label: "no name, carrying an idempotency token",
            submitted: None,
            extra: json!({"ClientRequestToken": "token-for-the-nameless-one"}),
            changes: vec![("MachineLearningProduct", "tokened")],
        },
        Case {
            label: "an ordinary submitted name",
            submitted: Some("named-one".to_string()),
            extra: json!({}),
            changes: vec![("AmiProduct", "named")],
        },
        Case {
            label: "a submitted name using every punctuation the pattern admits",
            submitted: Some("a b+c=d.e:f@g-h_1".to_string()),
            extra: json!({}),
            changes: vec![("SaaSProduct", "punctuated")],
        },
        Case {
            label: "a submitted name at the published maximum length",
            submitted: Some("n".repeat(CHANGE_SET_NAME_MAX)),
            extra: json!({}),
            changes: vec![("DataProduct", "longest")],
        },
    ];

    for row in cases {
        let case = row.label;
        let router = emulator();
        let started = start_change_set(&router, row.request()).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: StartChangeSet must answer 200, got {} with {:?}",
            started.status, started.body
        );
        let id = started.text("/ChangeSetId");

        let described = describe_change_set(&router, &id).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: DescribeChangeSet must answer 200, got {} with {:?}",
            described.status, described.body
        );
        let reported = described.body.pointer("/ChangeSetName").cloned();
        let name = described.text("/ChangeSetName");
        assert!(
            reported.is_some() && !name.is_empty(),
            "case {case:?}: every change set reports a ChangeSetName, got {:?}",
            described.body
        );
        assert!(
            is_change_set_name(&name),
            "case {case:?}: the reported name must satisfy ChangeSetName's own form, \
             got {name:?}"
        );
        if let Some(submitted) = row.submitted.as_deref() {
            assert_eq!(
                name, submitted,
                "case {case:?}: a submitted name is the name reported, got {name:?}"
            );
        }

        // A name is part of the record, so reading the change set again reports it again.
        let again = describe_change_set(&router, &id).await;
        assert_eq!(
            again.text("/ChangeSetName"),
            name,
            "case {case:?}: reading a change set twice must report the same name, got {:?}",
            again.body
        );

        // `ListChangeSets` projects the same record, so it reports the same name.
        let listed = list_change_sets(&router, json!({"Catalog": CATALOG})).await;
        let item = listed
            .list("/ChangeSetSummaryList")
            .into_iter()
            .find(|item| item["ChangeSetId"] == json!(id))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: ListChangeSets must report the change set, got {:?}",
                    listed.body
                )
            });
        assert_eq!(
            item["ChangeSetName"],
            json!(name),
            "case {case:?}: the listed name must be the described one, got {item:?}"
        );

        // And the name selects the change set, which is what a change set name is for.
        let selected = ids_matching(
            &router,
            json!([{"Name": "ChangeSetName", "ValueList": [name.clone()]}]),
        )
        .await;
        assert_eq!(
            selected,
            vec![id.clone()],
            "case {case:?}: the ChangeSetName filter must select the change set by the name \
             it reports, got {selected:?}"
        );
        let other = ids_matching(
            &router,
            json!([{"Name": "ChangeSetName", "ValueList": ["a name no change set holds"]}]),
        )
        .await;
        assert!(
            other.is_empty(),
            "case {case:?}: a name no change set holds must select none, got {other:?}"
        );
    }
}

/// The name the service sets names ONE change set: two change sets started without a name
/// carry different names, and a filter on either name selects only the change set holding
/// it.
#[tokio::test]
async fn each_unnamed_change_set_is_named_distinctly() {
    let router = emulator();
    let mut named: Vec<(String, String)> = Vec::new();
    for title in ["left", "right", "middle"] {
        let started = start_change_set(&router, unnamed_request(title)).await;
        assert_eq!(
            started.status, 200,
            "starting the {title:?} change set must answer 200, got {} with {:?}",
            started.status, started.body
        );
        let id = started.text("/ChangeSetId");
        let described = describe_change_set(&router, &id).await;
        let name = described.text("/ChangeSetName");
        assert!(
            is_change_set_name(&name),
            "the {title:?} change set must report a well-formed name, got {name:?}"
        );
        named.push((id, name));
    }

    for (id, name) in &named {
        assert_eq!(
            named.iter().filter(|(_, held)| held == name).count(),
            1,
            "the name the service sets must name one change set, {name:?} names several \
             of {named:?}"
        );
        let selected = ids_matching(
            &router,
            json!([{"Name": "ChangeSetName", "ValueList": [name.clone()]}]),
        )
        .await;
        assert_eq!(
            selected,
            vec![id.clone()],
            "filtering on {name:?} must select only {id:?}, got {selected:?}"
        );
    }
}
