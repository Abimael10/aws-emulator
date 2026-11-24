// SPDX-License-Identifier: CC0-1.0

//! `ExecuteProvisionedProductPlan` over the wire, driven as awsJson1_1 requests through a
//! `MultiServiceRouter` carrying Service Catalog beside the services a plan is computed
//! out of and the ones the resources it plans really live in.
//!
//! The rules under test:
//!
//! * executing a plan "provisions or modifies a product based on the resource changes for
//!   the specified plan": the resource changes the plan lists are CARRIED OUT, so the
//!   resources the product version's template declares exist in the services that own
//!   them once the execution has settled, and the stack behind the provisioned product is
//!   the one the plan's change set belongs to;
//! * the execution is one request operation on that provisioned product, so it mints the
//!   record of that operation — `PROVISION_PRODUCT` for a provisioned product the plan
//!   itself brought into existence, `UPDATE_PROVISIONED_PRODUCT` for one this account
//!   already held — answered in the non-terminal status the clock owes the completion of,
//!   and every member of that record comes from the plan and the provisioned product it
//!   names;
//! * a plan is executed ONCE: a repeat of the request that executed it replays that
//!   execution's record, and any other execution of it is refused on the status the plan
//!   is now in, as is a plan whose change set was never computed;
//! * a plan id that resolves to no stored plan is refused as a missing plan.
//!
//! The rows differ along what the rules generalise over: how many resources the staged
//! template declares, the product type planned, whether the provisioned product already
//! existed, the tags and the provisioning parameters the plan carries.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    associate_product_with_portfolio, call, create_portfolio, create_product_with_info,
    create_template_bucket, emulator_with_template_services, error_code,
    first_provisioning_artifact_id, is_catalog_id, message, s3, stage_template,
    staged_template_url, tag_list, tag_map, ACCOUNT_ID,
};

/// The only `ProvisionedProductPlanType` the model declares
/// (`smithy_query query="shape" shape="ProvisionedProductPlanType"`).
const PLAN_TYPE: &str = "CLOUDFORMATION";

/// `ProvisionedProductPlanStatus`'s terminal value for a plan that was created
/// (`smithy_query query="shape" shape="ProvisionedProductPlanStatus"`:
/// CREATE_IN_PROGRESS, CREATE_SUCCESS, CREATE_FAILED, EXECUTE_IN_PROGRESS,
/// EXECUTE_SUCCESS, EXECUTE_FAILED).
const CREATE_SUCCESS: &str = "CREATE_SUCCESS";

/// `ProvisionedProductPlanStatus`'s terminal value for a plan that could not be created.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// `ProvisionedProductPlanStatus`'s terminal value for a plan whose execution completed.
const EXECUTE_SUCCESS: &str = "EXECUTE_SUCCESS";

/// `RecordStatus`'s non-terminal first value, "the request was created but the operation
/// has not started" (`smithy_query query="shape" shape="RecordStatus"`).
const CREATED: &str = "CREATED";

/// `RecordStatus`'s terminal value for an operation that completed.
const SUCCEEDED: &str = "SUCCEEDED";

/// The `RecordType` of the record of a request that provisions a product
/// (`smithy_query query="doc" shape="RecordDetail" member="RecordType"`:
/// PROVISION_PRODUCT, UPDATE_PROVISIONED_PRODUCT, TERMINATE_PROVISIONED_PRODUCT).
const PROVISION_PRODUCT: &str = "PROVISION_PRODUCT";

/// The `RecordType` of the record of a request that modifies an existing provisioned
/// product.
const UPDATE_PROVISIONED_PRODUCT: &str = "UPDATE_PROVISIONED_PRODUCT";

/// `ProvisionedProductStatus`'s stable value (`smithy_query query="shape"
/// shape="ProvisionedProductStatus"`: AVAILABLE, UNDER_CHANGE, TAINTED, ERROR,
/// PLAN_IN_PROGRESS).
const AVAILABLE: &str = "AVAILABLE";

/// The `OutputKey` Service Catalog reports the stack of a CloudFormation-backed
/// provisioned product under, beside the outputs that stack's own template declares.
const STACK_ARN_OUTPUT: &str = "CloudformationStackARN";

