// SPDX-License-Identifier: CC0-1.0

//! The TAG SET the CloudFormation stack a launch deploys carries, driven end to end over
//! the wire through a router carrying Service Catalog beside the services a launch is made
//! out of.
//!
//! The rule under test: the stack Service Catalog deploys for a provisioned product
//! (`SC-<account id>-<provisioned product id>`) carries, besides the `Tags` the request
//! supplied, the TagOptions the resolved launch path forces onto everything launched
//! through it — the same set `DescribeProvisioningParameters` offers under `TagOptions` —
//! and the tags that record the GRAPH the launch went through: the portfolio whose path
//! granted it, the product and the version launched, the principal that asked for it and
//! the provisioned product it is for.
//!
//! Every expected value is read back from the API that OWNS it rather than spelled here:
//! the portfolio's ARN from `DescribePortfolio`, the product's from
//! `DescribeProductAsAdmin`, the provisioned product's from `DescribeProvisionedProduct`,
//! and the stack's tags from CloudFormation's own wire. So an assertion can only pass if
//! the tag names the very resource the catalogue reports.
//!
//! The rows differ along what the rule generalises over: how many tags the request carries,
//! which resource of the path carries the TagOption (the portfolio, the product, both),
//! how many values a TagOption key offers (one, several, none — the conflict the model
//! documents), whether the TagOption is active, whether the request tags the same key
//! itself, and which credential signed the launch.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, call_signed_by, create_portfolio, create_product_with_info, create_template_bucket,
    emulator_with_identity_services, emulator_with_template_services,
    first_provisioning_artifact_id, grant_principal_on_portfolio, seed_launch_path, stage_template,
    staged_template_url, tag_list, ACCOUNT_ID, DEFAULT_REGION, HARNESS_ACCESS_KEY_ID,
};

/// The `ProductType` of a CloudFormation-backed product, which is the only kind whose
/// launch stands a stack up (`smithy_query query="shape" shape="ProductType"`).
const CFN_PRODUCT: &str = "CLOUD_FORMATION_TEMPLATE";

/// The tags Service Catalog records the launch graph with. AWS applies all five to the
/// stack it deploys for a provisioned product; each one is stated once here and asserted
/// against the resource the catalogue itself reports for it.
const TAG_PORTFOLIO_ARN: &str = "aws:servicecatalog:portfolioArn";
const TAG_PRODUCT_ARN: &str = "aws:servicecatalog:productArn";
const TAG_PROVISIONING_ARTIFACT: &str = "aws:servicecatalog:provisioningArtifactIdentifier";
const TAG_PROVISIONING_PRINCIPAL_ARN: &str = "aws:servicecatalog:provisioningPrincipalArn";
const TAG_PROVISIONED_PRODUCT_ARN: &str = "aws:servicecatalog:provisionedProductArn";

/// How long a poll keeps looking for a condition the emulator's clock owes before it gives
/// up. Far longer than any span this emulator settles on, so a loaded or slow host cannot
/// fail a case a fast one passes: the loop waits for the CONDITION, never for a fixed time.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Drive one CloudFormation request through the same router, as a client's own SDK sends
/// it: the query protocol that service speaks.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    pairs: &[(&str, &str)],
) -> (u16, String) {
    query_call(
        router,
        "cloudformation",
        &format!("https://cloudformation.{DEFAULT_REGION}.amazonaws.com/"),
        action,
        "2010-05-15",
        pairs,
    )
    .await
}

/// Drive one IAM request through the same router: the query protocol that service speaks.
async fn iam(router: &MultiServiceRouter, action: &str, pairs: &[(&str, &str)]) -> (u16, String) {
    query_call(
        router,
        "iam",
        "https://iam.amazonaws.com/",
        action,
        "2010-05-08",
        pairs,
    )
    .await
}

