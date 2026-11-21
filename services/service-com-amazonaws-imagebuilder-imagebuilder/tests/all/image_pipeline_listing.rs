// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the pipeline listing: which pipelines it reports, which member
//! each of its filters selects on, that its entries are the view a read of one pipeline
//! reports, and which page a token stands for.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * a listing reports the pipelines the Region it was served in holds, and none of another
//!   Region's — a regional endpoint serves only its own Region's resources;
//! * each filter the request carries names ONE member of a pipeline and the values that
//!   member may take, so a pipeline is reported only when EVERY filter the request carries
//!   admits the value it reports for that member, and a filter naming a member the listing
//!   does not filter on is refused rather than ignored. The members a pipeline listing
//!   filters on are its own `filters` member's list
//!   (`smithy_query(query="doc",shape="ListImagePipelinesRequest",member="filters")`:
//!   description, distributionConfigurationArn, imageRecipeArn,
//!   infrastructureConfigurationArn, name, status);
//! * a listing entry is the same `ImagePipeline` view `GetImagePipeline` reports for that
//!   pipeline, so a listing and a read cannot disagree about one pipeline;
//! * a `nextToken` stands for a position in ONE listing — one operation, one Region and one
//!   set of filters — so a token answered for one query names no position in another and is
//!   refused rather than guessed at.
//!
//! The rows span what the rules generalise over: which member a filter names, how many values
//! it admits, how many filters one request carries, which Region a pipeline lives in, and the
//! page size the listing is read at.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for unless a case says otherwise.
const REGION: &str = "us-west-2";

/// A second Region, so a listing's own Region can be told from another's.
const OTHER_REGION: &str = "eu-west-1";

/// The error this service refuses a parameter value with
/// (`smithy_query(query="shape",shape="InvalidParameterValueException")`: a client error,
/// `@httpError` 400).
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The sentence a listing refuses a `nextToken` it did not issue for that query with: the
/// token is the refused value and is never echoed.
const INVALID_TOKEN_MESSAGE: &str = "The value supplied for parameter 'nextToken' is not \
                                     valid. The provided token does not meet the required \
                                     format.";

/// The class a filter that left out a member every filter has to carry is reported under
/// (`crate::state::MISSING_PARAMETERS_CODE`): the member is not one the model can require, so
/// the report is this service's missing-parameter class rather than a modelled error.
const MISSING_PARAMETERS: &str = "MissingParametersException";

/// Why a filter of a PIPELINE listing that left `member` out is reported: the
/// missing-parameter report, naming the element and the member of it the request omitted.
fn missing_filter_member(index: usize, member: &str) -> String {
    format!("Required parameters are missing: filters[{index}].{member}")
}

/// The sentence a filter whose value list is empty is refused with: the modelled `values`
/// length (`smithy_query(query="shape",shape="Filter")`: 1..10) stated as the requirement,
/// naming the member by its path through the request. This listing states a collection's
/// length requirement as the modelled bound it had to satisfy; `tests/all/filter_list_refusals.rs`
/// spans that wording and the one every other operation of this service states.
const EMPTY_FILTER_VALUES_MESSAGE: &str = "The value supplied for parameter 'filters[0].values' \
                                           is not valid. filters[0].values length must satisfy \
                                           min_items 1";

/// A build component document: one `build` phase, which is all the recipe these cases name
/// needs.
const BUILD_DOCUMENT: &str = "name: HelloWorld\nschemaVersion: 1.0\nphases:\n  - name: build\n    \
                              steps:\n      - name: Greet\n        action: ExecuteBash\n        \
                              inputs:\n          commands:\n            - echo hello\n";

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

/// A request signed for `imagebuilder` in `region`.
fn request(
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> http::Request<bytes::Bytes> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("imagebuilder.{region}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDIMAGEBUILDER/20260101/{region}/imagebuilder/\
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

/// Status and parsed body of one call made in `region`.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let response = router.dispatch(request(region, method, uri, body)).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// Status and parsed body of one call made in [`REGION`].
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    call_in(router, REGION, method, uri, body).await
}

/// The error code a refusal carries, from `__type`.
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an error shape, got {body}"))
        .rsplit(['#', '/'])
        .next()
        .expect("a non-empty error type")
        .to_string()
}

/// The text one member of a response carries.
fn text(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {name} of {body}"))
        .to_string()
}

/// Assert a call succeeded, and answer its body.
fn assert_ok(case: &str, status: u16, body: Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {body}"
    );
    body
}

