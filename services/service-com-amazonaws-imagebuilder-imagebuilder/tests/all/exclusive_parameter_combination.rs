// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for a request that names BOTH members of a pair of parameters that
//! exclude each other.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, this service's front door, the modelled-constraint walk and the handlers —
//! so what is asserted is what a signed AWS SDK request observes.
//!
//! The rule under test: where two optional members of one input exclude each other, a
//! request naming BOTH is refused as the parameter COMBINATION it carries — HTTP 400 with the
//! wire code `InvalidParameterCombinationException`
//! (`smithy_query(query="shape",shape="InvalidParameterCombinationException")`: a client
//! error, `@httpError` 400, "You have specified a combination of parameters that isn't
//! valid") — and the sentence names BOTH members, in the order the input shape declares them,
//! followed by the requirement the pair had to satisfy. No ONE value is wrong, so no single
//! parameter is named and nothing of either value is echoed. Naming exactly one of the pair
//! is the accepted request, and a refused one writes nothing.
//!
//! The rows span what the rule generalises over: which operation carries the pair
//! (`CreateImagePipeline`, `UpdateImagePipeline`, `CreateImage`, `CreateComponent`,
//! `CreateContainerRecipe`), which pair it is (the two recipe references, a component's
//! document and its location, a container recipe's template and its location), the ORDER the
//! request names the two members in, and whether the values name resources this store holds.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::image_pipeline_schedule::{
    assert_ok, call, emulator, error_code, error_message, get_pipeline, text,
};

/// The Region every case signs for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The class a request carrying a combination of parameters that cannot hold together is
/// refused with.
const COMBINATION: &str = "InvalidParameterCombinationException";

/// A build component document: one `build` phase, which is all a component these cases create
/// needs.
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

/// A Dockerfile template a container recipe can carry inline.
const INLINE_TEMPLATE: &str = "FROM {{{ imagebuilder:parentImage }}}\n";

/// One row of the refusal table: the case, the request (method, URI, body), the two members
/// the refusal names and the requirement it states for the pair.
type Row = (&'static str, &'static str, Value, [&'static str; 2], String);

/// The sentence this service states for a `requirement` that holds over `members` together.
///
/// The template is the one every parameter refusal of this service states — the parameter the
/// value arrived in, then the requirement — with BOTH members named because neither value is
/// wrong on its own.
fn combination_message(members: &[&str; 2], requirement: &str) -> String {
    let [first, second] = members;
    format!("The value supplied for parameter '{first}' and '{second}' is not valid. {requirement}")
}

/// Assert one call was refused as the parameter combination it carried.
fn assert_combination(case: &str, status: u16, body: &Value, expected: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body}"
    );
    let code = error_code(case, body);
    assert_eq!(
        code, COMBINATION,
        "case {case:?}: expected {COMBINATION}, got {code:?} with {body}"
    );
    let message = error_message(case, body);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
}

/// The resources these cases reference, each created through its own operation.
struct Fixture {
    /// An image recipe this store holds.
    image_recipe: String,
    /// A container recipe this store holds.
    container_recipe: String,
    /// The infrastructure configuration a pipeline and a build run in.
    configuration: String,
}

impl Fixture {
    /// Create one of each resource the rows name.
    async fn given(router: &MultiServiceRouter) -> Self {
        let component = create(
            router,
            "the component",
            "/CreateComponent",
            &json!({
                "name": "combination-component",
                "semanticVersion": "1.0.0",
                "platform": "Linux",
                "clientToken": "combination-component",
                "data": BUILD_DOCUMENT,
            }),
            "componentBuildVersionArn",
        )
        .await;
        let image_recipe = create(
            router,
            "the image recipe",
            "/CreateImageRecipe",
            &json!({
                "name": "combination-recipe",
                "semanticVersion": "1.0.0",
                "parentImage": "ami-0123456789abcdef0",
                "clientToken": "combination-recipe",
                "components": [{ "componentArn": component }],
            }),
            "imageRecipeArn",
        )
        .await;
        let container_recipe = create(
            router,
            "the container recipe",
            "/CreateContainerRecipe",
            &container_recipe_body(
                "combination-container",
                "combination-container",
                json!({ "dockerfileTemplateData": INLINE_TEMPLATE }),
            ),
            "containerRecipeArn",
        )
        .await;
        let configuration = create(
            router,
            "the infrastructure configuration",
            "/CreateInfrastructureConfiguration",
            &json!({
                "name": "combination-infra",
                "clientToken": "combination-infra",
                "instanceProfileName": "EC2InstanceProfileForImageBuilder",
            }),
            "infrastructureConfigurationArn",
        )
        .await;
        Self {
            image_recipe,
            container_recipe,
            configuration,
        }
    }