/// One form-encoded query-protocol request, as a client's own SDK sends it.
async fn query_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    endpoint: &str,
    action: &str,
    version: &str,
    pairs: &[(&str, &str)],
) -> (u16, String) {
    let mut form = ::form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", action);
    form.append_pair("Version", version);
    for (key, value) in pairs {
        form.append_pair(key, value);
    }
    let request = ::http::Request::builder()
        .method("POST")
        .uri(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={HARNESS_ACCESS_KEY_ID}/20260101/{DEFAULT_REGION}/\
                 {signing_name}/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(form.finish()))
        .expect("a well-formed query-protocol request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let read = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, read)
}

/// The text of the first `<tag>` of an XML body, or `None` when it carries none.
fn xml_value(body: &str, tag: &str) -> Option<String> {
    body.split(&format!("<{tag}>"))
        .nth(1)
        .and_then(|rest| rest.split(&format!("</{tag}>")).next())
        .map(str::to_string)
}

/// The tags one stack carries, as `key -> value`, read out of the `<Tags>` block of a
/// `DescribeStacks` answer (`<Tags><member><Key/><Value/></member></Tags>`).
fn stack_tags(described: &str) -> BTreeMap<String, String> {
    let block = described
        .split("<Tags>")
        .nth(1)
        .and_then(|rest| rest.split("</Tags>").next())
        .unwrap_or_default()
        .to_string();
    block
        .split("<member>")
        .skip(1)
        .filter_map(|member| Some((xml_value(member, "Key")?, xml_value(member, "Value")?)))
        .collect()
}

/// Look at the stack `stack` until CloudFormation holds it, then answer its tags.
///
/// A launch stands its stack up before it answers, so the first look normally finds it;
/// the loop waits for the stack to EXIST rather than for any fixed time, so the suite
/// cannot depend on how fast the host runs.
async fn tags_of_deployed_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    let started = std::time::Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        if status == 200 {
            return stack_tags(&described);
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected CloudFormation to hold the stack {stack}, last answered \
             {status} with {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A minimal product template: one bucket, named so two launches cannot collide.
fn template(bucket: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "A staged product template",
        "Resources": {
            "Data": {"Type": "AWS::S3::Bucket", "Properties": {"BucketName": bucket}}
        }
    })
    .to_string()
}

/// Create one TagOption and answer the id the service minted.
async fn create_tag_option(router: &MultiServiceRouter, key: &str, value: &str) -> String {
    let (status, created) =
        call(router, "CreateTagOption", &json!({"Key": key, "Value": value})).await;
    assert_eq!(
        status, 200,
        "seed CreateTagOption({key:?}, {value:?}): expected 200, got {status} with {created}"
    );
    created["TagOptionDetail"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("seed CreateTagOption({key:?}): no Id in {created}"))
        .to_string()
}

/// Associate one TagOption with one resource — a portfolio or a product, the two kinds
/// `AssociateTagOptionWithResource` takes.
async fn associate_tag_option(router: &MultiServiceRouter, resource_id: &str, tag_option: &str) {
    let (status, body) = call(
        router,
        "AssociateTagOptionWithResource",
        &json!({"ResourceId": resource_id, "TagOptionId": tag_option}),
    )
    .await;
    assert_eq!(
        status, 200,
        "seed AssociateTagOptionWithResource({resource_id}, {tag_option}): expected 200, got \
         {status} with {body}"
    );
}

/// Which resource of the launch path a seeded TagOption is associated with.
#[derive(Clone, Copy, Debug)]
enum On {
    Portfolio,
    Product,
    Both,
}

/// One TagOption a row seeds on the launch path it provisions through.
#[derive(Clone, Copy, Debug)]
struct TagOptionSeed {
    key: &'static str,
    value: &'static str,
    on: On,
    /// Whether the TagOption is left active; `UpdateTagOption`'s `Active` is what a
    /// deactivated one is turned off with.
    active: bool,
}

/// One row: a launch through a path carrying `tag_options`, with the request's own `tags`,
/// and the tags the deployed stack must then carry beyond the launch graph.
struct TagCase {
    case: &'static str,
    /// The provisioned product's name, which also names its portfolio, product and the
    /// bucket its template declares, so two rows share nothing.
    name: &'static str,
    tags: &'static [(&'static str, &'static str)],
    tag_options: &'static [TagOptionSeed],
    /// The TagOptions the path must OFFER, as `DescribeProvisioningParameters` reports
    /// them: `key -> values`.
    offered: &'static [(&'static str, &'static [&'static str])],
    /// The tags the stack must carry besides the five that record the launch graph.
    expected: &'static [(&'static str, &'static str)],
}