/// `CreateComponent` for one build version of `name` in `region`, answering its build
/// version ARN.
async fn create_component(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "platform": "Linux",
        "clientToken": format!("component-{region}-{name}"),
        "data": BUILD_DOCUMENT,
    });
    let case = format!("create the component {name} in {region}");
    let (status, answer) = call_in(router, region, "PUT", "/CreateComponent", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "componentBuildVersionArn")
}

/// `CreateImageRecipe` for `name` in `region`, answering the recipe's ARN.
async fn create_recipe(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    component_arn: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "parentImage": "ami-0123456789abcdef0",
        "clientToken": format!("recipe-{region}-{name}"),
        "components": [{ "componentArn": component_arn }],
    });
    let case = format!("create the image recipe {name} in {region}");
    let (status, answer) = call_in(router, region, "PUT", "/CreateImageRecipe", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageRecipeArn")
}

/// `CreateInfrastructureConfiguration` for `name` in `region`, answering its ARN.
async fn create_configuration(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("configuration-{region}-{name}"),
        "instanceProfileName": "EC2InstanceProfileForImageBuilder",
    });
    let case = format!("create the infrastructure configuration {name} in {region}");
    let (status, answer) = call_in(
        router,
        region,
        "PUT",
        "/CreateInfrastructureConfiguration",
        Some(&body),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "infrastructureConfigurationArn")
}

/// `CreateDistributionConfiguration` for `name` in [`REGION`], answering its ARN.
async fn create_distribution(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("distribution-{name}"),
        "distributions": [{ "region": REGION, "amiDistributionConfiguration": {} }],
    });
    let case = format!("create the distribution configuration {name}");
    let (status, answer) = call(router, "PUT", "/CreateDistributionConfiguration", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "distributionConfigurationArn")
}

/// `CreateImagePipeline` in `region` for `name`, over whatever `extra` names beside the
/// recipe and the infrastructure configuration, answering the pipeline's ARN.
async fn create_pipeline(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    recipe: &str,
    configuration: &str,
    extra: &[(&str, Value)],
) -> String {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert(
        "clientToken".to_string(),
        json!(format!("pipeline-{region}-{name}")),
    );
    body.insert("imageRecipeArn".to_string(), json!(recipe));
    body.insert(
        "infrastructureConfigurationArn".to_string(),
        json!(configuration),
    );
    for (member, value) in extra {
        body.insert((*member).to_string(), value.clone());
    }
    let case = format!("create the pipeline {name} in {region}");
    let (status, answer) = call_in(
        router,
        region,
        "PUT",
        "/CreateImagePipeline",
        Some(&Value::Object(body)),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imagePipelineArn")
}

/// `ListImagePipelines` in `region` with `body`.
async fn list_in(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in(router, region, "POST", "/ListImagePipelines", Some(body)).await
}

/// The ARNs one `ListImagePipelines` answer carries, in the order it reports them.
fn listed_arns(case: &str, body: &Value) -> Vec<String> {
    body.get("imagePipelineList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected an imagePipelineList in {body}"))
        .iter()
        .map(|entry| text(case, entry, "arn"))
        .collect()
}

/// The ARNs a successful listing of `body` in `region` reports.
async fn listed(router: &MultiServiceRouter, region: &str, case: &str, body: &Value) -> Vec<String> {
    let (status, answer) = list_in(router, region, body).await;
    let answer = assert_ok(case, status, answer);
    listed_arns(case, &answer)
}