    /// A `CreateImagePipeline` body for `name` carrying `extra` beside the members a create
    /// cannot omit.
    fn create_pipeline_body(&self, name: &str, extra: Value) -> Value {
        let mut body = Map::new();
        body.insert("name".to_string(), json!(name));
        body.insert("clientToken".to_string(), json!(name));
        body.insert(
            "infrastructureConfigurationArn".to_string(),
            json!(self.configuration),
        );
        merge(body, extra)
    }

    /// An `UpdateImagePipeline` body for the pipeline `arn` carrying `extra`.
    fn update_pipeline_body(&self, arn: &str, client_token: &str, extra: Value) -> Value {
        let mut body = Map::new();
        body.insert("imagePipelineArn".to_string(), json!(arn));
        body.insert("clientToken".to_string(), json!(client_token));
        body.insert(
            "infrastructureConfigurationArn".to_string(),
            json!(self.configuration),
        );
        merge(body, extra)
    }

    /// A `CreateImage` body carrying `extra`.
    fn create_image_body(&self, client_token: &str, extra: Value) -> Value {
        let mut body = Map::new();
        body.insert("clientToken".to_string(), json!(client_token));
        body.insert(
            "infrastructureConfigurationArn".to_string(),
            json!(self.configuration),
        );
        merge(body, extra)
    }
}

/// `base` with every member of `extra` added to it.
fn merge(mut base: Map<String, Value>, extra: Value) -> Value {
    if let Value::Object(extra) = extra {
        for (member, value) in extra {
            base.insert(member, value);
        }
    }
    Value::Object(base)
}

/// A `CreateContainerRecipe` body: the members a create cannot omit, plus `extra`.
fn container_recipe_body(name: &str, client_token: &str, extra: Value) -> Value {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("semanticVersion".to_string(), json!("1.0.0"));
    body.insert("containerType".to_string(), json!("DOCKER"));
    body.insert("parentImage".to_string(), json!("amazonlinux:latest"));
    body.insert("clientToken".to_string(), json!(client_token));
    body.insert(
        "targetRepository".to_string(),
        json!({ "service": "ECR", "repositoryName": "team-a/my-repo" }),
    );
    merge(body, extra)
}

/// Create one resource and answer the ARN its response carries.
async fn create(
    router: &MultiServiceRouter,
    case: &str,
    target: &str,
    body: &Value,
    member: &str,
) -> String {
    let (status, answer) = call(router, "PUT", target, Some(body)).await;
    let answer = assert_ok(case, status, answer);
    text(case, &answer, member)
}

/// A container-recipe ARN of the calling Region this store holds nothing under.
fn absent_container_recipe() -> String {
    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:container-recipe/nothing-stored/1.0.0")
}

/// The requirement the two recipe references of a pipeline or a build have to satisfy: "You
/// must specify exactly one recipe for the pipeline, using either a containerRecipeArn or an
/// imageRecipeArn" (`smithy_query(query="operation",operation="CreateImagePipeline")`), which
/// each member repeats as "You must specify either this property or … but not both".
const ONE_RECIPE: &str = "You must specify either imageRecipeArn or containerRecipeArn.";

/// The members of that pair, in the order every input shape carrying it declares them.
const RECIPE_MEMBERS: [&str; 2] = ["imageRecipeArn", "containerRecipeArn"];