/// The rows: every combination the rule generalises over.
const TAG_CASES: &[TagCase] = &[
    TagCase {
        case: "no request tags and no TagOptions on the path",
        name: "governed-bare",
        tags: &[],
        tag_options: &[],
        offered: &[],
        expected: &[],
    },
    TagCase {
        case: "the request's own tags",
        name: "governed-request-tags",
        tags: &[("sim2real", "one"), ("env", "prod")],
        tag_options: &[],
        offered: &[],
        expected: &[("sim2real", "one"), ("env", "prod")],
    },
    TagCase {
        case: "a TagOption associated with the PORTFOLIO reaches the stack",
        name: "governed-portfolio-tagoption",
        tags: &[("sim2real", "two")],
        tag_options: &[TagOptionSeed {
            key: "costcenter",
            value: "cc-7",
            on: On::Portfolio,
            active: true,
        }],
        offered: &[("costcenter", &["cc-7"])],
        expected: &[("sim2real", "two"), ("costcenter", "cc-7")],
    },
    TagCase {
        case: "a TagOption associated with the PRODUCT reaches the stack",
        name: "governed-product-tagoption",
        tags: &[],
        tag_options: &[TagOptionSeed {
            key: "tier",
            value: "gold",
            on: On::Product,
            active: true,
        }],
        offered: &[("tier", &["gold"])],
        expected: &[("tier", "gold")],
    },
    TagCase {
        case: "the portfolio and the product agree on one value",
        name: "governed-agreed-tagoption",
        tags: &[],
        tag_options: &[TagOptionSeed {
            key: "owner",
            value: "platform",
            on: On::Both,
            active: true,
        }],
        offered: &[("owner", &["platform"])],
        expected: &[("owner", "platform")],
    },
    TagCase {
        case: "a key offering SEVERAL values leaves the choice to the request",
        name: "governed-several-values",
        tags: &[],
        tag_options: &[
            TagOptionSeed {
                key: "costcenter",
                value: "cc-s1",
                on: On::Portfolio,
                active: true,
            },
            TagOptionSeed {
                key: "costcenter",
                value: "cc-s2",
                on: On::Portfolio,
                active: true,
            },
        ],
        offered: &[("costcenter", &["cc-s1", "cc-s2"])],
        expected: &[],
    },
    TagCase {
        case: "the request's value wins over the one value offered",
        name: "governed-request-chooses",
        tags: &[("costcenter", "cc-9")],
        tag_options: &[TagOptionSeed {
            key: "costcenter",
            value: "cc-1",
            on: On::Portfolio,
            active: true,
        }],
        offered: &[("costcenter", &["cc-1"])],
        expected: &[("costcenter", "cc-9")],
    },
    TagCase {
        case: "a DEACTIVATED TagOption is neither offered nor applied",
        name: "governed-retired-tagoption",
        tags: &[("sim2real", "three")],
        tag_options: &[TagOptionSeed {
            key: "retired",
            value: "yes",
            on: On::Portfolio,
            active: false,
        }],
        offered: &[],
        expected: &[("sim2real", "three")],
    },
    TagCase {
        case: "a key the two resources DISAGREE on is the conflict, and reaches no tag",
        name: "governed-conflicted-tagoption",
        tags: &[],
        tag_options: &[
            TagOptionSeed {
                key: "costcenter",
                value: "cc-portfolio",
                on: On::Portfolio,
                active: true,
            },
            TagOptionSeed {
                key: "costcenter",
                value: "cc-product",
                on: On::Product,
                active: true,
            },
        ],
        offered: &[("costcenter", &[])],
        expected: &[],
    },
];

/// What one seeded launch path resolved to.
struct Path {
    portfolio_id: String,
    portfolio_arn: String,
    product_id: String,
    product_arn: String,
    version: String,
}

