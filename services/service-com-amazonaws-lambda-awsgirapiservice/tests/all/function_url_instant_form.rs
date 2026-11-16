// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FORM every instant a function URL's configuration reports is
//! rendered in.
//!
//! RULE: `CreationTime` and `LastModifiedTime` of a function URL's configuration are
//! documented "in ISO-8601 format (YYYY-MM-DDThh:mm:ss.sTZD)", linking the W3C
//! NOTE-datetime profile
//! (`smithy_query(query="doc", shape="FunctionUrlConfig", member="CreationTime")`), whose
//! `TZD` is either the literal designator `Z` or an offset written `+hh:mm` — a
//! colon-less four-digit offset is not one of its forms. So every function-URL instant
//! this service reports is text a strict ISO-8601 reader parses, and it is the SAME
//! rendering whichever operation reports it: the two writes
//! (`CreateFunctionUrlConfig`, `UpdateFunctionUrlConfig`) and the two reads
//! (`GetFunctionUrlConfig`, `ListFunctionUrlConfigs`) answer one stored value.
//!
//! The strict reader here is the one this tree already resolves an instant with
//! (`aws_smithy_types::DateTime::from_str` with `Format::DateTime`, which accepts the `Z`
//! form only — see `services/common/src/rfc3339.rs`), so a rendering it rejects is one a
//! client parsing these members with an ISO-8601/RFC-3339 parser rejects too.
//!
//! The rows differ along what the rule generalises over: WHICH operation reported the
//! instant (each of the four), WHICH member it is (`CreationTime`, `LastModifiedTime`),
//! the qualifier the function URL sits on (the unpublished version and an alias), and
//! whether the configuration has been updated since it was created. The scope of the rule
//! is pinned too: a FUNCTION's own `LastModified` keeps the numeric-offset form the
//! model's own instant example spells, which `tests/all/function_url_reported_members.rs`
//! owns.
//!
//! Every row drives real HTTP requests through the router, so a form answered by one
//! serializer but not by the stored projection behind the others fails here.

use std::sync::Arc;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where the function-URL operations are bound
/// (`smithy_query(query="operation", operation="CreateFunctionUrlConfig")`:
/// `POST /2021-10-31/functions/{FunctionName}/url`).
const URL_API_VERSION: &str = "/2021-10-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`,
/// which is what routes a request here. Nothing in it is verified: the harness turns
/// request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The numeric UTC offset the FUNCTION's own instants carry, which the function URL's do
/// not: the designator of the model's own instant example for `CreatedDate` ("in ISO 8601
/// format. For example, `2018-11-27T15:10:45.123+0000`").
const NUMERIC_OFFSET: &str = "+0000";

