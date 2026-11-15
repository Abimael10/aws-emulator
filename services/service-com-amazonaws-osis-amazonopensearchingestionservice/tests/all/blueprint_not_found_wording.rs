// SPDX-License-Identifier: CC0-1.0

//! How the blueprint lookup words its miss, driven end to end over HTTP.
//!
//! The rule under test: a blueprint lookup miss is refused 404
//! `ResourceNotFoundException` with the blueprint lookup's OWN wording — the resource
//! kind, then the requested name, then the service's "does not exist" clause — whose
//! only variable part is the `BlueprintName` path label, echoed verbatim; it is not a
//! not-found template shared with the name-addressed pipeline operations, each of which
//! words its own miss.
//!
//! The rows differ along what the rule generalises over: the shape and length of the
//! name that misses (nothing like a published name, a published name recased, suffixed,
//! prefixed, the catalog's own prefix alone, a one-character name, a long name, names
//! carrying digits, dots and underscores), and the `Format` the request asked for — the
//! wording depends on neither.

use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

/// A Region that is NOT the emulator default, so nothing here can be passing because it
/// was pinned to the default one.
const REGION: &str = "ap-southeast-2";

/// Every value `com.amazonaws.osis#BlueprintFormat` admits, from its
/// `@pattern("^(YAML|JSON)$")` (smithy_query: shape BlueprintFormat), plus the request
/// that names no format at all.
const FORMAT_CHOICES: [Option<&str>; 3] = [None, Some("YAML"), Some("JSON")];

/// The fixed text that precedes the requested name in the blueprint lookup's miss.
const PREFIX: &str = "Blueprint: ";

/// The fixed text that follows it.
const SUFFIX: &str = " does not exists";

/// What one call answered: the status, the error type header and the parsed body.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answer {
    /// The error message the response carries, under either casing a protocol may render
    /// the `message` member with.
    fn message(&self) -> String {
        for key in ["message", "Message"] {
            if let Some(text) = self.body.get(key).and_then(Value::as_str) {
                return text.to_string();
            }
        }
        panic!("expected an error message in {:?}", self.body)
    }
}

fn new_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere;
    // these cases are about this service's behaviour.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_osis_amazonopensearchingestionservice::EmulatedService::new(engine),
    );
    router
}

async fn call(router: &MultiServiceRouter, method: &str, path: &str) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://osis.{REGION}.amazonaws.com{path}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/osis/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::new())
        .expect("build request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(['/', ':'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_type,
        body,
    }
}

/// `GetPipelineBlueprint` for `name`, with `format` as its query parameter when given.
async fn get_blueprint(router: &MultiServiceRouter, name: &str, format: Option<&str>) -> Answer {
    let query = match format {
        Some(value) => format!("?format={value}"),
        None => String::new(),
    };
    call(
        router,
        "GET",
        &format!("/2022-01-01/osis/getPipelineBlueprint/{name}{query}"),
    )
    .await
}