/// Stage `name`'s template, create the portfolio and the product that give it a launch
/// path, associate them and seed `tag_options` — the administrator's half of a launch.
async fn seed_path(
    router: &MultiServiceRouter,
    name: &str,
    tag_options: &[TagOptionSeed],
) -> Path {
    let key = format!("{name}.template.json");
    stage_template(router, &key, &template(name)).await;
    let portfolio_id = create_portfolio(router, &format!("{name}-portfolio"), "acme", &[], name)
        .await;
    let product_id = create_product_with_info(
        router,
        name,
        CFN_PRODUCT,
        &json!({"LoadTemplateFromURL": staged_template_url(&key)}),
        name,
    )
    .await;
    let version = first_provisioning_artifact_id(router, &product_id).await;
    seed_launch_path(router, &product_id, &portfolio_id).await;

    for seed in tag_options {
        let id = create_tag_option(router, seed.key, seed.value).await;
        if matches!(seed.on, On::Portfolio | On::Both) {
            associate_tag_option(router, &portfolio_id, &id).await;
        }
        if matches!(seed.on, On::Product | On::Both) {
            associate_tag_option(router, &product_id, &id).await;
        }
        if !seed.active {
            let (status, updated) =
                call(router, "UpdateTagOption", &json!({"Id": id, "Active": false})).await;
            assert_eq!(
                status, 200,
                "seed UpdateTagOption({id}): expected 200, got {status} with {updated}"
            );
        }
    }

    let (status, portfolio) = call(router, "DescribePortfolio", &json!({"Id": portfolio_id})).await;
    assert_eq!(
        status, 200,
        "seed DescribePortfolio({portfolio_id}): expected 200, got {status} with {portfolio}"
    );
    let portfolio_arn = portfolio["PortfolioDetail"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("seed DescribePortfolio({portfolio_id}): no ARN in {portfolio}"))
        .to_string();
    let (status, product) =
        call(router, "DescribeProductAsAdmin", &json!({"Id": product_id})).await;
    assert_eq!(
        status, 200,
        "seed DescribeProductAsAdmin({product_id}): expected 200, got {status} with {product}"
    );
    let product_arn = product["ProductViewDetail"]["ProductARN"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("seed DescribeProductAsAdmin({product_id}): no ProductARN in {product}")
        })
        .to_string();

    Path {
        portfolio_id,
        portfolio_arn,
        product_id,
        product_arn,
        version,
    }
}

/// The TagOptions `DescribeProvisioningParameters` offers for one launch, as
/// `key -> values`.
async fn offered_tag_options(
    router: &MultiServiceRouter,
    case: &str,
    path: &Path,
) -> BTreeMap<String, Vec<String>> {
    let (status, described) = call(
        router,
        "DescribeProvisioningParameters",
        &json!({
            "ProductId": path.product_id,
            "ProvisioningArtifactId": path.version,
            "PathId": path.portfolio_id,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the launch to be described, got {status} with {described}"
    );
    described["TagOptions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a TagOptions list, got {described}"))
        .iter()
        .map(|summary| {
            let key = summary["Key"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a TagOption has no Key: {summary}"))
                .to_string();
            let values = summary["Values"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: a TagOption has no Values: {summary}"))
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: a TagOption value is not a string: {summary}")
                        })
                        .to_string()
                })
                .collect();
            (key, values)
        })
        .collect()
}

/// What one launch left behind: the provisioned product and the stack it stood up.
struct Launched {
    provisioned_product_id: String,
    provisioned_product_arn: String,
    stack: String,
}

