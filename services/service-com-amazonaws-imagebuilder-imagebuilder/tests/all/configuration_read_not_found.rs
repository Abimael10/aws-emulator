// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the sentence a call naming one of this service's CONFIGURATION
//! resources carries when the ARN it names holds nothing.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes.
//!
//! The rule under test: a call that finds nothing stored under the ARN it names in the
//! calling Region is refused with the not-found error this service models
//! (`ResourceNotFoundException`, HTTP 404), and the message that refusal carries is the
//! template of the RESOURCE KIND the ARN identifies, not of the operation that looked it up:
//!
//! * the INFRASTRUCTURE configuration is reported by the ARN the request carried, quoted,
//!   naming no kind — "The resource ARN '<arn>' was not found." — for its read and for its
//!   delete alike;
//! * every other resource of this service, the DISTRIBUTION configuration included, is
//!   reported as the required resource it is, naming the kind its ARN identifies — "The
//!   following required resource '<Kind>' cannot be found: '<arn>'." — whether the call
//!   reads one, deletes one, updates one, tags one or resolves a reference to one.
//!
//! The rows span what the rule generalises over: which configuration family the ARN names,
//! which operation performs the lookup, the form of the configuration name the ARN carries,
//! and why the ARN resolves to nothing (never created, deleted, a name that only resembles a
//! stored one, a stored configuration reached from another Region's endpoint).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region a case signs for when it only needs one; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// A second Region, for the rows that read a stored configuration from an endpoint of
/// another Region.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// A build component document: one `build` phase, which is all the component the contrast
/// rows look up needs.
const BUILD_DOCUMENT: &str = "name: HelloWorld\nschemaVersion: 1.0\nphases:\n  - name: build\n    steps:\n      - name: Greet\n        action: ExecuteBash\n        inputs:\n          commands:\n            - echo hello\n";

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

/// A request signed for `imagebuilder` in `region` — the signal the router routes on and the
/// handlers read the Region from.
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

/// Status and parsed body of one call; the error code is folded into the body as `__type`, so
/// the header and the body are read as one.
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

/// Percent-encode one path segment, as an AWS SDK encodes a NON-greedy `@httpLabel`: the `:`
/// and `/` of an ARN travel as `%3A` and `%2F`, so the whole ARN stays ONE segment.
fn encode_label(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The error code a refusal carries, from `__type` (restJson1 renders the shape there,
/// optionally namespace-qualified).
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an error shape, got {body}"))
        .rsplit(['#', '/'])
        .next()
        .expect("a non-empty error type")
        .to_string()
}

/// The text one member of an answer carries.
fn text(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {member} of {body}"))
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

/// Which of this service's two missing-resource sentences an answer has to carry.
#[derive(Clone, Copy)]
enum Sentence {
    /// The one the infrastructure configuration carries: the ARN the request named, quoted.
    Arn,
    /// The one every other report carries: the kind the ARN identifies, and the ARN.
    Kind(&'static str),
}

/// The whole message `sentence` requires for `arn`.
fn expected_message(sentence: Sentence, arn: &str) -> String {
    match sentence {
        Sentence::Arn => format!("The resource ARN '{arn}' was not found."),
        Sentence::Kind(kind) => {
            format!("The following required resource '{kind}' cannot be found: '{arn}'.")
        }
    }
}

/// One call that has to be refused as missing: how to send it, the ARN its report has to
/// state, and the sentence that report has to carry.
struct Probe {
    /// What this row is, named in every assertion it makes.
    case: String,
    /// The Region the request is signed for.
    region: &'static str,
    /// The HTTP method the modelled operation binds.
    method: &'static str,
    /// The request target, the ARN included where the operation takes it in the URI.
    uri: String,
    /// The request body, for an operation that takes the ARN in one.
    body: Option<Value>,
    /// The ARN the report has to state, verbatim as the request carried it.
    arn: String,
    /// Which of the two sentences the report has to carry.
    sentence: Sentence,
}

/// Assert one probe's call was refused as the modelled not-found error carrying its sentence.
async fn assert_reports_missing(router: &MultiServiceRouter, probe: &Probe) {
    let case = probe.case.as_str();
    let (status, answer) = call_in(
        router,
        probe.region,
        probe.method,
        &probe.uri,
        probe.body.as_ref(),
    )
    .await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404, got {status} with {answer}"
    );
    let code = error_code(case, &answer);
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {code:?} with {answer}"
    );
    let expected = expected_message(probe.sentence, &probe.arn);
    let actual = text(case, &answer, "message");
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the message {expected:?}, got {actual:?}"
    );
}