/// The fixtures one Region holds: the pipelines created in it, by the name they carry.
struct Fixtures {
    /// The image recipe every pipeline of the Region names.
    recipe: String,
    /// The infrastructure configuration two of its pipelines name.
    configuration_a: String,
    /// The infrastructure configuration its remaining pipeline names.
    configuration_b: String,
    /// The distribution configuration one of its pipelines names (only in [`REGION`]).
    distribution: Option<String>,
    /// Every pipeline's ARN, paired with the name it was created under.
    pipelines: Vec<(&'static str, String)>,
}

/// The ARN of the pipeline created under `name`.
fn pipeline(fixtures: &Fixtures, name: &str) -> String {
    fixtures
        .pipelines
        .iter()
        .find(|(created, _)| *created == name)
        .map(|(_, arn)| arn.clone())
        .unwrap_or_else(|| panic!("the fixture pipeline {name:?} must exist"))
}

/// Create one Region's fixtures: a component, a recipe, two infrastructure configurations,
/// a distribution configuration in [`REGION`] only, and three pipelines differing in the
/// members a filter selects on.
async fn fixtures(router: &MultiServiceRouter, region: &str) -> Fixtures {
    let component = create_component(router, region, "pipelines-component").await;
    let recipe = create_recipe(router, region, "pipelines-recipe", &component).await;
    let configuration_a = create_configuration(router, region, "pipelines-infra-a").await;
    let configuration_b = create_configuration(router, region, "pipelines-infra-b").await;
    let distribution = if region == REGION {
        Some(create_distribution(router, "pipelines-distribution").await)
    } else {
        None
    };

    let mut pipelines = Vec::new();
    let mut first_extras: Vec<(&str, Value)> = vec![
        ("status", json!("ENABLED")),
        ("description", json!("the first pipeline")),
    ];
    if let Some(distribution) = &distribution {
        first_extras.push(("distributionConfigurationArn", json!(distribution)));
    }
    pipelines.push((
        "pipelines-one",
        create_pipeline(
            router,
            region,
            "pipelines-one",
            &recipe,
            &configuration_a,
            &first_extras,
        )
        .await,
    ));
    pipelines.push((
        "pipelines-two",
        create_pipeline(
            router,
            region,
            "pipelines-two",
            &recipe,
            &configuration_b,
            &[
                ("status", json!("DISABLED")),
                ("description", json!("a shared description")),
            ],
        )
        .await,
    ));
    pipelines.push((
        "pipelines-three",
        create_pipeline(
            router,
            region,
            "pipelines-three",
            &recipe,
            &configuration_a,
            &[
                ("status", json!("ENABLED")),
                ("description", json!("a shared description")),
            ],
        )
        .await,
    ));

    Fixtures {
        recipe,
        configuration_a,
        configuration_b,
        distribution,
        pipelines,
    }
}

/// The ARNs of `names`, in the order the listing reports them (by ARN, which carries the
/// pipeline's normalized name).
fn expected(fixtures: &Fixtures, names: &[&str]) -> Vec<String> {
    let mut arns: Vec<String> = names.iter().map(|name| pipeline(fixtures, name)).collect();
    arns.sort();
    arns
}

#[tokio::test]
async fn each_pipeline_filter_selects_on_its_own_member_and_filters_combine() {
    let router = emulator();
    let held = fixtures(&router, REGION).await;
    let distribution = held
        .distribution
        .clone()
        .expect("this Region's fixtures carry a distribution configuration");

    // (case, the filters the request carries, the pipelines it answers with)
    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "no filter at all",
            json!({}),
            expected(&held, &["pipelines-one", "pipelines-two", "pipelines-three"]),
        ),
        (
            "a filter on the name",
            json!([{ "name": "name", "values": ["pipelines-two"] }]),
            expected(&held, &["pipelines-two"]),
        ),
        (
            "a filter on the name, naming two values",
            json!([{ "name": "name", "values": ["pipelines-two", "pipelines-three"] }]),
            expected(&held, &["pipelines-two", "pipelines-three"]),
        ),
        (
            "a filter on a name nothing carries",
            json!([{ "name": "name", "values": ["pipelines-absent"] }]),
            Vec::new(),
        ),
        (
            "a filter on the description two pipelines share",
            json!([{ "name": "description", "values": ["a shared description"] }]),
            expected(&held, &["pipelines-two", "pipelines-three"]),
        ),
        (
            "a filter on the status",
            json!([{ "name": "status", "values": ["ENABLED"] }]),
            expected(&held, &["pipelines-one", "pipelines-three"]),
        ),
        (
            "a filter on the infrastructure configuration two pipelines name",
            json!([{
                "name": "infrastructureConfigurationArn",
                "values": [held.configuration_a.clone()],
            }]),
            expected(&held, &["pipelines-one", "pipelines-three"]),
        ),
        (
            "a filter on the infrastructure configuration one pipeline names",
            json!([{
                "name": "infrastructureConfigurationArn",
                "values": [held.configuration_b.clone()],
            }]),
            expected(&held, &["pipelines-two"]),
        ),
        (
            "a filter on the distribution configuration one pipeline names",
            json!([{
                "name": "distributionConfigurationArn",
                "values": [distribution.clone()],
            }]),
            expected(&held, &["pipelines-one"]),
        ),
        (
            "a filter on the image recipe every pipeline names",
            json!([{ "name": "imageRecipeArn", "values": [held.recipe.clone()] }]),
            expected(&held, &["pipelines-one", "pipelines-two", "pipelines-three"]),
        ),
        (
            "two filters, both admitting two pipelines",
            json!([
                { "name": "status", "values": ["ENABLED"] },
                {
                    "name": "infrastructureConfigurationArn",
                    "values": [held.configuration_a.clone()],
                },
            ]),
            expected(&held, &["pipelines-one", "pipelines-three"]),
        ),
        (
            "two filters, admitting one pipeline between them",
            json!([
                { "name": "description", "values": ["a shared description"] },
                { "name": "status", "values": ["ENABLED"] },
            ]),
            expected(&held, &["pipelines-three"]),
        ),
        (
            "two filters no one pipeline satisfies",
            json!([
                { "name": "name", "values": ["pipelines-two"] },
                { "name": "status", "values": ["ENABLED"] },
            ]),
            Vec::new(),
        ),
    ];

    for (case, filters, want) in rows {
        let body = match filters {
            Value::Array(entries) => json!({ "filters": entries }),
            other => other,
        };
        let arns = listed(&router, REGION, case, &body).await;
        assert_eq!(arns, want, "case {case:?}: expected {want:?}, got {arns:?}");
    }

    // A filter naming a member the listing does not filter on is refused rather than
    // ignored, whichever member it names.
    for member in ["platform", "arn", "dateCreated"] {
        let case = format!("a filter on the unlisted member {member}");
        let (status, answer) = list_in(
            &router,
            REGION,
            &json!({ "filters": [{ "name": member, "values": ["anything"] }] }),
        )
        .await;
        let code = error_code(&case, &answer);
        assert_eq!(
            (status, code.as_str()),
            (400, INVALID_PARAMETER_VALUE),
            "case {case:?}: got {answer}"
        );
        let message = text(&case, &answer, "message");
        assert_eq!(
            message,
            "The value supplied for parameter 'filters[0].name' is not valid. Unsupported \
             filter name. Allowed: name, description, status, imageRecipeArn, \
             infrastructureConfigurationArn, distributionConfigurationArn",
            "case {case:?}: got {message:?}"
        );
    }

    // A filter carrying no name, and one carrying no values, left out a member every filter
    // has to carry, so each is reported as the parameter the request omitted. A filter whose
    // value list is EMPTY carried the member and is refused one step earlier, by the modelled
    // constraint the `values` member carries
    // (`smithy_query(query="shape",shape="Filter")`: `length` 1..10), which is this service's
    // parameter-value refusal.
    for (case, filter, code, message) in [
        (
            "a filter with no name",
            json!({ "values": ["pipelines-two"] }),
            MISSING_PARAMETERS,
            missing_filter_member(0, "name"),
        ),
        (
            "a filter with no values",
            json!({ "name": "name" }),
            MISSING_PARAMETERS,
            missing_filter_member(0, "values"),
        ),
        (
            "a filter with an empty value list",
            json!({ "name": "name", "values": [] }),
            INVALID_PARAMETER_VALUE,
            EMPTY_FILTER_VALUES_MESSAGE.to_string(),
        ),
    ] {
        let (status, answer) = list_in(&router, REGION, &json!({ "filters": [filter] })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answer}"
        );
        let actual_code = error_code(case, &answer);
        assert_eq!(
            actual_code, code,
            "case {case:?}: expected {code:?}, got {actual_code:?} with {answer}"
        );
        let actual_message = text(case, &answer, "message");
        assert_eq!(
            actual_message, message,
            "case {case:?}: expected the message {message:?}, got {actual_message:?}"
        );
    }
}