/// How long a poll keeps looking for a condition the emulator's clock owes before it
/// gives up. Far longer than any span this emulator settles on, so a loaded or slow host
/// cannot fail a case a fast one passes: the loop waits for the CONDITION, never for a
/// fixed time.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A staged product template declaring ONE bucket, named by a parameter so the launch the
/// plan plans is the one the plan's own parameters describe.
fn one_bucket_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "A staged product template",
        "Parameters": {"BucketName": {"Type": "String"}},
        "Resources": {
            "Data": {
                "Type": "AWS::S3::Bucket",
                "Properties": {"BucketName": {"Ref": "BucketName"}}
            }
        }
    })
    .to_string()
}

/// A staged template declaring TWO buckets, each named by a parameter, so a plan over it
/// carries out more than one resource change.
fn two_bucket_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {"BucketName": {"Type": "String"}, "ArchiveName": {"Type": "String"}},
        "Resources": {
            "Data": {
                "Type": "AWS::S3::Bucket",
                "Properties": {"BucketName": {"Ref": "BucketName"}}
            },
            "Archive": {
                "Type": "AWS::S3::Bucket",
                "Properties": {"BucketName": {"Ref": "ArchiveName"}}
            }
        }
    })
    .to_string()
}

/// One row of the execution table.
struct ExecutionCase {
    case: &'static str,
    /// The template the row's product version names.
    template: fn() -> String,
    /// Whether the template declares the second (archive) bucket, which the plan then has
    /// to name a parameter for.
    archives: bool,
    /// The `ProductType` of the product the row plans a provisioning of.
    product_type: &'static str,
    /// Whether this account already holds the provisioned product when the plan is
    /// created: a plan for a name it does not hold PROVISIONS a new one, a plan for one it
    /// holds MODIFIES it.
    provisioned_first: bool,
    tags: &'static [(&'static str, &'static str)],
    /// The `RecordType` the execution's record must carry.
    record_type: &'static str,
}

/// The launch one row plans a provisioning of.
struct Launch {
    product_id: String,
    artifact_id: String,
    portfolio_id: String,
}

/// Stage `template`, create one product of `product_type` whose only version names it, put
/// that product in one portfolio and answer the launch that makes.
async fn seed_launch(
    router: &MultiServiceRouter,
    slug: &str,
    product_type: &str,
    template: &str,
) -> Launch {
    let key = format!("{slug}/template.json");
    stage_template(router, &key, template).await;
    let product_id = create_product_with_info(
        router,
        &format!("{slug}-product"),
        product_type,
        // The accepted `Info` keys are the model's own:
        // `ProvisioningArtifactProperties$Info` documents
        // "Keys accepted: [ LoadTemplateFromURL, ImportFromPhysicalId ]".
        &json!({"LoadTemplateFromURL": staged_template_url(&key)}),
        slug,
    )
    .await;
    let artifact_id = first_provisioning_artifact_id(router, &product_id).await;
    let portfolio_id = create_portfolio(
        router,
        &format!("{slug}-portfolio"),
        "acme",
        &[],
        &format!("{slug}-portfolio"),
    )
    .await;
    associate_product_with_portfolio(router, &product_id, &portfolio_id).await;
    Launch {
        product_id,
        artifact_id,
        portfolio_id,
    }
}

/// `parameters` as the modelled `UpdateProvisioningParameters` list a plan carries.
fn plan_parameters(parameters: &[(String, String)]) -> Value {
    Value::Array(
        parameters
            .iter()
            .map(|(key, value)| json!({"Key": key, "Value": value, "UsePreviousValue": false}))
            .collect(),
    )
}

/// `parameters` as the modelled `ProvisioningParameters` list a provisioning request
/// carries.
fn provisioning_parameters(parameters: &[(String, String)]) -> Value {
    Value::Array(
        parameters
            .iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect(),
    )
}

/// Describe one plan.
async fn describe_plan(router: &MultiServiceRouter, plan_id: &str) -> (u16, Value) {
    call(
        router,
        "DescribeProvisionedProductPlan",
        &json!({"PlanId": plan_id}),
    )
    .await
}

