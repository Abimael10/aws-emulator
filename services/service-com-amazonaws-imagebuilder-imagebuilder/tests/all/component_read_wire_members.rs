// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what `GetComponent` reports about one component build version:
//! every member of the detailed view, including `billable` — the flag the real service sends
//! on a component read and this checkout's on-disk model does not declare.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * A component a read reports carries `billable`, the boolean that says whether the
//!   component incurs Amazon Web Services Marketplace charges, and it is read off the stored
//!   component: only a component the Marketplace publishes is metered — its charges are
//!   billed through the `productCodes` such a component carries — so every component a caller
//!   created in this account reports `false`, never an omitted member. The Marketplace-owned
//!   case, which no request of this API can store, is pinned by the unit test beside
//!   `ComponentRecord::billable` in `src/state.rs`, and the same flag on every entry of a
//!   component LISTING by `component_listing_wire_members`.
//! * The member sits beside the whole modelled view, which the read still reports member by
//!   member: the rows below differ along everything the view generalises over — the platform,
//!   the derived type, how many supported OS versions and tags the component carries, whether
//!   its document defines parameters, and which optional members the create left out — so
//!   what is pinned is the view rather than one component.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every case signs for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries, which is
/// also the `owner` of a component this account created.
const ACCOUNT: &str = "000000000000";

/// A build component document: one `build` phase, so the derived type is `BUILD`.
const BUILD_DOCUMENT: &str = concat!(
    "name: HelloWorld\n",
    "schemaVersion: 1.0\n",
    "phases:\n",
    "  - name: build\n",
    "    steps:\n",
    "      - name: Greet\n",
    "        action: ExecuteBash\n",
    "        inputs:\n",
    "          commands:\n",
    "            - echo hello\n",
);

/// A test component document: a single phase named `test`, so the derived type is `TEST`.
const TEST_DOCUMENT: &str = concat!(
    "name: CheckWorld\n",
    "schemaVersion: 1.0\n",
    "phases:\n",
    "  - name: test\n",
    "    steps:\n",
    "      - name: Check\n",
    "        action: ExecuteBash\n",
    "        inputs:\n",
    "          commands:\n",
    "            - echo ok\n",
);

/// A build document that DEFINES a parameter, so the read reports a parameter detail beside
/// the rest of the view.
const PARAMETERISED_DOCUMENT: &str = concat!(
    "name: Parameterised\n",
    "schemaVersion: 1.0\n",
    "parameters:\n",
    "  - Size:\n",
    "      type: string\n",
    "      default: small\n",
    "      description: how big\n",
    "phases:\n",
    "  - name: build\n",
    "    steps: []\n",
);

/// One emulator with an empty store, mounted exactly as the router mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(engine),
    );
    router
}

/// A request signed for `imagebuilder` in [`REGION`] — the signal the router routes on and
/// the handlers read the Region from.
fn request(method: &str, uri: &str, body: Option<&Value>) -> http::Request<bytes::Bytes> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("imagebuilder.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDIMAGEBUILDER/20260101/{REGION}/imagebuilder/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        );
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(serde_json::to_vec(value).expect("serialize the request body"))
        }
        None => bytes::Bytes::new(),
    };
    builder.body(payload).expect("a well-formed test request")
}

/// Status and parsed body of one call. An empty body reads as `Value::Null`.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let response = router.dispatch(request(method, uri, body)).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    (status, parsed)
}