/// What a dispatched request answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "expected a JSON body, got {error}: {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// A string member of the body, when it carries one.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The body text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// One service mounted on its own engine, reached the way the production router reaches
/// it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one JSON request.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// Create a function, publishing version 1 as well when asked (an alias needs a version to
/// point at).
async fn given_function(router: &MultiServiceRouter, name: &str, publish: bool) {
    let package: Vec<u8> = (0..64u16)
        .map(|byte| u8::try_from(byte % 251).unwrap_or(0))
        .collect();
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Publish": publish,
            "Code": { "ZipFile": base64::engine::general_purpose::STANDARD.encode(&package) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
}

/// Create the alias `alias` on version 1 of `name`, so a row can put a function URL on a
/// qualifier other than the unpublished version.
async fn given_alias(router: &MultiServiceRouter, name: &str, alias: &str) {
    let created = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        &json!({ "Name": alias, "FunctionVersion": "1" }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateAlias {alias:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
}

/// The function-URL URI of `name`, with the `Qualifier` query parameter when the row sits
/// on one.
fn url_uri(name: &str, qualifier: Option<&str>, collection: bool) -> String {
    let leaf = if collection { "urls" } else { "url" };
    let query = qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"));
    format!("{URL_API_VERSION}/{name}/{leaf}{query}")
}

/// Assert `value` is an instant in the form the function-URL members are documented in:
/// text the strict ISO-8601 reader parses, carrying the UTC designator rather than a
/// colon-less numeric offset. Answers the text, so a caller can order two of them.
fn asserted_instant(case: &str, member: &str, value: Option<String>) -> String {
    let text = value.unwrap_or_else(|| {
        panic!("case {case:?}: expected the string member {member} to be reported")
    });
    let read = DateTime::from_str(&text, Format::DateTime);
    assert!(
        read.is_ok(),
        "case {case:?}: {member} must be an ISO-8601 instant a strict reader parses, got \
         {text:?} ({read:?})"
    );
    assert!(
        !text.ends_with(NUMERIC_OFFSET),
        "case {case:?}: {member} must not carry the colon-less numeric offset \
         {NUMERIC_OFFSET:?}, which is no TZD of the documented profile, got {text:?}"
    );
    assert!(
        text.contains('T') && text.contains('.'),
        "case {case:?}: {member} must be a date, a `T`, a time and a fractional second, \
         got {text:?}"
    );
    text
}

/// RULE: every function-URL instant, from every operation that reports one, is rendered in
/// the documented TZD form — on the unpublished version and on an alias alike.
#[tokio::test]
async fn every_function_url_instant_carries_the_documented_designator() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // The qualifiers a function URL can sit on: the function's unpublished version (no
    // `Qualifier` at all) and an alias.
    let rows: Vec<(&str, &str, Option<&str>)> = vec![
        ("the unpublished version", "url-instants-latest", None),
        ("an alias", "url-instants-alias", Some("live")),
    ];

    for (case, function, qualifier) in rows {
        given_function(&router, function, qualifier.is_some()).await;
        if let Some(alias) = qualifier {
            given_alias(&router, function, alias).await;
        }

        let created = call(
            &router,
            "POST",
            &url_uri(function, qualifier, false),
            &json!({ "AuthType": "NONE" }),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: CreateFunctionUrlConfig must succeed, got {} {}",
            created.status,
            created.text()
        );
        let create_time = asserted_instant(
            &format!("{case}: CreateFunctionUrlConfig"),
            "CreationTime",
            created.string("CreationTime"),
        );

        // The READ of the same configuration projects the stored instants: the create's
        // own value, and the last change to the configuration, which is its creation
        // until something changes it.
        let read = call(&router, "GET", &url_uri(function, qualifier, false), &Value::Null).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetFunctionUrlConfig must succeed, got {} {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.string("CreationTime"),
            Some(create_time.clone()),
            "case {case:?}: the read must report the instant the create answered, got {}",
            read.text()
        );
        for member in ["CreationTime", "LastModifiedTime"] {
            asserted_instant(
                &format!("{case}: GetFunctionUrlConfig"),
                member,
                read.string(member),
            );
        }

        // The UPDATE re-stamps the last change, leaving the creation instant alone, and
        // both are reported in the same form.
        let updated = call(
            &router,
            "PUT",
            &url_uri(function, qualifier, false),
            &json!({ "AuthType": "AWS_IAM" }),
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: UpdateFunctionUrlConfig must succeed, got {} {}",
            updated.status,
            updated.text()
        );
        let update_case = format!("{case}: UpdateFunctionUrlConfig");
        let updated_creation =
            asserted_instant(&update_case, "CreationTime", updated.string("CreationTime"));
        let updated_modified = asserted_instant(
            &update_case,
            "LastModifiedTime",
            updated.string("LastModifiedTime"),
        );
        assert_eq!(
            updated_creation, create_time,
            "case {case:?}: an update leaves CreationTime where it was, got \
             {updated_creation:?}"
        );
        // One rendering of fixed width is what makes the TEXT order the instants: the
        // change cannot read as older than the creation.
        assert!(
            updated_modified >= create_time,
            "case {case:?}: LastModifiedTime {updated_modified:?} must not read as older \
             than CreationTime {create_time:?}"
        );

        // And the LIST projection of the same configuration, which is its own serializer.
        let listed = call(&router, "GET", &url_uri(function, None, true), &Value::Null).await;
        assert_eq!(
            listed.status,
            200,
            "case {case:?}: ListFunctionUrlConfigs must succeed, got {} {}",
            listed.status,
            listed.text()
        );
        let entries = listed.json()["FunctionUrlConfigs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: expected the one function URL to be listed, got {}",
            listed.text()
        );
        for member in ["CreationTime", "LastModifiedTime"] {
            let value = entries[0]
                .get(member)
                .and_then(Value::as_str)
                .map(str::to_string);
            let listed_instant = asserted_instant(
                &format!("{case}: ListFunctionUrlConfigs"),
                member,
                value,
            );
            let expected = if member == "CreationTime" {
                &create_time
            } else {
                &updated_modified
            };
            assert_eq!(
                &listed_instant, expected,
                "case {case:?}: the listed {member} must be the stored one, got \
                 {listed_instant:?}"
            );
        }
    }
}

/// RULE (the scope): the designator is the FUNCTION URL's, not every instant of this
/// service — a function's own `LastModified` keeps the numeric-offset form AWS's own
/// example of such a member spells, so the two renderings stand side by side on one
/// function.
#[tokio::test]
async fn a_functions_own_last_modified_keeps_the_numeric_offset() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let function = "url-instants-scope";
    given_function(&router, function, false).await;

    let configuration = call(
        &router,
        "GET",
        &format!("{FUNCTIONS_URI}/{function}/configuration"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        configuration.status,
        200,
        "GetFunctionConfiguration must succeed, got {} {}",
        configuration.status,
        configuration.text()
    );
    let last_modified = configuration.string("LastModified").unwrap_or_default();
    assert!(
        last_modified.contains('T') && last_modified.ends_with(NUMERIC_OFFSET),
        "the function's own LastModified keeps the numeric UTC offset of the model's \
         instant example, got {last_modified:?}"
    );

    let created = call(
        &router,
        "POST",
        &url_uri(function, None, false),
        &json!({ "AuthType": "NONE" }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunctionUrlConfig must succeed, got {} {}",
        created.status,
        created.text()
    );
    asserted_instant(
        "one function, both renderings",
        "CreationTime",
        created.string("CreationTime"),
    );
}