#[tokio::test]
async fn naming_both_members_of_an_exclusive_pair_is_refused_as_that_combination() {
    let router = emulator();
    let fixture = Fixture::given(&router).await;

    // A pipeline the update rows address, created with exactly ONE recipe.
    let pipeline = create(
        &router,
        "the pipeline an update addresses",
        "/CreateImagePipeline",
        &fixture.create_pipeline_body(
            "combination-pipeline",
            json!({ "imageRecipeArn": fixture.image_recipe }),
        ),
        "imagePipelineArn",
    )
    .await;

    let rows: Vec<Row> = vec![
        (
            "a pipeline create naming both recipes",
            "/CreateImagePipeline",
            fixture.create_pipeline_body(
                "both-recipes-pipeline",
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "containerRecipeArn": fixture.container_recipe,
                }),
            ),
            RECIPE_MEMBERS,
            ONE_RECIPE.to_string(),
        ),
        (
            // The sentence names the members in the ORDER THE SHAPE declares them, whatever
            // order the request carried them in.
            "a pipeline create naming the container recipe first",
            "/CreateImagePipeline",
            fixture.create_pipeline_body(
                "both-recipes-reordered",
                json!({
                    "containerRecipeArn": fixture.container_recipe,
                    "imageRecipeArn": fixture.image_recipe,
                }),
            ),
            RECIPE_MEMBERS,
            ONE_RECIPE.to_string(),
        ),
        (
            // The pair cannot hold together whatever the two values name, so it is refused
            // before either reference is resolved.
            "a pipeline create whose second recipe names nothing stored",
            "/CreateImagePipeline",
            fixture.create_pipeline_body(
                "both-recipes-absent",
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "containerRecipeArn": absent_container_recipe(),
                }),
            ),
            RECIPE_MEMBERS,
            ONE_RECIPE.to_string(),
        ),
        (
            "a pipeline update naming both recipes",
            "/UpdateImagePipeline",
            fixture.update_pipeline_body(
                &pipeline,
                "both-recipes-update",
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "containerRecipeArn": fixture.container_recipe,
                }),
            ),
            RECIPE_MEMBERS,
            ONE_RECIPE.to_string(),
        ),
        (
            "a build naming both recipes",
            "/CreateImage",
            fixture.create_image_body(
                "both-recipes-build",
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "containerRecipeArn": fixture.container_recipe,
                }),
            ),
            RECIPE_MEMBERS,
            ONE_RECIPE.to_string(),
        ),
        (
            // "The component is based on a YAML document that you specify using exactly one of
            // the following methods" — the same shape of requirement over another pair.
            "a component create naming both its document and its location",
            "/CreateComponent",
            json!({
                "name": "combination-both-sources",
                "semanticVersion": "1.0.0",
                "platform": "Linux",
                "clientToken": "combination-both-sources",
                "data": BUILD_DOCUMENT,
                "uri": "s3://combination-bucket/component.yaml",
            }),
            ["data", "uri"],
            "You must specify either data or a uri.".to_string(),
        ),
        (
            // "You must specify exactly one of the dockerfileTemplateData or
            // dockerfileTemplateUri properties."
            "a container recipe create naming both its template and its location",
            "/CreateContainerRecipe",
            container_recipe_body(
                "combination-both-templates",
                "combination-both-templates",
                json!({
                    "dockerfileTemplateData": INLINE_TEMPLATE,
                    "dockerfileTemplateUri": "s3://combination-bucket/Dockerfile",
                }),
            ),
            ["dockerfileTemplateData", "dockerfileTemplateUri"],
            "You must specify exactly one of the dockerfileTemplateData or \
             dockerfileTemplateUri properties."
                .to_string(),
        ),
    ];

    for (case, target, body, members, requirement) in &rows {
        let (status, answer) = call(&router, "PUT", target, Some(body)).await;
        assert_combination(
            case,
            status,
            &answer,
            &combination_message(members, requirement),
        );
    }
}

#[tokio::test]
async fn naming_exactly_one_member_of_the_pair_is_the_request_that_is_performed() {
    let router = emulator();
    let fixture = Fixture::given(&router).await;

    // Either member ON ITS OWN is the accepted request, and the pipeline reads back carrying
    // the one recipe the create named and no other.
    let rows: [(&str, &str, &str); 2] = [
        (
            "a pipeline naming only the image recipe",
            "imageRecipeArn",
            &fixture.image_recipe,
        ),
        (
            "a pipeline naming only the container recipe",
            "containerRecipeArn",
            &fixture.container_recipe,
        ),
    ];

    let mut created: Vec<(&str, String, &str, &str)> = Vec::new();
    for (case, member, value) in rows {
        let name = format!("one-recipe-{member}").to_lowercase();
        let body = fixture.create_pipeline_body(&name, json!({ member: value }));
        let arn = create(&router, case, "/CreateImagePipeline", &body, "imagePipelineArn").await;
        let (status, answer) = get_pipeline(&router, &arn).await;
        let answer = assert_ok(case, status, answer);
        let pipeline = answer
            .get("imagePipeline")
            .unwrap_or_else(|| panic!("case {case:?}: expected an imagePipeline in {answer}"));
        let stored = text(case, pipeline, member);
        assert_eq!(
            stored, value,
            "case {case:?}: expected the stored {member} {value:?}, got {stored:?}"
        );
        let other = RECIPE_MEMBERS
            .iter()
            .find(|candidate| **candidate != member)
            .copied()
            .expect("the pair has another member");
        assert!(
            pipeline.get(other).is_none(),
            "case {case:?}: expected no {other} on the stored pipeline, got {pipeline}"
        );
        created.push((case, arn, member, value));
    }

    // And an update that names BOTH replaces nothing: each pipeline still reports the one
    // recipe it was created with.
    for (case, arn, member, value) in created {
        let (status, refused) = call(
            &router,
            "PUT",
            "/UpdateImagePipeline",
            Some(&fixture.update_pipeline_body(
                &arn,
                &format!("refused-update-{member}"),
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "containerRecipeArn": fixture.container_recipe,
                }),
            )),
        )
        .await;
        assert_combination(
            case,
            status,
            &refused,
            &combination_message(&RECIPE_MEMBERS, ONE_RECIPE),
        );

        let (status, answer) = get_pipeline(&router, &arn).await;
        let answer = assert_ok(case, status, answer);
        let pipeline = answer
            .get("imagePipeline")
            .unwrap_or_else(|| panic!("case {case:?}: expected an imagePipeline in {answer}"));
        let stored = text(case, pipeline, member);
        assert_eq!(
            stored, value,
            "case {case:?}: expected the refused update to leave {member} at {value:?}, got \
             {stored:?}"
        );
    }
}