/// The body a `CreateInfrastructureConfiguration` of `name` carries in `region`: the name,
/// an idempotency token of its own, and the instance profile the model requires.
fn infrastructure_body(region: &str, name: &str) -> Value {
    json!({
        "name": name,
        "clientToken": format!("infra-{region}-{name}"),
        "instanceProfileName": "EC2InstanceProfileForImageBuilder",
    })
}

/// The body a `CreateDistributionConfiguration` of `name` carries in `region`: one target
/// Region with one output, which is the least a distribution entry may carry.
fn distribution_body(region: &str, name: &str) -> Value {
    json!({
        "name": name,
        "clientToken": format!("dist-{region}-{name}"),
        "distributions": [{ "region": region, "amiDistributionConfiguration": {} }],
    })
}

/// `Create<Family>Configuration` for `name` in `region`, answering the ARN it minted.
async fn create_configuration(
    router: &MultiServiceRouter,
    family: Family,
    region: &str,
    name: &str,
) -> String {
    let body = (family.body)(region, name);
    let case = format!("create {} {name} in {region}", family.label);
    let (status, answer) = call_in(router, region, "PUT", family.create, Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, family.minted)
}

/// `CreateComponent` for one build version of `name`, answering its build version ARN.
async fn create_component(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "platform": "Linux",
        "clientToken": format!("component-{name}"),
        "data": BUILD_DOCUMENT,
    });
    let case = format!("create the component {name}");
    let (status, answer) = call_in(router, REGION, "PUT", "/CreateComponent", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "componentBuildVersionArn")
}

/// Which configuration family a row is about: how one is created, read and deleted, and the
/// member each operation names it by.
#[derive(Clone, Copy)]
struct Family {
    /// What this family is, named in the cases of every row about it.
    label: &'static str,
    /// The resource type an ARN of this family carries, as the modelled ARN pattern spells
    /// it (`smithy_query(query="shape",shape="ImageBuilderArn")`): the configurations are the
    /// two kinds whose resource field is a plain `<kind>/<name>`.
    resource: &'static str,
    /// The create that stores one.
    create: &'static str,
    /// The body that create carries, for a Region and a name.
    body: fn(&str, &str) -> Value,
    /// The output member the create answers the minted ARN in.
    minted: &'static str,
    /// The read under test, up to the ARN it takes in its query string.
    read: &'static str,
    /// The delete that clears one, up to the ARN it takes in its query string.
    delete: &'static str,
    /// The sentence a report of this family's missing resource carries.
    sentence: Sentence,
}

/// The infrastructure configuration, as the model addresses it.
const INFRASTRUCTURE: Family = Family {
    label: "the infrastructure configuration",
    resource: "infrastructure-configuration",
    create: "/CreateInfrastructureConfiguration",
    body: infrastructure_body,
    minted: "infrastructureConfigurationArn",
    read: "/GetInfrastructureConfiguration?infrastructureConfigurationArn=",
    delete: "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn=",
    sentence: Sentence::Arn,
};