/// Percent-encode one path segment, as an AWS SDK encodes a NON-greedy `@httpLabel`, so the
/// `:` and `/` of an ARN travel as `%3A` and `%2F` and the whole ARN stays ONE segment.
fn encode_label(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// A `CreateComponent` body: the required members, `clientToken`, and whatever else `extra`
/// names.
fn component_body(name: &str, client_token: &str, extra: Value) -> Value {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("semanticVersion".to_string(), json!("1.0.0"));
    body.insert("platform".to_string(), json!("Linux"));
    body.insert("clientToken".to_string(), json!(client_token));
    body.insert("data".to_string(), json!(BUILD_DOCUMENT));
    if let Value::Object(extra) = extra {
        for (member, value) in extra {
            body.insert(member, value);
        }
    }
    Value::Object(body)
}

/// One component a case stores and reads back.
struct Row {
    /// What the row is about.
    case: &'static str,
    /// The component's name.
    name: &'static str,
    /// The normalized name its ARN carries.
    normalized: &'static str,
    /// What the create carries beside the required members.
    extra: fn() -> Value,
    /// The type the document implies.
    component_type: &'static str,
    /// The platform the create named.
    platform: &'static str,
    /// The document the create carried, which a read reports as `data`.
    data: &'static str,
    /// The tags the read must report.
    tags: fn() -> Value,
    /// The parameter details the document defines, or `Value::Null` for a document that
    /// defines none.
    parameters: fn() -> Value,
}

/// The components the case below stores: rows differing along the platform, the derived type,
/// how many supported OS versions and tags the component carries, whether its document
/// defines parameters, and which optional members the create left out.
const ROWS: &[Row] = &[
    Row {
        case: "a component carrying only the required members",
        name: "minimal-component",
        normalized: "minimal-component",
        extra: || json!({}),
        component_type: "BUILD",
        platform: "Linux",
        data: BUILD_DOCUMENT,
        tags: || json!({}),
        parameters: || Value::Null,
    },
    Row {
        case: "a described and tagged component of several supported OS versions",
        name: "Described Component",
        normalized: "described-component",
        extra: || {
            json!({
                "description": "greets the image",
                "changeDescription": "the first version",
                "supportedOsVersions": ["Amazon Linux 2", "Amazon Linux 2023"],
                "tags": {"owner": "platform", "stage": "test"},
            })
        },
        component_type: "BUILD",
        platform: "Linux",
        data: BUILD_DOCUMENT,
        tags: || json!({"owner": "platform", "stage": "test"}),
        parameters: || Value::Null,
    },
    Row {
        case: "a test component on Windows",
        name: "windows-test-component",
        normalized: "windows-test-component",
        extra: || {
            json!({
                "platform": "Windows",
                "data": TEST_DOCUMENT,
                "supportedOsVersions": ["Microsoft Windows Server 2019"],
                "tags": {"owner": "platform"},
            })
        },
        component_type: "TEST",
        platform: "Windows",
        data: TEST_DOCUMENT,
        tags: || json!({"owner": "platform"}),
        parameters: || Value::Null,
    },
    Row {
        case: "a component whose document defines a parameter",
        name: "parameterised-component",
        normalized: "parameterised-component",
        extra: || json!({"data": PARAMETERISED_DOCUMENT}),
        component_type: "BUILD",
        platform: "Linux",
        data: PARAMETERISED_DOCUMENT,
        tags: || json!({}),
        parameters: || {
            json!([{
                "name": "Size",
                "type": "string",
                "defaultValue": ["small"],
                "description": "how big",
            }])
        },
    },
];

#[tokio::test]
async fn a_component_read_reports_billable_beside_the_whole_modelled_view() {
    let router = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let body = component_body(row.name, &format!("ctok-{index:04}"), (row.extra)());
        let (status, created) = call(&router, "PUT", "/CreateComponent", Some(&body)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the create, got {status} with {created}"
        );
        let arn = created
            .get("componentBuildVersionArn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a componentBuildVersionArn in {created}")
            })
            .to_string();
        let expected_arn = format!(
            "arn:aws:imagebuilder:{REGION}:{ACCOUNT}:component/{}/1.0.0/1",
            row.normalized
        );
        assert_eq!(
            arn, expected_arn,
            "case {case:?}: expected the build version ARN {expected_arn:?}, got {arn:?}"
        );

        let (status, read) = call(
            &router,
            "GET",
            &format!(
                "/GetComponent?componentBuildVersionArn={}",
                encode_label(&arn)
            ),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the read, got {status} with {read}"
        );
        let component = read
            .get("component")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a component member in {read}"));

        // The member the report is about: a boolean, false for a component this account owns.
        let billable = component.get("billable");
        assert_eq!(
            billable,
            Some(&json!(false)),
            "case {case:?}: expected billable false, got {billable:?} in {component}"
        );
        assert!(
            billable.is_some_and(Value::is_boolean),
            "case {case:?}: expected billable to be a JSON boolean, got {billable:?}"
        );

        // Beside it, the whole modelled view the read reports, member by member.
        let expected_members: Vec<(&str, Value)> = vec![
            ("arn", json!(arn)),
            ("name", json!(row.name)),
            ("version", json!("1.0.0")),
            ("type", json!(row.component_type)),
            ("platform", json!(row.platform)),
            ("owner", json!(ACCOUNT)),
            ("data", json!(row.data)),
            // "Image Builder encrypts all component data at rest, so this value is always
            // true" (`Component$encrypted`).
            ("encrypted", json!(true)),
            // "Indicates whether component source is hidden from view" — the document is
            // reported, so it is not hidden.
            ("obfuscate", json!(false)),
            ("state", json!({"status": "ACTIVE"})),
            ("tags", (row.tags)()),
        ];
        for (member, expected) in expected_members {
            let actual = component.get(member);
            assert_eq!(
                actual,
                Some(&expected),
                "case {case:?}: expected {member} to be {expected}, got {actual:?} in \
                 {component}"
            );
        }

        let parameters = (row.parameters)();
        let actual = component.get("parameters");
        if parameters.is_null() {
            assert!(
                actual.is_none(),
                "case {case:?}: expected no parameters for a document that defines none, got \
                 {actual:?}"
            );
        } else {
            assert_eq!(
                actual,
                Some(&parameters),
                "case {case:?}: expected the parameters the document defines, got {actual:?}"
            );
        }

        let described = (row.extra)();
        for member in ["description", "changeDescription", "supportedOsVersions"] {
            let submitted = described.get(member).cloned();
            let actual = component.get(member).cloned();
            assert_eq!(
                actual, submitted,
                "case {case:?}: expected {member} to be reported as submitted ({submitted:?}), \
                 got {actual:?}"
            );
        }

        // A component a caller created is neither third-party nor published through the
        // Marketplace, so it carries neither a publisher nor product codes — which is the
        // same reason it is not billable.
        for member in ["publisher", "productCodes"] {
            assert!(
                component.get(member).is_none(),
                "case {case:?}: expected no {member} on a component this account created, got \
                 {component}"
            );
        }

        let date_created = component.get("dateCreated").and_then(Value::as_str);
        assert!(
            date_created.is_some_and(|stamp| stamp.contains('T') && stamp.ends_with('Z')),
            "case {case:?}: expected an ISO 8601 dateCreated, got {date_created:?}"
        );
    }
}
