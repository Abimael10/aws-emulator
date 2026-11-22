// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for WHICH of this service's two missing-resource sentences a call
//! carries, over the CONFIGURATION families.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rule under test: WHICH of the two sentences a refusal carries is the recorded contract
//! of the ROUTE that looked the ARN up, under the error this service models for a resource it
//! does not hold (`ResourceNotFoundException`, HTTP 404; the model defines the shape and no
//! message text, `smithy_query(query="shape",shape="ResourceNotFoundException")`). The READ of
//! an INFRASTRUCTURE configuration reports the ARN it could not resolve, quoted and by itself
//! — "The resource ARN '<arn>' was not found." — while the kind-naming sentence — "The
//! following required resource '<Kind>' cannot be found: '<arn>'." — is carried by every route
//! that must RESOLVE a reference to a resource (a delete, an update, a tagging write, the
//! versioned families' own lookups) and by every lookup of a DISTRIBUTION configuration, its
//! read included.
//!
//! The rows span what the rule generalises over: which configuration family the ARN names,
//! why the ARN resolves to nothing (nothing ever created it, it was deleted, its name only
//! resembles a stored one, it is a stored configuration reached through another Region's
//! endpoint), and the form of the configuration name the ARN carries — against the contrast
//! rows, which send the SAME absent ARN through the calls that resolve a reference.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call is signed for unless a row says otherwise.
const REGION: &str = "us-west-2";

/// A second Region, so a configuration stored in one can be asked for through another's
/// endpoint.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The sentence a call addressed by an INFRASTRUCTURE configuration's own ARN carries for a
/// read it cannot resolve, spelled out here rather than taken from the crate so a reworded
/// refusal fails.
fn arn_sentence(arn: &str) -> String {
    format!("The resource ARN '{arn}' was not found.")
}

/// The sentence a call that has to RESOLVE a reference to a resource carries.
fn kind_sentence(kind: &str, arn: &str) -> String {
    format!("The following required resource '{kind}' cannot be found: '{arn}'.")
}

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

/// The `message` a refusal carries.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message in {body}"))
        .to_string()
}

/// Assert one call was refused as the modelled not-found error carrying `expected`.
async fn assert_reports_missing(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    expected: &str,
) {
    let (status, answer) = call_in(router, region, method, uri, body).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404, got {status} with {answer}"
    );
    let code = answer
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {code:?} with {answer}"
    );
    let actual = error_message(case, &answer);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the message {expected:?}, got {actual:?}"
    );
}

/// Which configuration family a row is about: how one is created and read, the resource
/// type its ARN carries (as the modelled ARN patterns spell it,
/// `smithy_query(query="shape",shape="DistributionConfigurationArn,InfrastructureConfigurationArn")`),
/// and the kind its READ names in a missing-resource report — `None` where that read quotes
/// the ARN alone.
#[derive(Clone, Copy)]
struct Family {
    label: &'static str,
    resource: &'static str,
    create: &'static str,
    body: fn(&str, &str) -> Value,
    minted: &'static str,
    read: &'static str,
    delete: &'static str,
    read_names_the_kind: Option<&'static str>,
}

/// The body a `CreateDistributionConfiguration` carries: one target Region with one output,
/// the least a distribution entry may carry.
fn distribution_body(region: &str, name: &str) -> Value {
    json!({
        "name": name,
        "clientToken": format!("dist-{region}-{name}"),
        "distributions": [{ "region": region, "amiDistributionConfiguration": {} }],
    })
}

/// The body a `CreateInfrastructureConfiguration` carries: a name, a token of its own and the
/// instance profile the model requires.
fn infrastructure_body(region: &str, name: &str) -> Value {
    json!({
        "name": name,
        "clientToken": format!("infra-{region}-{name}"),
        "instanceProfileName": "EC2InstanceProfileForImageBuilder",
    })
}