/// Provision `name` through `path`, signed by `access_key_id`, and answer the provisioned
/// product and the stack the launch stood up.
async fn provision(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    name: &str,
    path: &Path,
    tags: &[(&str, &str)],
) -> Launched {
    let mut request = json!({
        "ProductId": path.product_id,
        "ProvisioningArtifactId": path.version,
        "PathId": path.portfolio_id,
        "ProvisionedProductName": name,
        "ProvisionToken": format!("{name}-provision"),
    });
    if !tags.is_empty() {
        request["Tags"] = tag_list(tags);
    }
    let (status, provisioned) =
        call_signed_by(router, access_key_id, DEFAULT_REGION, "ProvisionProduct", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the launch to be served, got {status} with {provisioned}"
    );
    let provisioned_product_id = provisioned["RecordDetail"]["ProvisionedProductId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no ProvisionedProductId in {provisioned}"))
        .to_string();

    let (status, described) = call(
        router,
        "DescribeProvisionedProduct",
        &json!({"Id": provisioned_product_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the provisioned product to be described, got {status} with \
         {described}"
    );
    let provisioned_product_arn = described["ProvisionedProductDetail"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Arn in {described}"))
        .to_string();

    Launched {
        // AWS names the stack a launch stands up `SC-<account id>-<provisioned product
        // id>`, so it is addressed by the id the launch minted.
        stack: format!("SC-{ACCOUNT_ID}-{provisioned_product_id}"),
        provisioned_product_id,
        provisioned_product_arn,
    }
}

/// The five tags that record one launch's graph, as the resources the catalogue reports
/// for it.
fn launch_graph_tags(
    path: &Path,
    launched: &Launched,
    principal_arn: &str,
) -> BTreeMap<String, String> {
    [
        (TAG_PORTFOLIO_ARN, path.portfolio_arn.clone()),
        (TAG_PRODUCT_ARN, path.product_arn.clone()),
        (TAG_PROVISIONING_ARTIFACT, path.version.clone()),
        (TAG_PROVISIONING_PRINCIPAL_ARN, principal_arn.to_string()),
        (
            TAG_PROVISIONED_PRODUCT_ARN,
            launched.provisioned_product_arn.clone(),
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect()
}

/// The stack a launch deploys carries the request's `Tags`, the TagOptions the launch path
/// forces onto it, and the five tags that record the launch graph — and nothing else.
#[tokio::test]
async fn the_deployed_stack_carries_the_launch_path_tag_options_and_the_launch_graph() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    // The principal the harness's own credential speaks for: the identity this emulator
    // publishes its long-term key as, which is what a caller reads back for itself.
    let principal_arn = ::service_common::caller_principal::ambient_user_arn();

    for row in TAG_CASES {
        let case = row.case;
        let path = seed_path(&router, row.name, row.tag_options).await;

        // The hop before the launch: what the path OFFERS is what the launch applies, so
        // the two readings are asserted against one expectation.
        let offered = offered_tag_options(&router, case, &path).await;
        let wanted_offer: BTreeMap<String, Vec<String>> = row
            .offered
            .iter()
            .map(|(key, values)| {
                (
                    key.to_string(),
                    values.iter().map(|value| value.to_string()).collect(),
                )
            })
            .collect();
        assert_eq!(
            offered, wanted_offer,
            "case {case:?}: expected the path to offer {wanted_offer:?}, got {offered:?}"
        );

        let launched = provision(
            &router,
            case,
            HARNESS_ACCESS_KEY_ID,
            row.name,
            &path,
            row.tags,
        )
        .await;

        let mut wanted: BTreeMap<String, String> = row
            .expected
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        wanted.extend(launch_graph_tags(&path, &launched, &principal_arn));

        let carried = tags_of_deployed_stack(&router, case, &launched.stack).await;
        assert_eq!(
            carried, wanted,
            "case {case:?}: expected the stack {} to carry {wanted:?}, got {carried:?}",
            launched.stack
        );
        // The governance tags are the link from the stack back to the launch: each names
        // the resource it records, which the identifiers the launch minted end in.
        for (tag, identifier) in [
            (TAG_PORTFOLIO_ARN, &path.portfolio_id),
            (TAG_PRODUCT_ARN, &path.product_id),
            (TAG_PROVISIONED_PRODUCT_ARN, &launched.provisioned_product_id),
        ] {
            let carried_tag = carried.get(tag).cloned().unwrap_or_default();
            assert!(
                carried_tag.ends_with(&format!("/{identifier}")),
                "case {case:?}: expected the stack's {tag} to name {identifier}, got \
                 {carried_tag:?}"
            );
        }
    }
}

/// Create one IAM user and answer its ARN.
async fn create_user(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = iam(router, "CreateUser", &[("UserName", name)]).await;
    assert_eq!(
        status, 200,
        "seed iam:CreateUser({name}): expected 200, got {status} with {created}"
    );
    xml_value(&created, "Arn")
        .unwrap_or_else(|| panic!("seed iam:CreateUser({name}): no Arn in {created}"))
}

/// Create one access key for the IAM user `name` and answer its id.
async fn create_access_key(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = iam(router, "CreateAccessKey", &[("UserName", name)]).await;
    assert_eq!(
        status, 200,
        "seed iam:CreateAccessKey({name}): expected 200, got {status} with {created}"
    );
    xml_value(&created, "AccessKeyId")
        .unwrap_or_else(|| panic!("seed iam:CreateAccessKey({name}): no AccessKeyId in {created}"))
}

/// The principal the deployed stack records is the principal whose CREDENTIAL asked for the
/// launch: the IAM user that holds the access key the request was signed with, and the
/// identity this emulator publishes its own credential as for a key the account's identity
/// records hold no user for.
#[tokio::test]
async fn the_deployed_stack_records_the_principal_whose_credential_asked_for_the_launch() {
    let router = emulator_with_identity_services();
    create_template_bucket(&router).await;

    // Rows differ along the credential the launch was signed with: a key the account's
    // identity records hold a user for, and one they hold none for.
    let launcher = "launcher";
    let launcher_arn = create_user(&router, launcher).await;
    let launcher_key = create_access_key(&router, launcher).await;
    let rows: &[(&str, &str, &str, &str)] = &[
        (
            "an IAM user's own access key",
            "governed-by-a-user",
            &launcher_key,
            &launcher_arn,
        ),
        (
            "a key the identity records hold no user for",
            "governed-by-the-account",
            HARNESS_ACCESS_KEY_ID,
            &::service_common::caller_principal::ambient_user_arn(),
        ),
    ];

    for (case, name, access_key_id, expected) in rows {
        let path = seed_path(&router, name, &[]).await;
        // A launch is dependent on a principal as well as on a product and a portfolio, so
        // the principal that will ask for THIS launch is granted access to its path's
        // portfolio ([`seed_launch_path`] grants the harness's own, which is the second
        // row's caller).
        grant_principal_on_portfolio(&router, &path.portfolio_id, expected, "IAM").await;
        let launched = provision(&router, case, access_key_id, name, &path, &[]).await;
        let carried = tags_of_deployed_stack(&router, case, &launched.stack).await;
        let recorded = carried
            .get(TAG_PROVISIONING_PRINCIPAL_ARN)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            &recorded, expected,
            "case {case:?}: expected the stack's {TAG_PROVISIONING_PRINCIPAL_ARN} to be \
             {expected:?}, got {recorded:?}"
        );
    }
}

/// The change set behind a plan is what would deploy that provisioned product's stack, so
/// it carries the same composed tag set: the request's `Tags`, the launch path's TagOptions
/// and the tags recording the launch graph.
#[tokio::test]
async fn the_change_set_a_plan_is_carries_the_same_composed_tags() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    let principal_arn = ::service_common::caller_principal::ambient_user_arn();

    let name = "planned-governance";
    let path = seed_path(
        &router,
        name,
        &[TagOptionSeed {
            key: "costcenter",
            value: "cc-plan",
            on: On::Portfolio,
            active: true,
        }],
    )
    .await;

    let (status, planned) = call(
        &router,
        "CreateProvisionedProductPlan",
        &json!({
            "PlanName": format!("{name}-plan"),
            "PlanType": "CLOUDFORMATION",
            "ProductId": path.product_id,
            "ProvisioningArtifactId": path.version,
            "PathId": path.portfolio_id,
            "ProvisionedProductName": name,
            "IdempotencyToken": format!("{name}-plan-token"),
            "Tags": tag_list(&[("sim2real", "planned")]),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the plan to be created, got {status} with {planned}"
    );
    let provisioned_product_id = planned["ProvisionProductId"]
        .as_str()
        .unwrap_or_else(|| panic!("no ProvisionProductId in {planned}"))
        .to_string();
    let plan_id = planned["PlanId"]
        .as_str()
        .unwrap_or_else(|| panic!("no PlanId in {planned}"))
        .to_string();

    let (status, described) = call(
        &router,
        "DescribeProvisionedProduct",
        &json!({"Id": provisioned_product_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the planned provisioned product to be described, got {status} with {described}"
    );
    let launched = Launched {
        provisioned_product_arn: described["ProvisionedProductDetail"]["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("no Arn in {described}"))
            .to_string(),
        stack: format!("SC-{ACCOUNT_ID}-{provisioned_product_id}"),
        provisioned_product_id,
    };

    let (status, change_set) = cloudformation(
        &router,
        "DescribeChangeSet",
        &[
            ("ChangeSetName", &plan_id),
            // A change set is addressed by its name within the stack it is computed
            // against: "StackName must be specified if ChangeSetName is not specified as an
            // ARN".
            ("StackName", &launched.stack),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected CloudFormation to hold the change set {plan_id}, got {status} with {change_set}"
    );
    let carried = stack_tags(&change_set);
    let mut wanted: BTreeMap<String, String> = [("sim2real", "planned"), ("costcenter", "cc-plan")]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    wanted.extend(launch_graph_tags(&path, &launched, &principal_arn));
    assert_eq!(
        carried, wanted,
        "case \"a plan's change set\": expected {wanted:?}, got {carried:?}"
    );
}
