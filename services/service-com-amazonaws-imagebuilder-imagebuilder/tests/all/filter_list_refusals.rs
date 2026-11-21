// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for how this service refuses the `filters` a listing carries.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, this service's front door, the modelled-constraint walk and the handlers —
//! so what is asserted is what a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * a filter naming a member a listing does not filter on is refused HTTP 400
//!   `InvalidParameterValueException`, stating the parameter the request carried and, as its
//!   requirement, the filter names THAT operation admits in the order that operation states
//!   them. The pipeline listing reports the failing LIST ELEMENT (`filters[<i>].name`), so a
//!   request carrying several filters says which one was refused; every other listing reports
//!   the `filters` member and names the operation whose set it is;
//! * a collection the request carries whose modelled `@length` bound it broke is refused the
//!   same way — one of this service's own modelled error shapes, stating that member's
//!   requirement — rather than as the framework's generic constraint violation under a class
//!   this model does not declare at all. The pipeline listing states that requirement as the
//!   modelled bound the member's length had to satisfy; every other operation states how many
//!   items the member has to carry or may carry. A collection AT either bound is served;
//! * a filter that left out a member every filter has to carry — the member it selects on, or
//!   the values that member may take, neither of which the model can require — is reported as
//!   a missing required parameter, naming that member of the filter and nothing else of the
//!   request; the pipeline listing names the ELEMENT and the member of it.
//!
//! The rows span what those rules generalise over: which listing was addressed, which member
//! the filter names and how it is wrong (another listing's member, a member that is not a
//! filter, a spelling, a name nothing names), WHERE in the list the refused filter sits, which
//! collection broke its bound (`filters` and one filter's `values`), how far past the bound it
//! is, and which member of a filter the request left out.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The error this service refuses a parameter value with
/// (`smithy_query(query="shape",shape="InvalidParameterValueException")`: a client error,
/// `@httpError` 400).
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The class a filter that left out a member every filter has to carry is reported under: the
/// missing-parameter class this service reports a parameter its model cannot require with.
const MISSING_PARAMETERS: &str = "MissingParametersException";

/// The most filters one listing request may carry: the modelled `FilterList` `@length`
/// maximum (`smithy_query(query="shape",shape="FilterList")`: 1..10). The same bound the
/// values of ONE filter carry (`smithy_query(query="shape",shape="Filter")`: `values`
/// 1..10).
const MAX_FILTERS: usize = 10;

/// One emulator with an empty store, mounted exactly as the router mounts it. A listing of an
/// empty store reports an empty page, which is all an accepted row here has to observe.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(engine),
    );
    router
}

/// One listing call: its status, the error shape the `x-amzn-errortype` header names, the
/// `__type` the body carries and the `message` it states.
async fn list(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, String, String, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("imagebuilder.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDIMAGEBUILDER/20260101/{REGION}/imagebuilder/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("serialize the request body"),
        ))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(shape_name)
        .unwrap_or_default();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(shape_name)
        .unwrap_or_default();
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, header, code, message)
}

/// The shape name a discriminator value names, without its namespace or locator.
fn shape_name(raw: &str) -> String {
    raw.rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `count` filters this listing admits the names of, so nothing but the LENGTH of the list
/// can be refused: one filter name is as valid repeated as it is once.
fn filters_named(name: &str, count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|_| json!({ "name": name, "values": ["anything"] }))
            .collect(),
    )
}

