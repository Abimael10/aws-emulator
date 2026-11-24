// SPDX-License-Identifier: CC0-1.0

//! `DescribeRecord` over the wire, driven as awsJson1_1 requests through the same
//! `MultiServiceRouter` the production emulator mounts.
//!
//! The rule under test: `DescribeRecord` REPORTS the stored record its `Id` names —
//! whichever request operation minted it, in whatever status the clock has it, and whether
//! or not the resources that request operated on still exist — and reports as that record's
//! `RecordOutputs` what the request PRODUCED, which is the stack a successful provisioning
//! stood up and nothing at all for any other record.
//!
//! The rows differ along which request operation minted the record (a provisioning and a
//! termination), whether the operation is still in flight or has settled, and whether the
//! provisioned product it is about still exists — so what is asserted is the class of
//! records a request operation leaves behind rather than one of them. The last rows are the
//! case a record OUTLIVES what it describes: "this operation does not delete any records
//! associated with the provisioned product", so both records of a terminated provisioned
//! product stay readable by their own ids once the provisioned product itself is gone.
//!
//! Every assertion is about STORED state: each response is checked against what the request
//! operation that minted the record answered with, or against what another reader of the
//! same state reports.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    seed_launch_path, call, create_portfolio, create_product_with_info,
    create_template_bucket, emulator_with_template_services, error_code, first_provisioning_artifact_id,
    message, s3, stage_template, staged_template_url,
};

/// The `ProductType` of a CloudFormation-backed product, the only kind whose launch stands
/// a stack up (`smithy_query query="shape" shape="ProductType"`).
const CFN_PRODUCT: &str = "CLOUD_FORMATION_TEMPLATE";

/// `RecordStatus`'s non-terminal first value: "the request was created but the operation has
/// not started" (`smithy_query query="shape" shape="RecordStatus"`: CREATED, IN_PROGRESS,
/// IN_PROGRESS_IN_ERROR, SUCCEEDED, FAILED).
const CREATED: &str = "CREATED";

/// `RecordStatus`'s terminal value for a request operation that completed.
const SUCCEEDED: &str = "SUCCEEDED";

/// The `RecordType` of the record `ProvisionProduct` mints
/// (`smithy_query query="doc" shape="RecordDetail" member="RecordType"`: PROVISION_PRODUCT,
/// UPDATE_PROVISIONED_PRODUCT, TERMINATE_PROVISIONED_PRODUCT).
const PROVISION_PRODUCT: &str = "PROVISION_PRODUCT";

/// The `RecordType` of the record `TerminateProvisionedProduct` mints; see
/// [`PROVISION_PRODUCT`] for the three values the member documents.
const TERMINATE_PROVISIONED_PRODUCT: &str = "TERMINATE_PROVISIONED_PRODUCT";

/// How long a poll keeps looking for a condition the emulator's clock owes before it gives
/// up. Far longer than any span this emulator settles on, so a loaded or slow host cannot
/// fail a case a fast one passes: every loop below waits for the CONDITION, never for a
/// fixed time.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What one launch left behind: the provisioned product it minted and the record of the
/// request that minted it.
struct Launched {
    provisioned_product_id: String,
    provisioned_product_name: String,
    record_id: String,
    /// The `RecordDetail` the provisioning request itself answered with, which every later
    /// read of that record is checked against.
    accepted_detail: Value,
}

/// A template standing one bucket up and declaring that bucket's name as its one output —
/// the smallest template whose launch has something to report as the record's
/// `RecordOutputs`.
fn template_for(bucket: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "A staged product template",
        "Resources": {
            "Data": {"Type": "AWS::S3::Bucket", "Properties": {"BucketName": bucket}}
        },
        "Outputs": {"BucketName": {"Value": bucket}}
    })
    .to_string()
}