const DISTRIBUTION: Family = Family {
    label: "the distribution configuration",
    resource: "distribution-configuration",
    create: "/CreateDistributionConfiguration",
    body: distribution_body,
    minted: "distributionConfigurationArn",
    read: "/GetDistributionConfiguration?distributionConfigurationArn=",
    delete: "/DeleteDistributionConfiguration?distributionConfigurationArn=",
    read_names_the_kind: Some("DistributionConfiguration"),
};

const INFRASTRUCTURE: Family = Family {
    label: "the infrastructure configuration",
    resource: "infrastructure-configuration",
    create: "/CreateInfrastructureConfiguration",
    body: infrastructure_body,
    minted: "infrastructureConfigurationArn",
    read: "/GetInfrastructureConfiguration?infrastructureConfigurationArn=",
    delete: "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn=",
    read_names_the_kind: None,
};

/// The ARN a configuration of `family` named `name` carries in `region`.
fn arn_of(family: Family, region: &str, name: &str) -> String {
    format!(
        "arn:aws:imagebuilder:{region}:{ACCOUNT}:{}/{name}",
        family.resource
    )
}

/// Store one configuration of `family` in `region`, answering the ARN it minted.
async fn create(
    router: &MultiServiceRouter,
    family: Family,
    region: &str,
    name: &str,
) -> String {
    let case = format!("create {} {name} in {region}", family.label);
    let body = (family.body)(region, name);
    let (status, answer) = call_in(router, region, "PUT", family.create, Some(&body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {answer}"
    );
    answer
        .get(family.minted)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the minted ARN in {answer}"))
        .to_string()
}

/// RULE: the READ of either configuration family reports an ARN it cannot resolve in the one
/// sentence that read carries — the ARN alone for the infrastructure configuration, the kind it
/// is for the distribution configuration — whatever the reason it resolves to nothing and
/// whatever form the configuration name takes.
#[tokio::test]
async fn a_configuration_read_reports_an_unresolvable_arn_in_its_own_sentence() {
    for family in [DISTRIBUTION, INFRASTRUCTURE] {
        let router = emulator();

        // One stored configuration per name form the modelled pattern admits, each read back,
        // so the rows below report what is absent rather than refusing everything.
        let mut stored = Vec::new();
        for name in ["kept-one", "kept_two_2", "k33"] {
            let arn = create(&router, family, REGION, name).await;
            let case = format!("{} {name} read back after its create", family.label);
            let (status, read) =
                call_in(&router, REGION, "GET", &format!("{}{arn}", family.read), None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {read}"
            );
            stored.push(arn);
        }

        // One configuration that WAS stored and then deleted, so a row reads an ARN that lived.
        let deleted = create(&router, family, REGION, "stored-then-deleted").await;
        let (status, cleared) = call_in(
            &router,
            REGION,
            "DELETE",
            &format!("{}{deleted}", family.delete),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the delete to succeed, got {status} with {cleared}",
            family.label
        );

        // (row, the Region the read is signed for, the ARN it names)
        let rows: Vec<(String, &'static str, String)> = vec![
            (
                "a configuration that was stored and then deleted".to_string(),
                REGION,
                deleted,
            ),
            (
                "a name nothing created".to_string(),
                REGION,
                arn_of(family, REGION, "absent-one"),
            ),
            (
                "a one-token name nothing created".to_string(),
                REGION,
                arn_of(family, REGION, "absent_two_2"),
            ),
            (
                "a short name nothing created".to_string(),
                REGION,
                arn_of(family, REGION, "a1"),
            ),
            (
                "a name that only resembles a stored one".to_string(),
                REGION,
                arn_of(family, REGION, "kept-one-2"),
            ),
            (
                "a stored configuration read through another Region's endpoint".to_string(),
                OTHER_REGION,
                stored[1].clone(),
            ),
            (
                "a name of another Region nothing created".to_string(),
                REGION,
                arn_of(family, OTHER_REGION, "k33"),
            ),
        ];

        for (row, region, arn) in rows {
            let case = format!("{} on {row}", family.label);
            let expected = match family.read_names_the_kind {
                Some(kind) => kind_sentence(kind, &arn),
                None => arn_sentence(&arn),
            };
            assert_reports_missing(
                &router,
                &case,
                region,
                "GET",
                &format!("{}{arn}", family.read),
                None,
                &expected,
            )
            .await;
        }

        // The configurations the rows did not name are still readable, so every sentence above
        // answered an ARN rather than a store this suite broke.
        for arn in &stored[1..] {
            let case = format!("{} {arn} left standing", family.label);
            let (status, read) =
                call_in(&router, REGION, "GET", &format!("{}{arn}", family.read), None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {read}"
            );
        }
    }
}

/// One call that resolves a REFERENCE to a resource: how to send it, the kind its report has
/// to name and the ARN it has to state.
struct Reference {
    /// What this row is, named in every assertion it makes.
    case: &'static str,
    /// The HTTP method the modelled operation binds.
    method: &'static str,
    /// The request target, the ARN included where the operation takes it in the URI.
    uri: String,
    /// The request body, for an operation that takes the ARN in one.
    body: Option<Value>,
    /// The resource-type token the report has to name.
    kind: &'static str,
    /// The ARN the report has to state, verbatim as the request carried it.
    arn: String,
}

/// RULE's boundary: the calls that have to RESOLVE a reference to a resource name the KIND the
/// ARN identifies — the same absent ARN a read reports by itself.
#[tokio::test]
async fn a_call_that_resolves_a_reference_names_the_kind() {
    let router = emulator();
    let absent_distribution = arn_of(DISTRIBUTION, REGION, "absent-dist");
    let absent_infrastructure = arn_of(INFRASTRUCTURE, REGION, "absent-infra");
    let absent_recipe =
        format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:image-recipe/absent-recipe/1.0.0");

    let rows: Vec<Reference> = vec![
        Reference {
            case: "a delete of a distribution configuration nothing created",
            method: "DELETE",
            uri: format!("{}{absent_distribution}", DISTRIBUTION.delete),
            body: None,
            kind: "DistributionConfiguration",
            arn: absent_distribution.clone(),
        },
        Reference {
            case: "an update of a distribution configuration nothing created",
            method: "PUT",
            uri: "/UpdateDistributionConfiguration".to_string(),
            body: Some(json!({
                "distributionConfigurationArn": absent_distribution,
                "clientToken": "boundary-update-dist",
                "distributions": [{ "region": REGION, "amiDistributionConfiguration": {} }],
            })),
            kind: "DistributionConfiguration",
            arn: absent_distribution.clone(),
        },
        Reference {
            case: "a tagging write naming a distribution configuration nothing created",
            method: "POST",
            uri: format!("/tags/{}", encode_label(&absent_distribution)),
            body: Some(json!({ "tags": { "owner": "platform" } })),
            kind: "DistributionConfiguration",
            arn: absent_distribution.clone(),
        },
        Reference {
            case: "a tagging write naming an infrastructure configuration nothing created",
            method: "POST",
            uri: format!("/tags/{}", encode_label(&absent_infrastructure)),
            body: Some(json!({ "tags": { "owner": "platform" } })),
            kind: "InfrastructureConfiguration",
            arn: absent_infrastructure.clone(),
        },
        Reference {
            case: "a read of an image recipe nothing created",
            method: "GET",
            uri: format!("/GetImageRecipe?imageRecipeArn={absent_recipe}"),
            body: None,
            kind: "ImageRecipe",
            arn: absent_recipe.clone(),
        },
    ];

    for row in rows {
        assert_reports_missing(
            &router,
            row.case,
            REGION,
            row.method,
            &row.uri,
            row.body.as_ref(),
            &kind_sentence(row.kind, &row.arn),
        )
        .await;
    }
}