/// The status a plan describe reports.
fn plan_status(case: &str, body: &Value) -> String {
    body["ProvisionedProductPlanDetails"]["Status"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no plan Status in {body}"))
        .to_string()
}

/// Look at the plan until it reports `wanted`, or the generous deadline passes: a plan's
/// status settles on the emulator's clock, so the loop waits for the CONDITION and never
/// for a fixed time.
async fn plan_settles_to(
    router: &MultiServiceRouter,
    case: &str,
    plan_id: &str,
    wanted: &str,
) -> Value {
    let started = std::time::Instant::now();
    loop {
        let (status, body) = describe_plan(router, plan_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected plan {plan_id} to be described, got {status} with {body}"
        );
        let reported = plan_status(case, &body);
        if reported == wanted {
            return body;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected plan {plan_id} to reach {wanted}, last reported \
             {reported:?} with {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at the provisioned product `name` until it reports `wanted`, or the deadline
/// passes, and answer the detail it then reports.
async fn provisioned_product_settles_to(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> Value {
    let started = std::time::Instant::now();
    loop {
        let (status, body) = call(router, "DescribeProvisionedProduct", &json!({"Name": name})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected provisioned product {name} to be described, got {status} \
             with {body}"
        );
        let detail = &body["ProvisionedProductDetail"];
        let reported = detail["Status"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no provisioned product Status in {body}"))
            .to_string();
        if reported == wanted {
            return detail.clone();
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected provisioned product {name} to reach {wanted}, last \
             reported {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at the record `record_id` until it reports `wanted`, or the deadline passes, and
/// answer the `RecordDetail` it then carries.
async fn record_settles_to(
    router: &MultiServiceRouter,
    case: &str,
    record_id: &str,
    wanted: &str,
) -> Value {
    let started = std::time::Instant::now();
    loop {
        let (status, body) = call(router, "DescribeRecord", &json!({"Id": record_id})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected record {record_id} to be described, got {status} with {body}"
        );
        let detail = &body["RecordDetail"];
        let reported = detail["Status"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no record Status in {body}"))
            .to_string();
        if reported == wanted {
            return detail.clone();
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected record {record_id} to reach {wanted}, last reported \
             {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether Amazon S3 holds the bucket `bucket` — the resource the executed template
/// declared, read from the service that OWNS it rather than from anything Service Catalog
/// reported.
async fn bucket_exists(router: &MultiServiceRouter, bucket: &str) -> bool {
    let (status, _) = s3(router, "HEAD", &format!("/{bucket}"), b"").await;
    status == 200
}

/// Look for the bucket `bucket` in Amazon S3 until it is there, or the deadline passes:
/// the resources an execution creates come into existence as the operation settles.
async fn bucket_appears(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let started = std::time::Instant::now();
    loop {
        if bucket_exists(router, bucket).await {
            return;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected the executed plan to create the bucket {bucket} in \
             Amazon S3, and Amazon S3 holds no such bucket"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The outputs Service Catalog reports for the provisioned product `name`, as
/// `key -> value`.
async fn provisioned_product_outputs(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> BTreeMap<String, String> {
    let (status, body) = call(
        router,
        "GetProvisionedProductOutputs",
        &json!({"ProvisionedProductName": name}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the outputs of {name} to be reported, got {status} with {body}"
    );
    body["Outputs"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an Outputs list, got {body}"))
        .iter()
        .map(|output| {
            (
                output["OutputKey"].as_str().unwrap_or_default().to_string(),
                output["OutputValue"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Executing a plan carries the resource changes it lists OUT: the resources the product
/// version's template declares come to exist in the services that own them, the record of
/// the operation says which operation it was, and the plan and the provisioned product
/// report that execution afterwards.
#[tokio::test]
async fn executing_a_plan_carries_the_planned_change_out() {
    let cases = [
        ExecutionCase {
            case: "a plan for a new provisioned product, one declared bucket, no tags",
            template: one_bucket_template,
            archives: false,
            product_type: "CLOUD_FORMATION_TEMPLATE",
            provisioned_first: false,
            tags: &[],
            record_type: PROVISION_PRODUCT,
        },
        ExecutionCase {
            case: "a plan for a new provisioned product, two declared buckets, two tags",
            template: two_bucket_template,
            archives: true,
            product_type: "CLOUD_FORMATION_TEMPLATE",
            provisioned_first: false,
            tags: &[("env", "prod"), ("owner", "platform")],
            record_type: PROVISION_PRODUCT,
        },
        ExecutionCase {
            case: "a plan for a provisioned product this account already holds",
            template: one_bucket_template,
            archives: false,
            product_type: "CLOUD_FORMATION_TEMPLATE",
            provisioned_first: true,
            tags: &[("env", "stage")],
            record_type: UPDATE_PROVISIONED_PRODUCT,
        },
        ExecutionCase {
            case: "a Marketplace product's plan for a new provisioned product",
            template: one_bucket_template,
            archives: false,
            product_type: "MARKETPLACE",
            provisioned_first: false,
            tags: &[("tier", "2")],
            record_type: PROVISION_PRODUCT,
        },
    ];

    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    for (row, case) in cases.iter().enumerate() {
        let slug = format!("exec-{row}");
        let launch = seed_launch(&router, &slug, case.product_type, &(case.template)()).await;
        let provisioned_name = format!("{slug}-pp");
        // The buckets the row's plan declares, named after the row so no two rows share a
        // bucket: the plan's parameters are what name them.
        let mut planned: Vec<(String, String)> =
            vec![("BucketName".to_string(), format!("{slug}-planned"))];
        if case.archives {
            planned.push(("ArchiveName".to_string(), format!("{slug}-archive")));
        }

        // A row whose provisioned product already exists provisions it FIRST, with
        // different bucket names: the plan then plans a change to it rather than its
        // creation.
        if case.provisioned_first {
            let already: Vec<(String, String)> = planned
                .iter()
                .map(|(key, value)| (key.clone(), format!("{value}-before")))
                .collect();
            let (status, provisioned) = call(
                &router,
                "ProvisionProduct",
                &json!({
                    "ProductId": launch.product_id,
                    "ProvisioningArtifactId": launch.artifact_id,
                    "ProvisionedProductName": provisioned_name,
                    "ProvisioningParameters": provisioning_parameters(&already),
                    "ProvisionToken": format!("{slug}-provision"),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: expected the product to be provisioned first, got {status} with \
                 {provisioned}",
                case.case
            );
            provisioned_product_settles_to(&router, case.case, &provisioned_name, AVAILABLE).await;
        }

        // The plan, and the change set behind it.
        let mut plan_request = json!({
            "PlanName": format!("{slug}-plan"),
            "PlanType": PLAN_TYPE,
            "ProductId": launch.product_id,
            "ProvisionedProductName": provisioned_name,
            "ProvisioningArtifactId": launch.artifact_id,
            "PathId": launch.portfolio_id,
            "ProvisioningParameters": plan_parameters(&planned),
            "IdempotencyToken": format!("{slug}-plan-token"),
        });
        if !case.tags.is_empty() {
            plan_request["Tags"] = tag_list(case.tags);
        }
        let (status, created) = call(&router, "CreateProvisionedProductPlan", &plan_request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the plan to be created, got {status} with {created}",
            case.case
        );
        let plan_id = created["PlanId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: no PlanId in {created}", case.case))
            .to_string();
        let provision_product_id = created["ProvisionProductId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: no ProvisionProductId in {created}", case.case))
            .to_string();
        let before = plan_settles_to(&router, case.case, &plan_id, CREATE_SUCCESS).await;
        let planned_at = before["ProvisionedProductPlanDetails"]["CreatedTime"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {:?}: no plan CreatedTime in {before}", case.case));

        // The execution itself: the record it answers is the record of ONE operation on
        // the provisioned product, answered in the non-terminal status.
        let (status, executed) = call(
            &router,
            "ExecuteProvisionedProductPlan",
            &json!({"PlanId": plan_id, "IdempotencyToken": format!("{slug}-exec-token")}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the plan to be executed, got {status} with {executed}",
            case.case
        );
        let detail = &executed["RecordDetail"];
        let record_id = detail["RecordId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: no RecordId in {executed}", case.case))
            .to_string();
        assert!(
            is_catalog_id("rec", &record_id),
            "case {:?}: expected a minted record id, got {record_id:?}",
            case.case
        );
        assert_eq!(
            detail["Status"].as_str(),
            Some(CREATED),
            "case {:?}: expected the execution to be answered in flight, got {executed}",
            case.case
        );
        assert_eq!(
            detail["RecordType"].as_str(),
            Some(case.record_type),
            "case {:?}: expected the record type of the operation the execution is, got \
             {executed}",
            case.case
        );
        assert_eq!(
            detail["ProvisionedProductId"].as_str(),
            Some(provision_product_id.as_str()),
            "case {:?}: expected the provisioned product the plan named, got {executed}",
            case.case
        );
        assert_eq!(
            detail["ProvisionedProductName"].as_str(),
            Some(provisioned_name.as_str()),
            "case {:?}: expected the provisioned product's name, got {executed}",
            case.case
        );
        assert_eq!(
            detail["ProductId"].as_str(),
            Some(launch.product_id.as_str()),
            "case {:?}: expected the product the plan provisions, got {executed}",
            case.case
        );
        assert_eq!(
            detail["ProvisioningArtifactId"].as_str(),
            Some(launch.artifact_id.as_str()),
            "case {:?}: expected the product version the plan provisions, got {executed}",
            case.case
        );
        assert_eq!(
            detail["PathId"].as_str(),
            Some(launch.portfolio_id.as_str()),
            "case {:?}: expected the launch path the plan provisions through, got {executed}",
            case.case
        );
        assert_eq!(
            detail["RecordErrors"],
            json!([]),
            "case {:?}: expected no errors on an execution in flight, got {executed}",
            case.case
        );
        let expected_tags: BTreeMap<String, String> = case
            .tags
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        assert_eq!(
            tag_map(case.case, &detail["RecordTags"]),
            expected_tags,
            "case {:?}: expected the tags the plan carried, got {executed}",
            case.case
        );

        // What the execution CARRIED OUT: the record completes, and the buckets the
        // template declares exist in Amazon S3, which is the service that owns them.
        let settled = record_settles_to(&router, case.case, &record_id, SUCCEEDED).await;
        assert_eq!(
            settled["RecordType"].as_str(),
            Some(case.record_type),
            "case {:?}: expected the settled record to be the same operation, got {settled}",
            case.case
        );
        for (_, bucket) in &planned {
            bucket_appears(&router, case.case, bucket).await;
        }

        // The provisioned product the execution provisioned or modified: settled, carrying
        // this execution's record as its last and its last successful provisioning, and
        // the product version the plan named.
        let provisioned =
            provisioned_product_settles_to(&router, case.case, &provisioned_name, AVAILABLE).await;
        assert_eq!(
            provisioned["LastRecordId"].as_str(),
            Some(record_id.as_str()),
            "case {:?}: expected the execution's record to be the last one, got {provisioned}",
            case.case
        );
        assert_eq!(
            provisioned["LastProvisioningRecordId"].as_str(),
            Some(record_id.as_str()),
            "case {:?}: expected the execution to be the last provisioning request, got \
             {provisioned}",
            case.case
        );
        assert_eq!(
            provisioned["LastSuccessfulProvisioningRecordId"].as_str(),
            Some(record_id.as_str()),
            "case {:?}: expected the execution to be the last successful provisioning, got \
             {provisioned}",
            case.case
        );
        assert_eq!(
            provisioned["ProvisioningArtifactId"].as_str(),
            Some(launch.artifact_id.as_str()),
            "case {:?}: expected the product version the plan provisioned, got {provisioned}",
            case.case
        );

        // The stack the executed change set belongs to IS the resource behind the
        // provisioned product, which is what its outputs report.
        let outputs = provisioned_product_outputs(&router, case.case, &provisioned_name).await;
        let stack_arn = outputs.get(STACK_ARN_OUTPUT).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the stack of the executed plan to be reported as \
                 {STACK_ARN_OUTPUT}, got {outputs:?}",
                case.case
            )
        });
        // AWS names the stack of a provisioned product `SC-<account id>-<provisioned
        // product id>`, and a stack's id is its ARN
        // (`arn:aws:cloudformation:<region>:<account>:stack/<name>/<id>`).
        assert!(
            stack_arn.contains(&format!(
                "stack/SC-{ACCOUNT_ID}-{provision_product_id}/"
            )),
            "case {:?}: expected the stack of this provisioned product, got {stack_arn:?}",
            case.case
        );

        // And the plan itself reports the execution: its terminal execution status, and an
        // update instant no earlier than its creation.
        let after = plan_settles_to(&router, case.case, &plan_id, EXECUTE_SUCCESS).await;
        let details = &after["ProvisionedProductPlanDetails"];
        assert_eq!(
            details.get("StatusMessage"),
            None,
            "case {:?}: expected no status message on an execution that succeeded, got {after}",
            case.case
        );
        let updated_at = details["UpdatedTime"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {:?}: no plan UpdatedTime in {after}", case.case));
        assert!(
            updated_at >= planned_at,
            "case {:?}: expected the executed plan's update instant to be no earlier than \
             its creation ({planned_at}), got {updated_at}",
            case.case
        );
        assert_eq!(
            details["PlanId"].as_str(),
            Some(plan_id.as_str()),
            "case {:?}: expected the executed plan to still be addressable, got {after}",
            case.case
        );
    }
}

/// A plan is executed ONCE: a repeat of the request that executed it replays that
/// execution's record, and any other execution of it is refused on the status the plan is
/// now in.
#[tokio::test]
async fn a_plan_is_executed_once() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    let launch = seed_launch(
        &router,
        "once",
        "CLOUD_FORMATION_TEMPLATE",
        &one_bucket_template(),
    )
    .await;

    let (status, created) = call(
        &router,
        "CreateProvisionedProductPlan",
        &json!({
            "PlanName": "once-plan",
            "PlanType": PLAN_TYPE,
            "ProductId": launch.product_id,
            "ProvisionedProductName": "once-pp",
            "ProvisioningArtifactId": launch.artifact_id,
            "ProvisioningParameters": plan_parameters(&[(
                "BucketName".to_string(),
                "once-planned".to_string(),
            )]),
            "IdempotencyToken": "once-plan-token",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the plan to be created, got {status} with {created}"
    );
    let plan_id = created["PlanId"]
        .as_str()
        .unwrap_or_else(|| panic!("no PlanId in {created}"))
        .to_string();
    plan_settles_to(&router, "one execution", &plan_id, CREATE_SUCCESS).await;

    let execute = |token: &'static str| {
        let request = json!({"PlanId": plan_id.clone(), "IdempotencyToken": token});
        let router = &router;
        async move { call(router, "ExecuteProvisionedProductPlan", &request).await }
    };

    let (status, executed) = execute("once-exec-token").await;
    assert_eq!(
        status, 200,
        "expected the plan to be executed, got {status} with {executed}"
    );

    // The SAME request again: `IdempotencyToken` is `@idempotencyToken`, so it answers
    // the execution it already made rather than carrying the change out twice.
    let (status, again) = execute("once-exec-token").await;
    assert_eq!(
        status, 200,
        "expected the repeated request to be answered, got {status} with {again}"
    );
    assert_eq!(
        again["RecordDetail"]["RecordId"], executed["RecordDetail"]["RecordId"],
        "expected the repeat to replay the execution's own record, got {again}"
    );

    // Any OTHER execution of that plan is refused on the status the plan is now in, which
    // is the status of its execution.
    plan_settles_to(&router, "one execution", &plan_id, EXECUTE_SUCCESS).await;
    let (status, refused) = execute("once-exec-token-two").await;
    assert_eq!(
        status, 400,
        "expected a second execution to be refused, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "InvalidStateException",
        "expected the state error for a plan that cannot be executed, got {refused}"
    );
    assert_eq!(
        message(&refused),
        format!(
            "Plan {plan_id} cannot be executed in status {EXECUTE_SUCCESS}: the plan must be in \
             status {CREATE_SUCCESS}."
        ),
        "expected the refusal to name the plan's status and the one it must be in, got {refused}"
    );
}

/// A plan whose change set was never computed lists no settled resource changes, so it
/// cannot be executed either — the refusal names the status it is in.
#[tokio::test]
async fn a_plan_that_was_not_created_cannot_be_executed() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    // A product version naming the physical id of a stack that does not exist addresses a
    // template nothing can read, so CloudFormation cannot plan the change at all.
    let product_id = create_product_with_info(
        &router,
        "unplanned-product",
        "CLOUD_FORMATION_TEMPLATE",
        &json!({"ImportFromPhysicalId": "SC-000000000000-absent"}),
        "unplanned",
    )
    .await;
    let artifact_id = first_provisioning_artifact_id(&router, &product_id).await;
    let portfolio_id =
        create_portfolio(&router, "unplanned-portfolio", "acme", &[], "unplanned-pf").await;
    associate_product_with_portfolio(&router, &product_id, &portfolio_id).await;

    let (status, created) = call(
        &router,
        "CreateProvisionedProductPlan",
        &json!({
            "PlanName": "unplanned-plan",
            "PlanType": PLAN_TYPE,
            "ProductId": product_id,
            "ProvisionedProductName": "unplanned-pp",
            "ProvisioningArtifactId": artifact_id,
            "IdempotencyToken": "unplanned-plan-token",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the plan to be created, got {status} with {created}"
    );
    let plan_id = created["PlanId"]
        .as_str()
        .unwrap_or_else(|| panic!("no PlanId in {created}"))
        .to_string();
    plan_settles_to(&router, "an uncomputed plan", &plan_id, CREATE_FAILED).await;

    let (status, refused) = call(
        &router,
        "ExecuteProvisionedProductPlan",
        &json!({"PlanId": plan_id, "IdempotencyToken": "unplanned-exec-token"}),
    )
    .await;
    assert_eq!(
        status, 400,
        "expected the execution to be refused, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "InvalidStateException",
        "expected the state error for a plan that was not created, got {refused}"
    );
    assert_eq!(
        message(&refused),
        format!(
            "Plan {plan_id} cannot be executed in status {CREATE_FAILED}: the plan must be in \
             status {CREATE_SUCCESS}."
        ),
        "expected the refusal to name the plan's status and the one it must be in, got {refused}"
    );

    // Nothing was provisioned for it: the provisioned product the plan created is still
    // the one the model documents for a plan nothing has executed.
    let (status, provisioned) = call(
        &router,
        "DescribeProvisionedProduct",
        &json!({"Name": "unplanned-pp"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the planned provisioned product to exist, got {status} with {provisioned}"
    );
    assert_eq!(
        provisioned["ProvisionedProductDetail"].get("LastRecordId"),
        None,
        "expected a refused execution to record nothing, got {provisioned}"
    );
}

/// A plan id that resolves to no stored plan is refused as a missing plan, whatever shape
/// the identifier has.
#[tokio::test]
async fn an_unknown_plan_cannot_be_executed() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    let launch = seed_launch(
        &router,
        "unknown",
        "CLOUD_FORMATION_TEMPLATE",
        &one_bucket_template(),
    )
    .await;

    // Each row is an identifier the modelled `Id` pattern admits that addresses no plan:
    // one of this service's own minted shapes, an identifier of ANOTHER kind of resource,
    // and a bare word.
    let cases = [
        ("a plan id nothing minted", "plan-000000000000".to_string()),
        ("a portfolio id", launch.portfolio_id.clone()),
        ("a product id", launch.product_id.clone()),
        ("a bare word", "nope".to_string()),
    ];
    for (case, plan_id) in cases {
        let (status, refused) = call(
            &router,
            "ExecuteProvisionedProductPlan",
            &json!({"PlanId": plan_id, "IdempotencyToken": "unknown-exec-token"}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the execution to be refused, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundException",
            "case {case:?}: expected the not-found error, got {refused}"
        );
        assert_eq!(
            message(&refused),
            format!("No plan with ID {plan_id} exists."),
            "case {case:?}: expected the sentence a missing plan is refused with, got {refused}"
        );
    }
}