/// The distribution configuration, as the model addresses it.
const DISTRIBUTION: Family = Family {
    label: "the distribution configuration",
    resource: "distribution-configuration",
    create: "/CreateDistributionConfiguration",
    body: distribution_body,
    minted: "distributionConfigurationArn",
    read: "/GetDistributionConfiguration?distributionConfigurationArn=",
    delete: "/DeleteDistributionConfiguration?distributionConfigurationArn=",
    sentence: Sentence::Kind("DistributionConfiguration"),
};

/// The ARN a configuration of `family` named `name` carries in `region`, as the modelled
/// patterns lay it out — `DistributionConfigurationArn` and `InfrastructureConfigurationArn`
/// both read `arn:aws[^:]*:imagebuilder:<region>:<account>:<kind>/[a-z0-9-_]+`
/// (`smithy_query(query="shape",shape="DistributionConfigurationArn,InfrastructureConfigurationArn")`).
fn arn_of(family: Family, region: &str, name: &str) -> String {
    format!(
        "arn:aws:imagebuilder:{region}:{ACCOUNT}:{}/{name}",
        family.resource
    )
}

#[tokio::test]
async fn a_read_of_a_configuration_this_region_holds_nothing_under_carries_its_kinds_sentence()
{
    for family in [INFRASTRUCTURE, DISTRIBUTION] {
        let router = emulator();

        // One stored configuration of this family per name form the modelled pattern admits,
        // so the ARN every sentence below carries differs between the rows and cannot be a
        // constant a handler holds. Each reads back before anything is deleted.
        let mut stored = Vec::new();
        for name in ["kept-one", "kept_two_2", "k33"] {
            let arn = create_configuration(&router, family, REGION, name).await;
            let case = format!("{} {name} read back after its create", family.label);
            let (status, read) =
                call_in(&router, REGION, "GET", &format!("{}{arn}", family.read), None).await;
            assert_ok(&case, status, read);
            stored.push(arn);
        }

        // The configuration this family deletes, so a row can read an ARN that WAS stored.
        let deleted = stored[0].clone();
        let (status, cleared) = call_in(
            &router,
            REGION,
            "DELETE",
            &format!("{}{deleted}", family.delete),
            None,
        )
        .await;
        assert_ok(
            &format!("{}: the delete that clears it", family.label),
            status,
            cleared,
        );

        // Every row is a well-formed ARN of this family that the read finds nothing under,
        // and the rows differ in WHY it finds nothing: it was deleted, nothing ever created
        // it, its name only resembles a stored one, or it is a stored configuration of this
        // Region asked for through another Region's endpoint.
        let rows: Vec<(&str, &'static str, String)> = vec![
            ("a configuration that was deleted", REGION, deleted.clone()),
            (
                "a name nothing created",
                REGION,
                arn_of(family, REGION, "absent-one"),
            ),
            (
                "a one-token name nothing created",
                REGION,
                arn_of(family, REGION, "absent_two_2"),
            ),
            (
                "a name that only resembles a stored one",
                REGION,
                arn_of(family, REGION, "kept-one-2"),
            ),
            (
                "a stored configuration read from another Region's endpoint",
                OTHER_REGION,
                stored[1].clone(),
            ),
            (
                "a name of another Region nothing created",
                REGION,
                arn_of(family, OTHER_REGION, "k33"),
            ),
        ];

        for (row, region, arn) in rows {
            let probe = Probe {
                case: format!("{} on {row}", family.label),
                region,
                method: "GET",
                uri: format!("{}{arn}", family.read),
                body: None,
                arn,
                sentence: family.sentence,
            };
            assert_reports_missing(&router, &probe).await;
        }

        // The reads that are NOT about an absent ARN still answer the stored configurations,
        // so the rows above report what is absent rather than refusing everything.
        for arn in &stored[1..] {
            let case = format!("{}: the configurations left standing", family.label);
            let (status, read) =
                call_in(&router, REGION, "GET", &format!("{}{arn}", family.read), None).await;
            assert_ok(&case, status, read);
        }
    }
}