/// RULE: a filter naming a member a listing does not filter on is refused as a value that
/// listing's `filters` parameter does not admit, stating the parameter the request carried
/// and the names that operation admits.
#[tokio::test]
async fn an_unsupported_filter_name_states_the_names_that_listing_admits() {
    let router = emulator();

    // The names a row sends: another listing's member, a member of the resource that is not a
    // filter, a filter name spelled in the wrong case, and a name nothing at all names. Every
    // one matches the modelled `FilterName` pattern
    // (`smithy_query(query="shape",shape="FilterName")`: `^[a-zA-Z]{1,1024}$`), so each is
    // refused for the name it is rather than for the characters in it.
    let unsupported = ["owner", "arn", "Name", "nme"];

    // (case, the listing, the filters it sends around the refused one, the parameter the
    // refusal names, the requirement it states). The allowed-name lists are each the
    // operation's own `filters` member documentation
    // (`smithy_query(query="doc",shape="ListImagesRequest",member="filters")` and its
    // siblings), in the order that operation's own refusal states them.
    let rows: Vec<(&str, &str, usize, usize, &str, &str)> = vec![
        (
            "the image listing",
            "/ListImages",
            0,
            0,
            "filters",
            "Allowed filters for ListImages are: name, version, platform, osVersion, type.",
        ),
        (
            "the image listing, the refused filter last of three",
            "/ListImages",
            2,
            0,
            "filters",
            "Allowed filters for ListImages are: name, version, platform, osVersion, type.",
        ),
        (
            "the pipeline listing",
            "/ListImagePipelines",
            0,
            0,
            "filters[0].name",
            "Unsupported filter name. Allowed: name, description, status, imageRecipeArn, \
             infrastructureConfigurationArn, distributionConfigurationArn",
        ),
        (
            "the pipeline listing, the refused filter second",
            "/ListImagePipelines",
            1,
            0,
            "filters[1].name",
            "Unsupported filter name. Allowed: name, description, status, imageRecipeArn, \
             infrastructureConfigurationArn, distributionConfigurationArn",
        ),
        (
            "the pipeline listing, the refused filter last of three",
            "/ListImagePipelines",
            2,
            0,
            "filters[2].name",
            "Unsupported filter name. Allowed: name, description, status, imageRecipeArn, \
             infrastructureConfigurationArn, distributionConfigurationArn",
        ),
        (
            "the pipeline listing, the refused filter followed by two it admits",
            "/ListImagePipelines",
            0,
            2,
            "filters[0].name",
            "Unsupported filter name. Allowed: name, description, status, imageRecipeArn, \
             infrastructureConfigurationArn, distributionConfigurationArn",
        ),
        (
            "the component listing",
            "/ListComponents",
            0,
            0,
            "filters",
            "Allowed filters for ListComponents are: name, version, description, platform, \
             type, supportedOsVersion, productCodes, status.",
        ),
        (
            "the image-recipe listing",
            "/ListImageRecipes",
            0,
            0,
            "filters",
            "Allowed filters for ListImageRecipes are: name, parentImage, platform.",
        ),
    ];

    for (case, uri, before, after, parameter, requirement) in rows {
        for name in unsupported {
            let case = format!("{case}, the filter {name:?}");
            // The refused filter, with `before` filters the listing admits ahead of it and
            // `after` behind it: `name` is a member every one of these listings filters on.
            let mut filters: Vec<Value> = (0..before)
                .map(|_| json!({ "name": "name", "values": ["anything"] }))
                .collect();
            filters.push(json!({ "name": name, "values": ["anything"] }));
            filters.extend(
                (0..after).map(|_| json!({ "name": "name", "values": ["anything"] })),
            );
            let (status, header, code, message) =
                list(&router, uri, &json!({ "filters": filters })).await;
            assert_eq!(
                (status, header.as_str(), code.as_str()),
                (400, INVALID_PARAMETER_VALUE, INVALID_PARAMETER_VALUE),
                "case {case:?}: expected a 400 {INVALID_PARAMETER_VALUE}, got {status} \
                 {header:?}/{code:?} with {message:?}"
            );
            let expected =
                format!("The value supplied for parameter '{parameter}' is not valid. {requirement}");
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}

/// RULE: a collection a listing carries whose modelled `@length` bound it broke is refused as
/// that parameter's value, under one of this service's own modelled error shapes, stating the
/// member's requirement; one AT either bound is served.
#[tokio::test]
async fn a_collection_outside_its_modelled_length_is_refused_as_the_parameters_value() {
    let router = emulator();

    // (case, the listing, the body, the parameter the refusal names, the requirement it
    // states). The bound is the modelled one in every row; what differs is which collection
    // broke it, how far past it the request is, and which listing was addressed.
    let rows: Vec<(&str, &str, Value, &str, String)> = vec![
        (
            "the image listing, one filter past the maximum",
            "/ListImages",
            json!({ "filters": filters_named("name", MAX_FILTERS + 1) }),
            "filters",
            format!("filters must contain less than or equal to {MAX_FILTERS} items"),
        ),
        (
            "the image listing, far past the maximum",
            "/ListImages",
            json!({ "filters": filters_named("version", MAX_FILTERS * 3) }),
            "filters",
            format!("filters must contain less than or equal to {MAX_FILTERS} items"),
        ),
        (
            "the image listing, one filter's values past their maximum",
            "/ListImages",
            json!({ "filters": [{
                "name": "name",
                "values": (0..MAX_FILTERS + 1).map(|index| format!("value {index}")).collect::<Vec<_>>(),
            }] }),
            "filters[0].values",
            format!("filters[0].values must contain less than or equal to {MAX_FILTERS} items"),
        ),
        (
            "the pipeline listing, one filter past the maximum",
            "/ListImagePipelines",
            json!({ "filters": filters_named("name", MAX_FILTERS + 1) }),
            "filters",
            format!("filters length must satisfy max_items {MAX_FILTERS}"),
        ),
        (
            "the pipeline listing, far past the maximum",
            "/ListImagePipelines",
            json!({ "filters": filters_named("status", MAX_FILTERS * 2) }),
            "filters",
            format!("filters length must satisfy max_items {MAX_FILTERS}"),
        ),
        (
            "the pipeline listing, one filter's values past their maximum",
            "/ListImagePipelines",
            json!({ "filters": [{
                "name": "name",
                "values": (0..MAX_FILTERS + 1).map(|index| format!("value {index}")).collect::<Vec<_>>(),
            }] }),
            "filters[0].values",
            format!("filters[0].values length must satisfy max_items {MAX_FILTERS}"),
        ),
        (
            "the component listing, one filter past the maximum",
            "/ListComponents",
            json!({ "filters": filters_named("name", MAX_FILTERS + 1) }),
            "filters",
            format!("filters must contain less than or equal to {MAX_FILTERS} items"),
        ),
        (
            "the image-recipe listing, one filter past the maximum",
            "/ListImageRecipes",
            json!({ "filters": filters_named("name", MAX_FILTERS + 1) }),
            "filters",
            format!("filters must contain less than or equal to {MAX_FILTERS} items"),
        ),
        (
            // The other end of the same modelled bound, which this service has stated all
            // along: a list below the minimum states the count it had to reach.
            "the image listing, an empty filter list below the minimum",
            "/ListImages",
            json!({ "filters": [] }),
            "filters",
            "filters must contain at least 1 items".to_string(),
        ),
        (
            "the pipeline listing, an empty filter list below the minimum",
            "/ListImagePipelines",
            json!({ "filters": [] }),
            "filters",
            "filters length must satisfy min_items 1".to_string(),
        ),
        (
            "the pipeline listing, one filter admitting no value at all",
            "/ListImagePipelines",
            json!({ "filters": [{ "name": "name", "values": [] }] }),
            "filters[0].values",
            "filters[0].values length must satisfy min_items 1".to_string(),
        ),
        (
            "the component listing, an empty filter list below the minimum",
            "/ListComponents",
            json!({ "filters": [] }),
            "filters",
            "filters must contain at least 1 items".to_string(),
        ),
    ];

    for (case, uri, body, parameter, requirement) in rows {
        let (status, header, code, message) = list(&router, uri, &body).await;
        assert_eq!(
            (status, header.as_str(), code.as_str()),
            (400, INVALID_PARAMETER_VALUE, INVALID_PARAMETER_VALUE),
            "case {case:?}: expected a 400 {INVALID_PARAMETER_VALUE}, got {status} \
             {header:?}/{code:?} with {message:?}"
        );
        let expected =
            format!("The value supplied for parameter '{parameter}' is not valid. {requirement}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // AT the bound, both collections are served: the maximum itself is admitted, so the
    // refusals above are about the request being PAST it.
    for (case, uri, body) in [
        (
            "the image listing with as many filters as the maximum admits",
            "/ListImages",
            json!({ "filters": filters_named("name", MAX_FILTERS) }),
        ),
        (
            "the pipeline listing with as many filters as the maximum admits",
            "/ListImagePipelines",
            json!({ "filters": filters_named("name", MAX_FILTERS) }),
        ),
        (
            "the pipeline listing with one filter carrying as many values as the maximum admits",
            "/ListImagePipelines",
            json!({ "filters": [{
                "name": "name",
                "values": (0..MAX_FILTERS).map(|index| format!("value {index}")).collect::<Vec<_>>(),
            }] }),
        ),
        (
            "the pipeline listing with the fewest filters either bound admits",
            "/ListImagePipelines",
            json!({ "filters": filters_named("name", 1) }),
        ),
        (
            "the image listing with the fewest filters either bound admits",
            "/ListImages",
            json!({ "filters": filters_named("name", 1) }),
        ),
    ] {
        let (status, header, code, message) = list(&router, uri, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {header:?}/{code:?} with {message:?}"
        );
    }
}

/// RULE: a filter that left out a member every filter has to carry is reported as a missing
/// required parameter, naming that member of the filter.
#[tokio::test]
async fn a_filter_missing_a_member_is_reported_as_that_missing_parameter() {
    let router = emulator();

    /// How every listing but the pipeline one reports the omitted `member`: the sentence this
    /// service reports every parameter its model cannot require in.
    fn missing_member(member: &str) -> String {
        format!(
            "Required parameters are missing from the request: '{member}'. Verify that you \
             have entered all of the required parameters in your request and try again."
        )
    }

    /// How the PIPELINE listing reports it: the element and the member of it the request left
    /// out, so a request carrying several filters is told which one.
    fn missing_parameter(index: usize, member: &str) -> String {
        format!("Required parameters are missing: filters[{index}].{member}")
    }

    // Each listing of this service takes the same `Filter`, so the same omission is reported
    // whichever one was addressed and whichever member was left out.
    //
    // (case, the listing, the filters it sends, the report)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "the image listing, a filter carrying no values",
            "/ListImages",
            json!([{ "name": "name" }]),
            missing_member("values"),
        ),
        (
            "the image listing, a filter naming no member",
            "/ListImages",
            json!([{ "values": ["Alpha Image"] }]),
            missing_member("name"),
        ),
        (
            "the image listing, a filter carrying neither member",
            "/ListImages",
            json!([{}]),
            missing_member("name"),
        ),
        (
            "the pipeline listing, a filter carrying no values",
            "/ListImagePipelines",
            json!([{ "name": "status" }]),
            missing_parameter(0, "values"),
        ),
        (
            "the pipeline listing, a filter naming no member",
            "/ListImagePipelines",
            json!([{ "values": ["ENABLED"] }]),
            missing_parameter(0, "name"),
        ),
        (
            "the pipeline listing, the second of two filters carrying no values",
            "/ListImagePipelines",
            json!([{ "name": "name", "values": ["anything"] }, { "name": "status" }]),
            missing_parameter(1, "values"),
        ),
        (
            "the pipeline listing, the third of three filters naming no member",
            "/ListImagePipelines",
            json!([
                { "name": "name", "values": ["anything"] },
                { "name": "status", "values": ["ENABLED"] },
                { "values": ["anything"] },
            ]),
            missing_parameter(2, "name"),
        ),
        (
            "the component listing, a filter carrying no values",
            "/ListComponents",
            json!([{ "name": "name" }]),
            missing_member("values"),
        ),
        (
            "the image-recipe listing, a filter naming no member",
            "/ListImageRecipes",
            json!([{ "values": ["alpha-recipe"] }]),
            missing_member("name"),
        ),
        (
            "the workflow listing, a filter carrying no values",
            "/ListWorkflows",
            json!([{ "name": "name" }]),
            missing_member("values"),
        ),
    ];

    for (case, uri, filters, expected) in rows {
        let (status, header, code, message) =
            list(&router, uri, &json!({ "filters": filters })).await;
        assert_eq!(
            (status, header.as_str(), code.as_str()),
            (400, MISSING_PARAMETERS, MISSING_PARAMETERS),
            "case {case:?}: expected a 400 {MISSING_PARAMETERS}, got {status} \
             {header:?}/{code:?} with {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}