#[tokio::test]
async fn a_pipeline_listing_reports_only_its_own_region() {
    let router = emulator();
    let here = fixtures(&router, REGION).await;
    let elsewhere = fixtures(&router, OTHER_REGION).await;

    // (case, the Region the listing is served in, the pipelines it answers with)
    let rows = [
        (
            "this Region's listing",
            REGION,
            expected(&here, &["pipelines-one", "pipelines-two", "pipelines-three"]),
        ),
        (
            "the other Region's listing",
            OTHER_REGION,
            expected(
                &elsewhere,
                &["pipelines-one", "pipelines-two", "pipelines-three"],
            ),
        ),
    ];
    for (case, region, want) in rows {
        let arns = listed(&router, region, case, &json!({})).await;
        assert_eq!(arns, want, "case {case:?}: expected {want:?}, got {arns:?}");
        for arn in &arns {
            assert!(
                arn.contains(&format!(":imagebuilder:{region}:")),
                "case {case:?}: expected an ARN of {region}, got {arn:?}"
            );
        }
    }

    // A filter whose value names the OTHER Region's infrastructure configuration selects
    // nothing here, because no pipeline this Region holds names it.
    let arns = listed(
        &router,
        REGION,
        "a filter on another Region's configuration",
        &json!({ "filters": [{
            "name": "infrastructureConfigurationArn",
            "values": [elsewhere.configuration_a.clone()],
        }] }),
    )
    .await;
    assert!(
        arns.is_empty(),
        "case \"a filter on another Region's configuration\": expected nothing, got {arns:?}"
    );
}