#[tokio::test]
async fn every_other_missing_resource_report_names_the_kind() {
    let router = emulator();
    let component = create_component(&router, "boundary-component").await;
    let distribution = create_configuration(&router, DISTRIBUTION, REGION, "boundary-dist").await;

    // An ARN of each family a row needs, naming nothing: the two versioned kinds, whose
    // resource field carries a version, and the distribution configuration.
    let absent_component =
        format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:component/absent-component/1.0.0/1");
    let absent_recipe =
        format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:image-recipe/absent-recipe/1.0.0");
    let absent_distribution = arn_of(DISTRIBUTION, REGION, "absent-dist");

    // The rows span the reports that keep the kind-naming sentence: the reads of the two
    // versioned families, a MUTATION of a configuration, a tagging write, and the reference
    // a create has to resolve.
    let rows: Vec<Probe> = vec![
        Probe {
            case: "a read of a component build version nothing created".to_string(),
            region: REGION,
            method: "GET",
            uri: format!("/GetComponent?componentBuildVersionArn={absent_component}"),
            body: None,
            arn: absent_component.clone(),
            sentence: Sentence::Kind("Component"),
        },
        Probe {
            case: "a read of an image recipe nothing created".to_string(),
            region: REGION,
            method: "GET",
            uri: format!("/GetImageRecipe?imageRecipeArn={absent_recipe}"),
            body: None,
            arn: absent_recipe.clone(),
            sentence: Sentence::Kind("ImageRecipe"),
        },
        Probe {
            case: "a delete of a distribution configuration nothing created".to_string(),
            region: REGION,
            method: "DELETE",
            uri: format!(
                "/DeleteDistributionConfiguration?distributionConfigurationArn=\
                 {absent_distribution}"
            ),
            body: None,
            arn: absent_distribution.clone(),
            sentence: Sentence::Kind("DistributionConfiguration"),
        },
        Probe {
            case: "an update of a distribution configuration nothing created".to_string(),
            region: REGION,
            method: "PUT",
            uri: "/UpdateDistributionConfiguration".to_string(),
            body: Some(json!({
                "distributionConfigurationArn": absent_distribution,
                "clientToken": "boundary-update",
                "distributions": [{ "region": REGION, "amiDistributionConfiguration": {} }],
            })),
            arn: absent_distribution.clone(),
            sentence: Sentence::Kind("DistributionConfiguration"),
        },
        Probe {
            case: "a tagging write naming a distribution configuration nothing created"
                .to_string(),
            region: REGION,
            method: "POST",
            uri: format!("/tags/{}", encode_label(&absent_distribution)),
            body: Some(json!({ "tags": { "Owner": "platform" } })),
            arn: absent_distribution.clone(),
            sentence: Sentence::Kind("DistributionConfiguration"),
        },
        Probe {
            case: "a create resolving a component reference that reaches nothing".to_string(),
            region: REGION,
            method: "PUT",
            uri: "/CreateImageRecipe".to_string(),
            body: Some(json!({
                "name": "boundary-recipe",
                "semanticVersion": "1.0.0",
                "parentImage": "ami-0123456789abcdef0",
                "clientToken": "boundary-recipe-token",
                "components": [{ "componentArn": absent_component }],
            })),
            arn: absent_component.clone(),
            sentence: Sentence::Kind("Component"),
        },
    ];

    for probe in &rows {
        assert_reports_missing(&router, probe).await;
    }

    // The resources these rows named that DO exist are untouched by any of them.
    for (case, uri) in [
        (
            "the stored component",
            format!("/GetComponent?componentBuildVersionArn={component}"),
        ),
        (
            "the stored distribution configuration",
            format!("{}{distribution}", DISTRIBUTION.read),
        ),
    ] {
        let (status, read) = call_in(&router, REGION, "GET", &uri, None).await;
        assert_ok(case, status, read);
    }
}