/// The names `ListPipelineBlueprints` publishes, which is where a client resolves the
/// names this operation takes.
async fn published_names(router: &MultiServiceRouter) -> Vec<String> {
    let answer = call(router, "POST", "/2022-01-01/osis/listPipelineBlueprints").await;
    assert_eq!(
        answer.status, 200,
        "expected 200 from ListPipelineBlueprints, got {} / {:?}",
        answer.status, answer.body
    );
    answer.body["Blueprints"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a `Blueprints` array, got {:?}", answer.body))
        .iter()
        .map(|entry| {
            entry["BlueprintName"]
                .as_str()
                .unwrap_or_else(|| panic!("expected a string `BlueprintName` in {entry:?}"))
                .to_string()
        })
        .collect()
}

/// The rows: a case label and the name that misses, spanning the ways a name can miss and
/// the shapes a blueprint name can take.
fn missing_names(published: &str) -> Vec<(&'static str, String)> {
    vec![
        ("nothing like a published name", "no-such-blueprint".to_string()),
        ("a published name lower-cased", published.to_lowercase()),
        ("a published name upper-cased", published.to_uppercase()),
        ("a published name with a suffix", format!("{published}-v2")),
        ("a published name with a prefix", format!("Copy-Of-{published}")),
        ("the catalog prefix alone", "AWS-".to_string()),
        ("a one-character name", "x".to_string()),
        (
            "a long name",
            format!("AWS-{}", "Blueprint-That-Is-Not-Published".repeat(4)),
        ),
        ("digits only", "20220101".to_string()),
        ("a dotted name", "blueprint.not.published".to_string()),
        ("an underscored name", "blueprint_not_published".to_string()),
        ("mixed separators and digits", "AWS-Vpc_Flow-9.1".to_string()),
    ]
}

/// A blueprint name the catalog does not publish is refused 404 with the blueprint
/// lookup's own wording, the requested name echoed verbatim as its only variable part,
/// whatever shape the name takes and whichever format the request asked for.
#[tokio::test]
async fn a_blueprint_lookup_miss_carries_the_blueprint_wording() {
    let router = new_router();
    let published = published_names(&router).await;
    let a_published = published
        .first()
        .cloned()
        .expect("the catalog publishes at least one blueprint");

    for (case, name) in missing_names(&a_published) {
        assert!(
            !published.contains(&name),
            "case {case:?}: {name:?} must not be published for this row to test a miss, \
             got {published:?}"
        );

        for format in FORMAT_CHOICES {
            let case = format!("{case} (format {format:?})");
            let refused = get_blueprint(&router, &name, format).await;
            assert_eq!(
                refused.status, 404,
                "case {case:?}: expected status 404, got {} / {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_type, "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {:?}",
                refused.error_type
            );

            let message = refused.message();
            let expected = format!("{PREFIX}{name}{SUFFIX}");
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            // The name is the ONLY variable part: taking it back out leaves exactly the
            // fixed text, so no part of the request beyond the path label is echoed.
            let fixed = message.replacen(&name, "", 1);
            assert_eq!(
                fixed,
                format!("{PREFIX}{SUFFIX}"),
                "case {case:?}: the requested name is the only variable part, got {message:?}"
            );
            if let Some(requested) = format {
                assert!(
                    !message.contains(requested),
                    "case {case:?}: the requested format is not part of this wording, \
                     got {message:?}"
                );
            }
        }
    }
}

/// The blueprint lookup's miss is worded for the blueprint, not shared with the
/// name-addressed pipeline operations: the same missing name answered by each of them
/// carries a different message, and only the blueprint one states the blueprint kind.
#[tokio::test]
async fn the_blueprint_wording_is_not_a_shared_not_found_template() {
    let router = new_router();

    // Two names of different length and shape, so a wording's fixed text is
    // distinguished from its variable part.
    for name in ["mis", "missing-resource-name-of-28"] {
        let blueprint = get_blueprint(&router, name, None).await;
        assert_eq!(
            blueprint.status, 404,
            "case {name:?}: expected status 404 from GetPipelineBlueprint, got {} / {:?}",
            blueprint.status, blueprint.body
        );
        let blueprint_message = blueprint.message();
        assert_eq!(
            blueprint_message,
            format!("{PREFIX}{name}{SUFFIX}"),
            "case {name:?}: expected the blueprint lookup's wording, got {blueprint_message:?}"
        );

        // The pipeline operations addressed by a name answer the same missing name; none
        // of them may answer the blueprint lookup's wording.
        for (operation, method, segment) in [
            ("GetPipeline", "GET", "getPipeline"),
            ("StartPipeline", "PUT", "startPipeline"),
            ("StopPipeline", "PUT", "stopPipeline"),
            ("DeletePipeline", "DELETE", "deletePipeline"),
        ] {
            let case = format!("{operation} on {name:?}");
            let refused = call(
                &router,
                method,
                &format!("/2022-01-01/osis/{segment}/{name}"),
            )
            .await;
            assert_eq!(
                refused.status, 404,
                "case {case:?}: expected status 404, got {} / {:?}",
                refused.status, refused.body
            );
            let message = refused.message();
            assert_ne!(
                message, blueprint_message,
                "case {case:?}: a pipeline miss must not carry the blueprint wording, \
                 got {message:?}"
            );
            assert!(
                !message.starts_with(PREFIX),
                "case {case:?}: only the blueprint lookup states the blueprint kind, \
                 got {message:?}"
            );
        }
    }
}

/// The refusal is a report: a miss leaves the published catalog exactly as it was, so a
/// later caller sees the same names and the lookup of a published name still answers.
#[tokio::test]
async fn a_refused_lookup_changes_nothing_a_later_caller_sees() {
    let router = new_router();
    let before = published_names(&router).await;
    let a_published = before
        .first()
        .cloned()
        .expect("the catalog publishes at least one blueprint");

    for (case, name) in missing_names(&a_published) {
        let refused = get_blueprint(&router, &name, None).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected status 404, got {} / {:?}",
            refused.status, refused.body
        );
    }

    let after = published_names(&router).await;
    assert_eq!(
        after, before,
        "a refused lookup must change no published name, got {after:?} vs {before:?}"
    );

    let hit = get_blueprint(&router, &a_published, None).await;
    assert_eq!(
        hit.status, 200,
        "a published name must still answer 200 after the misses, got {} / {:?}",
        hit.status, hit.body
    );
}