#[tokio::test]
async fn a_listing_entry_is_the_view_a_pipeline_read_reports() {
    let router = emulator();
    let held = fixtures(&router, REGION).await;

    let (status, answer) = list_in(&router, REGION, &json!({})).await;
    let answer = assert_ok("the whole listing", status, answer);
    let entries = answer
        .get("imagePipelineList")
        .and_then(Value::as_array)
        .expect("the listing carries an imagePipelineList")
        .clone();
    assert_eq!(
        entries.len(),
        held.pipelines.len(),
        "case \"the whole listing\": expected {} entries, got {}",
        held.pipelines.len(),
        entries.len()
    );

    for entry in &entries {
        let arn = text("a listed entry", entry, "arn");
        let case = format!("the read of {arn}");
        let (status, read) = call(
            &router,
            "GET",
            &format!("/GetImagePipeline?imagePipelineArn={arn}"),
            None,
        )
        .await;
        let read = assert_ok(&case, status, read);
        let view = read
            .get("imagePipeline")
            .unwrap_or_else(|| panic!("case {case:?}: expected an imagePipeline in {read}"));
        assert_eq!(
            entry, view,
            "case {case:?}: expected the listing entry to be the read's view, got {entry} and \
             {view}"
        );
    }
}

#[tokio::test]
async fn a_page_token_names_a_position_in_the_listing_it_was_issued_for() {
    let router = emulator();
    let held = fixtures(&router, REGION).await;
    let all = expected(&held, &["pipelines-one", "pipelines-two", "pipelines-three"]);

    // Read the whole listing at each page size from one entry to more than it holds: the
    // pages together are the listing, in its one order, and a token is answered only while
    // an entry remains.
    for page_size in 1..=(all.len() + 1) {
        let case = format!("{page_size} entries per page");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut body = Map::new();
            body.insert("maxResults".to_string(), json!(page_size));
            if let Some(token) = &token {
                body.insert("nextToken".to_string(), json!(token));
            }
            let (status, answer) = list_in(&router, REGION, &Value::Object(body)).await;
            let answer = assert_ok(&case, status, answer);
            let page = listed_arns(&case, &answer);
            assert!(
                page.len() <= page_size,
                "case {case:?}: expected at most {page_size} entries, got {page:?}"
            );
            seen.extend(page);
            pages += 1;
            assert!(
                pages <= all.len() + 1,
                "case {case:?}: the pages did not end, got {seen:?}"
            );
            token = answer
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, all,
            "case {case:?}: expected the whole listing, got {seen:?}"
        );
    }

    // The token one query answers names a position in THAT query. Take one from the
    // unfiltered listing...
    let (status, answer) = list_in(&router, REGION, &json!({ "maxResults": 1 })).await;
    let answer = assert_ok("the first page of the unfiltered listing", status, answer);
    let token = text("the first page of the unfiltered listing", &answer, "nextToken");

    // ...and it is refused by every listing it was not issued for: another set of filters,
    // another Region, and a value this store never minted.
    let refusals: Vec<(&str, &str, Value)> = vec![
        (
            "the unfiltered listing's token, presented with a filter",
            REGION,
            json!({ "maxResults": 1, "nextToken": token.clone(), "filters": [
                { "name": "status", "values": ["ENABLED"] }
            ] }),
        ),
        (
            "the unfiltered listing's token, presented in another Region",
            OTHER_REGION,
            json!({ "maxResults": 1, "nextToken": token.clone() }),
        ),
        (
            "a token this store never issued",
            REGION,
            json!({ "nextToken": "bm90LWEtcGFnZS10b2tlbg==" }),
        ),
        (
            "a token this store never issued, built from one it did",
            REGION,
            json!({ "nextToken": format!("{token}x") }),
        ),
    ];
    for (case, region, body) in refusals {
        let (status, answer) = list_in(&router, region, &body).await;
        let code = error_code(case, &answer);
        assert_eq!(
            (status, code.as_str()),
            (400, INVALID_PARAMETER_VALUE),
            "case {case:?}: got {answer}"
        );
        let message = text(case, &answer, "message");
        assert_eq!(
            message, INVALID_TOKEN_MESSAGE,
            "case {case:?}: got {message:?}"
        );
    }

    // Presented to the listing it WAS issued for, the same token answers the next page.
    let arns = listed(
        &router,
        REGION,
        "the token presented to its own listing",
        &json!({ "maxResults": 1, "nextToken": token }),
    )
    .await;
    assert_eq!(
        arns,
        vec![all[1].clone()],
        "case \"the token presented to its own listing\": expected {:?}, got {arns:?}",
        vec![all[1].clone()]
    );
}