/// Stage a template, create the product and the portfolio that gives it a launch path, and
/// provision it under `name` — the whole administrator-then-end-user path a launch takes,
/// driven through this crate's shared harness.
///
/// Asserts only what the request is ACCEPTED as: the record answers in the non-terminal
/// status, carrying no errors, because nothing has happened yet. The launch is settled by
/// the clock, so a caller polls for it afterwards.
async fn launch(router: &MultiServiceRouter, name: &str) -> Launched {
    let key = format!("{name}.template.json");
    stage_template(router, &key, &template_for(name)).await;
    let portfolio = create_portfolio(router, &format!("{name}-portfolio"), "acme", &[], name).await;
    let product = create_product_with_info(
        router,
        name,
        CFN_PRODUCT,
        &json!({"LoadTemplateFromURL": staged_template_url(&key)}),
        name,
    )
    .await;
    let version = first_provisioning_artifact_id(router, &product).await;
    seed_launch_path(router, &product, &portfolio).await;

    let (status, provisioned) = call(
        router,
        "ProvisionProduct",
        &json!({
            "ProductId": product,
            "ProvisioningArtifactId": version,
            "ProvisionedProductName": name,
            "ProvisionToken": format!("{name}-provision"),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: expected the launch to be accepted, got {status} with {provisioned}"
    );
    let detail = provisioned["RecordDetail"].clone();
    assert_eq!(
        (detail["Status"].as_str(), detail["RecordType"].as_str()),
        (Some(CREATED), Some(PROVISION_PRODUCT)),
        "case {name:?}: expected the request to be accepted as a {CREATED} \
         {PROVISION_PRODUCT} record, got {provisioned}"
    );

    Launched {
        provisioned_product_id: detail["ProvisionedProductId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}: no ProvisionedProductId in {provisioned}"))
            .to_string(),
        provisioned_product_name: name.to_string(),
        record_id: detail["RecordId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {name:?}: no RecordId in {provisioned}"))
            .to_string(),
        accepted_detail: detail,
    }
}

/// Describe one record, asserting only that the read is SERVED, and answer the whole body.
async fn describe_record(router: &MultiServiceRouter, case: &str, record_id: &str) -> Value {
    let (status, described) = call(router, "DescribeRecord", &json!({"Id": record_id})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected record {record_id} to be described, got {status} with {described}"
    );
    described
}

/// Look at `record_id` until the record it names reports `wanted`, or the deadline passes,
/// and answer the body reported then.
async fn record_settles_to(
    router: &MultiServiceRouter,
    case: &str,
    record_id: &str,
    wanted: &str,
) -> Value {
    let started = std::time::Instant::now();
    loop {
        let described = describe_record(router, case, record_id).await;
        if described["RecordDetail"]["Status"].as_str() == Some(wanted) {
            return described;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected record {record_id} to reach {wanted}, last reported \
             {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at the provisioned product `name` until reading it is REFUSED, or the deadline
/// passes — the termination the clock owes having fallen due.
async fn stops_existing(router: &MultiServiceRouter, case: &str, name: &str) {
    let started = std::time::Instant::now();
    loop {
        let (status, body) = call(
            router,
            "DescribeProvisionedProduct",
            &json!({"Name": name}),
        )
        .await;
        if status != 200 {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ResourceNotFoundException"),
                "case {case:?}: a provisioned product that stopped existing is the modelled \
                 not-found, got {status} with {body}"
            );
            assert_eq!(
                message(&body),
                format!("Provisioned product not found: {name}"),
                "case {case:?}: unexpected not-found sentence, got {body}"
            );
            return;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected provisioned product {name} to stop existing, last reported \
             {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The members of a `RecordDetail` that are the same in every report of one record: what the
/// request operation stored about itself, as against the status and the update time the
/// clock moves.
fn stored_members(detail: &Value) -> Value {
    json!({
        "RecordId": detail["RecordId"],
        "RecordType": detail["RecordType"],
        "ProvisionedProductId": detail["ProvisionedProductId"],
        "ProvisionedProductName": detail["ProvisionedProductName"],
        "ProvisionedProductType": detail["ProvisionedProductType"],
        "ProductId": detail["ProductId"],
        "ProvisioningArtifactId": detail["ProvisioningArtifactId"],
        "PathId": detail["PathId"],
        "CreatedTime": detail["CreatedTime"],
        "RecordTags": detail["RecordTags"],
    })
}

/// The `OutputKey`s of a reported `RecordOutputs` list, in the order reported.
fn output_keys(case: &str, outputs: &Value) -> Vec<String> {
    outputs
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RecordOutputs list, got {outputs}"))
        .iter()
        .map(|output| {
            output["OutputKey"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: an output has no OutputKey: {output}"))
                .to_string()
        })
        .collect()
}

/// Every record a request operation minted is described by its own id, in the status the
/// clock has it and with the outputs that request produced — a provisioning's and a
/// termination's alike, and both of them after the provisioned product they are about has
/// stopped existing.
#[tokio::test]
async fn every_record_a_request_operation_minted_is_described_by_its_id() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;

    // Two launches, started together so the span each one's settling is owed over is the
    // same span for both: one is read while its provisioning is still in flight and then
    // once it has succeeded, the other is terminated afterwards.
    let read_in_flight = launch(&router, "record-read-in-flight").await;
    let terminated = launch(&router, "record-read-terminated").await;

    // A provisioning still in flight: the record is already described, reports the
    // non-terminal status, and has produced NOTHING yet, so it reports no outputs and no
    // errors.
    let case = "a provisioning in flight";
    let described = describe_record(&router, case, &read_in_flight.record_id).await;
    let detail = &described["RecordDetail"];
    assert_eq!(
        stored_members(detail),
        stored_members(&read_in_flight.accepted_detail),
        "case {case:?}: expected the record the request answered with, got {described}"
    );
    assert_eq!(
        detail["Status"].as_str(),
        Some(CREATED),
        "case {case:?}: expected the non-terminal status, got {described}"
    );
    assert_eq!(
        detail["RecordErrors"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected a request in flight to carry no errors, got {described}"
    );
    assert_eq!(
        described["RecordOutputs"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected a request that has produced nothing yet to report no \
         outputs, got {described}"
    );
    assert!(
        described["NextPageToken"].is_null(),
        "case {case:?}: expected no further page of an empty list, got {described}"
    );

    // The same record once the clock has settled the provisioning: the terminal status, and
    // as its outputs what the launch PRODUCED — the output the template declares, read from
    // the stack that stands behind it.
    let case = "a provisioning that succeeded";
    let described = record_settles_to(&router, case, &read_in_flight.record_id, SUCCEEDED).await;
    assert_eq!(
        stored_members(&described["RecordDetail"]),
        stored_members(&read_in_flight.accepted_detail),
        "case {case:?}: expected settling to change nothing the request stored, got {described}"
    );
    let keys = output_keys(case, &described["RecordOutputs"]);
    assert!(
        keys.contains(&"BucketName".to_string()),
        "case {case:?}: expected the output the template declares, got {keys:?} in {described}"
    );
    // The resource the stack declared really exists in the service that owns it, so those
    // outputs are a launch's rather than a report of one.
    let (status, held) = s3(
        &router,
        "HEAD",
        &format!("/{}", read_in_flight.provisioned_product_name),
        b"",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the stack's bucket to exist, got {status} with {held}"
    );

    // A read changes nothing a later caller sees: two describes of one settled record report
    // exactly the same thing.
    let again = describe_record(&router, case, &read_in_flight.record_id).await;
    assert_eq!(
        again, described,
        "case {case:?}: expected a second read of the same record to report the same thing, got \
         {again} against {described}"
    );

    // The other launch is terminated once it has succeeded, which mints a SECOND record
    // about the same provisioned product.
    let case = "a termination in flight";
    record_settles_to(&router, case, &terminated.record_id, SUCCEEDED).await;
    let (status, body) = call(
        &router,
        "TerminateProvisionedProduct",
        &json!({
            "ProvisionedProductId": terminated.provisioned_product_id,
            "TerminateToken": "record-read-terminate",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the termination to be served, got {status} with {body}"
    );
    let accepted_termination = body["RecordDetail"].clone();
    let termination_record_id = accepted_termination["RecordId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no RecordId in {body}"))
        .to_string();
    assert_ne!(
        termination_record_id, terminated.record_id,
        "case {case:?}: the termination mints its own record, got {termination_record_id:?} in \
         {body}"
    );

    // The termination's record is described by its own id, as the record of a TERMINATION of
    // that provisioned product — and a termination produces nothing, so it has no outputs
    // whatever its status.
    let described = describe_record(&router, case, &termination_record_id).await;
    let detail = &described["RecordDetail"];
    assert_eq!(
        stored_members(detail),
        stored_members(&accepted_termination),
        "case {case:?}: expected the record the termination answered with, got {described}"
    );
    assert_eq!(
        (
            detail["RecordType"].as_str(),
            detail["Status"].as_str(),
            detail["ProvisionedProductId"].as_str(),
        ),
        (
            Some(TERMINATE_PROVISIONED_PRODUCT),
            Some(CREATED),
            Some(terminated.provisioned_product_id.as_str()),
        ),
        "case {case:?}: unexpected record for the termination in flight, got {described}"
    );
    assert_eq!(
        described["RecordOutputs"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected a termination to report no outputs, got {described}"
    );

    // Once the clock settles the termination the provisioned product stops existing…
    stops_existing(
        &router,
        "a terminated provisioned product",
        &terminated.provisioned_product_name,
    )
    .await;

    // …and both records about it are still described by their own ids: "this operation does
    // not delete any records associated with the provisioned product".
    let case = "the termination of a provisioned product that is gone";
    let described = describe_record(&router, case, &termination_record_id).await;
    assert_eq!(
        (
            described["RecordDetail"]["RecordType"].as_str(),
            described["RecordDetail"]["Status"].as_str(),
            described["RecordDetail"]["RecordErrors"].as_array().map(Vec::len),
            described["RecordOutputs"].as_array().map(Vec::len),
        ),
        (
            Some(TERMINATE_PROVISIONED_PRODUCT),
            Some(SUCCEEDED),
            Some(0),
            Some(0),
        ),
        "case {case:?}: expected the settled termination's own record, got {described}"
    );
    assert_eq!(
        stored_members(&described["RecordDetail"]),
        stored_members(&accepted_termination),
        "case {case:?}: expected the record to outlive the provisioned product unchanged, got \
         {described}"
    );

    let case = "the provisioning of a provisioned product that is gone";
    let described = describe_record(&router, case, &terminated.record_id).await;
    assert_eq!(
        stored_members(&described["RecordDetail"]),
        stored_members(&terminated.accepted_detail),
        "case {case:?}: expected the provisioning's record to outlive the provisioned product \
         unchanged, got {described}"
    );
    assert_eq!(
        described["RecordDetail"]["Status"].as_str(),
        Some(SUCCEEDED),
        "case {case:?}: expected the provisioning that succeeded to stay succeeded, got \
         {described}"
    );
    // Its outputs were the STACK's, and the termination tore that stack down, so there are
    // none left to report: the resource it stood up is gone from the service that owns it.
    assert_eq!(
        output_keys(case, &described["RecordOutputs"]),
        Vec::<String>::new(),
        "case {case:?}: expected no outputs once the stack behind them is torn down, got \
         {described}"
    );
    let (status, held) = s3(
        &router,
        "HEAD",
        &format!("/{}", terminated.provisioned_product_name),
        b"",
    )
    .await;
    assert_ne!(
        status, 200,
        "case {case:?}: expected the terminated launch's bucket to be gone, got {status} with \
         {held}"
    );
}

/// A record identifier the store does not carry addresses no record, for every FORM such an
/// identifier can take — the one failure `DescribeRecord` models.
#[tokio::test]
async fn a_record_identifier_the_store_does_not_carry_is_refused() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    let launched = launch(&router, "record-read-refusals").await;

    // Rows differ along WHY the identifier names no record of this store: the shape this
    // service mints with nothing behind it, an identifier of another shape entirely, the id
    // of the provisioned product the record is about rather than the record's own, and the
    // id of the product behind it.
    let cases = [
        "rec-aaaaaaaaaaaa",
        "not-a-record-identifier",
        launched.provisioned_product_id.as_str(),
        launched.accepted_detail["ProductId"]
            .as_str()
            .unwrap_or_else(|| panic!("no ProductId in {}", launched.accepted_detail)),
    ];
    for id in cases {
        let (status, refused) = call(&router, "DescribeRecord", &json!({"Id": id})).await;
        assert_eq!(
            status, 400,
            "case {id:?}: expected the record lookup to be refused, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundException",
            "case {id:?}: expected the modelled not-found, got {refused}"
        );
        assert_eq!(
            message(&refused),
            format!("Record not found: {id}"),
            "case {id:?}: expected the service's not-found sentence, got {refused}"
        );
    }

    // The record the launch really minted is described, so the refusals above are about the
    // identifier rather than about the read.
    let described = describe_record(&router, "the launch's own record", &launched.record_id).await;
    assert_eq!(
        described["RecordDetail"]["RecordId"].as_str(),
        Some(launched.record_id.as_str()),
        "expected the record the launch minted, got {described}"
    );
}
