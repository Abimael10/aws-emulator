// SPDX-License-Identifier: CC0-1.0

//! What becomes of a launch the service that owns the deployment REFUSES, driven as
//! awsJson1_1 requests through a `MultiServiceRouter` carrying Service Catalog beside the
//! services a launch is made out of.
//!
//! The rule under test: a provisioning request is accepted synchronously — the record it
//! answers with is `CREATED` and carries no errors — and the launch it starts is then
//! SETTLED by what the deployment actually did. A deployment CloudFormation refuses settles
//! the launch as a failure:
//!
//! * the record of that request reaches `FAILED` and carries CloudFormation's own refusal
//!   as its single `RecordError`;
//! * the provisioned product reports `ERROR` — "the provisioned product exists but the
//!   stack is not running. For example, CloudFormation received a parameter value that was
//!   not valid and could not launch the stack" — with that same sentence as its
//!   `StatusMessage`;
//! * no provisioning of it has SUCCEEDED, so it names no last successful provisioning
//!   record and `GetProvisionedProductOutputs` refuses the question instead of answering an
//!   empty list;
//! * no stack stands behind it, which CloudFormation itself is asked to confirm.
//!
//! A deployment CloudFormation accepts settles the launch the other way, which the last
//! test here is the control for.
//!
//! The rows differ along WHICH part of the template's own input contract the values in
//! force break — the admitted value set, a length bound, the declared type, a numeric
//! bound, a pattern, a key the template declares nothing for, and a declared parameter
//! nothing puts a value in force for — so the rule is asserted over the class of refused
//! deployments rather than over one of them. Every sentence asserted is the one
//! CloudFormation words for that constraint, since the template is CloudFormation's to
//! judge: what Service Catalog adds is the clause naming the call it came from.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    associate_product_with_portfolio, call, create_portfolio, create_product_with_info,
    create_template_bucket, emulator_with_template_services, error_code,
    first_provisioning_artifact_id, message, s3, stage_template, staged_template_url, ACCOUNT_ID,
    DEFAULT_REGION,
};

/// The `ProductType` of a CloudFormation-backed product, which is the only kind whose
/// launch stands a stack up (`smithy_query query="shape" shape="ProductType"`).
const CFN_PRODUCT: &str = "CLOUD_FORMATION_TEMPLATE";

/// `ProvisionedProductStatus`'s value for a provisioned product whose stack is not running
/// (`smithy_query query="shape" shape="ProvisionedProductStatus"`: AVAILABLE, UNDER_CHANGE,
/// TAINTED, ERROR, PLAN_IN_PROGRESS).
const ERROR: &str = "ERROR";

/// `ProvisionedProductStatus`'s stable value for one whose last operation succeeded.
const AVAILABLE: &str = "AVAILABLE";

/// `RecordStatus`'s terminal value for a request operation that did not complete
/// (`smithy_query query="shape" shape="RecordStatus"`: CREATED, IN_PROGRESS,
/// IN_PROGRESS_IN_ERROR, SUCCEEDED, FAILED).
const FAILED: &str = "FAILED";

/// `RecordStatus`'s terminal value for one that did.
const SUCCEEDED: &str = "SUCCEEDED";

/// `RecordStatus`'s non-terminal first value: "the request was created but the operation has
/// not started".
const CREATED: &str = "CREATED";

/// The class of the exception a refused launch is recorded under, which is also the
/// `RecordError$Code` the failure is paired with.
const CLOUDFORMATION_EXCEPTION: &str = "AmazonCloudFormationException";

/// AWS's sentence for a provisioned product no provisioning of which has succeeded yet, so
/// it has no outputs to report. All fixed text.
const NO_SUCCESSFUL_RECORD: &str = "Last Successful Provisioning Record doesn't exist.";

/// How long a poll keeps looking for a condition the emulator's clock owes before it gives
/// up. Far longer than any span this emulator settles on, so a loaded or slow host cannot
/// fail a case a fast one passes: the loop waits for the CONDITION, never for a fixed time.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Drive one CloudFormation request through the same router, as a client's own SDK sends it:
/// the query protocol that service speaks.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    pairs: &[(&str, &str)],
) -> (u16, String) {
    let mut form = ::form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", action);
    form.append_pair("Version", "2010-05-15");
    for (key, value) in pairs {
        form.append_pair(key, value);
    }
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://cloudformation.{DEFAULT_REGION}.amazonaws.com/"
        ))
        .header(
            "host",
            format!("cloudformation.{DEFAULT_REGION}.amazonaws.com"),
        )
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTHARNESS/20260101/{DEFAULT_REGION}/\
                 cloudformation/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(::bytes::Bytes::from(form.finish().into_bytes()))
        .expect("a well-formed CloudFormation request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let read = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, read)
}

/// One row of the refused-deployment table.
struct Refused {
    case: &'static str,
    /// The provisioned product's name, which also names the staged template object, the
    /// product it is created from and the bucket its one resource would stand up.
    name: &'static str,
    /// The template's `Parameters` declarations — the input contract the values in force are
    /// held to.
    declared: Value,
    /// The provisioning parameters the launch request carries.
    supplied: &'static [(&'static str, &'static str)],
    /// The sentence CloudFormation words for the constraint this row breaks, which travels
    /// verbatim into the launch's failure.
    reason: &'static str,
}

/// What one launch left behind: the provisioned product it minted and the record of the
/// request that minted it.
struct Launched {
    provisioned_product_id: String,
    record_id: String,
}

/// A template declaring `declared` as its `Parameters`, one bucket as its resource and no
/// outputs — the shape every template this suite stages takes, so the rows differ only in
/// the input contract they declare.
fn template_with_parameters(bucket: &str, declared: &Value) -> String {
    let mut document = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "A staged product template",
        "Resources": {
            "Data": {
                "Type": "AWS::S3::Bucket",
                "Properties": {"BucketName": bucket}
            }
        }
    });
    if !declared.is_null() {
        document["Parameters"] = declared.clone();
    }
    document.to_string()
}

/// Stage `template`, create the product and the portfolio that gives it a launch path, and
/// provision it with `supplied` — the whole administrator-then-end-user path a launch takes.
///
/// Asserts what the request is ACCEPTED as, which is the same for every row whatever the
/// deployment then does: the record answers in the non-terminal status with no errors at
/// all. The launch itself is settled by the clock, so a caller polls for it afterwards.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    template: &str,
    supplied: &[(&str, &str)],
) -> Launched {
    let key = format!("{name}.template.json");
    stage_template(router, &key, template).await;
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
    associate_product_with_portfolio(router, &product, &portfolio).await;

    let mut request = json!({
        "ProductId": product,
        "ProvisioningArtifactId": version,
        "ProvisionedProductName": name,
        "ProvisionToken": format!("{name}-provision"),
    });
    if !supplied.is_empty() {
        request["ProvisioningParameters"] = Value::Array(
            supplied
                .iter()
                .map(|(key, value)| json!({"Key": key, "Value": value}))
                .collect(),
        );
    }
    let (status, provisioned) = call(router, "ProvisionProduct", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the launch to be accepted, got {status} with {provisioned}"
    );
    let detail = &provisioned["RecordDetail"];
    assert_eq!(
        detail["Status"].as_str(),
        Some(CREATED),
        "case {case:?}: expected the request to be accepted as {CREATED}, got {provisioned}"
    );
    assert_eq!(
        detail["RecordErrors"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected an accepted request to carry no RecordErrors, got {provisioned}"
    );

    Launched {
        provisioned_product_id: detail["ProvisionedProductId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ProvisionedProductId in {provisioned}"))
            .to_string(),
        record_id: detail["RecordId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no RecordId in {provisioned}"))
            .to_string(),
    }
}

/// Look at the provisioned product `name` until it reports `wanted`, or the deadline passes,
/// and answer the detail it reported then.
async fn settles_to(router: &MultiServiceRouter, case: &str, name: &str, wanted: &str) -> Value {
    let started = std::time::Instant::now();
    loop {
        let (status, body) = call(router, "DescribeProvisionedProduct", &json!({"Name": name})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected provisioned product {name} to be described, got {status} \
             with {body}"
        );
        let detail = body["ProvisionedProductDetail"].clone();
        if detail["Status"].as_str() == Some(wanted) {
            return detail;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: expected provisioned product {name} to reach {wanted}, last reported \
             {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether `message` is the refusal `reason` as the launch's own call renders it: the
/// exception class, that service's sentence verbatim, and the clause naming the call it came
/// from — whose request id belongs to that call and so is read out rather than expected.
fn states_refusal(reason: &str, message: &str) -> bool {
    let head = format!(
        "{CLOUDFORMATION_EXCEPTION}  {reason} (Service: AmazonCloudFormation; Status Code: 400; \
         Error Code: ValidationError; Request ID: "
    );
    message
        .strip_prefix(&head)
        .and_then(|rest| rest.strip_suffix("; Proxy: null)"))
        .is_some_and(|request_id| !request_id.is_empty())
}

/// A launch whose deployment CloudFormation refuses settles as a FAILURE: the record reaches
/// `FAILED` carrying that refusal, the provisioned product reports `ERROR` with the same
/// sentence, nothing stands behind it, and it has no successful provisioning to report
/// outputs for.
#[tokio::test]
async fn a_launch_cloudformation_refuses_settles_as_a_failure() {
    // Rows differ along WHICH part of the template's input contract the values in force
    // break. Each `reason` is CloudFormation's own sentence for that constraint.
    let rows = [
        Refused {
            case: "a value the admitted set does not cover",
            name: "launch-outside-allowed-values",
            declared: json!({"Env": {"Type": "String", "AllowedValues": ["dev", "prod"], "Default": "dev"}}),
            supplied: &[("Env", "staging")],
            reason: "Parameter 'Env' must be one of AllowedValues",
        },
        Refused {
            case: "a value longer than the declared maximum",
            name: "launch-over-max-length",
            declared: json!({"Label": {"Type": "String", "MaxLength": 4, "Default": "abc"}}),
            supplied: &[("Label", "abcdefgh")],
            reason: "Parameter 'Label' must have length not greater than 4",
        },
        Refused {
            case: "a value that is no value of the declared type",
            name: "launch-not-a-number",
            declared: json!({"Size": {"Type": "Number", "Default": "2"}}),
            supplied: &[("Size", "large")],
            reason: "Parameter 'Size' must be a number.",
        },
        Refused {
            case: "a number above the declared maximum",
            name: "launch-over-max-value",
            declared: json!({"Size": {"Type": "Number", "MaxValue": 10, "Default": "2"}}),
            supplied: &[("Size", "11")],
            reason: "Parameter 'Size' must be a number not greater than 10",
        },
        Refused {
            case: "a value the declared pattern does not match",
            name: "launch-off-pattern",
            declared: json!({"Owner": {"Type": "String", "AllowedPattern": "[a-z]+", "Default": "acme"}}),
            supplied: &[("Owner", "ACME-7")],
            reason: "Parameter 'Owner' must match pattern [a-z]+",
        },
        Refused {
            case: "a key the template declares nothing for",
            name: "launch-undeclared-key",
            declared: json!({"Env": {"Type": "String", "Default": "dev"}}),
            supplied: &[("Region", "moon-1")],
            reason: "Parameters: [Region] do not exist in the template",
        },
        Refused {
            case: "a declared parameter nothing puts a value in force for",
            name: "launch-unsatisfied-parameter",
            declared: json!({"Env": {"Type": "String"}}),
            supplied: &[],
            reason: "Parameters: [Env] must have values",
        },
    ];

    let router = emulator_with_template_services();
    create_template_bucket(&router).await;

    // Every launch is started FIRST, so the span each one's settling is owed over is the
    // same span for all of them rather than one per row in turn.
    let mut launched = Vec::new();
    for row in &rows {
        let template = template_with_parameters(row.name, &row.declared);
        launched.push(launch(&router, row.case, row.name, &template, row.supplied).await);
    }

    for (row, started) in rows.iter().zip(&launched) {
        let case = row.case;
        let detail = settles_to(&router, case, row.name, ERROR).await;

        // The status message is what the provisioned product reports about its resources:
        // CloudFormation's own sentence, inside the clause naming the call it came from.
        let status_message = detail["StatusMessage"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no StatusMessage in {detail}"));
        assert!(
            states_refusal(row.reason, status_message),
            "case {case:?}: expected the StatusMessage to state {:?} as the launch's call \
             rendered it, got {status_message:?}",
            row.reason
        );
        // "The record identifier of the last successful request performed on this
        // provisioned product": nothing has succeeded, so there is none.
        assert!(
            detail["LastSuccessfulProvisioningRecordId"].is_null(),
            "case {case:?}: expected no last successful provisioning record, got {detail}"
        );
        assert_eq!(
            detail["LastRecordId"].as_str(),
            Some(started.record_id.as_str()),
            "case {case:?}: expected the launch's own record to be the last one, got {detail}"
        );

        // The record of the request says the SAME thing, and says it as the error pair the
        // model declares (`RecordError{Code,Description}`).
        let (status, described) =
            call(&router, "DescribeRecord", &json!({"Id": started.record_id})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the record {} to be described, got {status} with {described}",
            started.record_id
        );
        let record = &described["RecordDetail"];
        assert_eq!(
            record["Status"].as_str(),
            Some(FAILED),
            "case {case:?}: expected the record to settle {FAILED}, got {described}"
        );
        let errors = record["RecordErrors"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no RecordErrors list in {described}"));
        assert_eq!(
            errors.len(),
            1,
            "case {case:?}: expected the one error the launch failed with, got {described}"
        );
        assert_eq!(
            errors[0]["Code"].as_str(),
            Some(CLOUDFORMATION_EXCEPTION),
            "case {case:?}: expected the caught exception's class as the error code, got {described}"
        );
        assert_eq!(
            errors[0]["Description"].as_str(),
            Some(status_message),
            "case {case:?}: expected the error description to be the sentence the provisioned \
             product reports, got {described}"
        );
        assert_eq!(
            described["RecordOutputs"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: expected a failed launch to have produced nothing, got {described}"
        );

        // Nothing stands behind the provisioned product: CloudFormation holds no stack under
        // the name a launch of it would have stood up.
        let stack = format!("SC-{ACCOUNT_ID}-{}", started.provisioned_product_id);
        let (status, held) = cloudformation(&router, "DescribeStacks", &[("StackName", &stack)]).await;
        assert_ne!(
            status, 200,
            "case {case:?}: expected CloudFormation to hold no stack {stack}, got {held}"
        );

        // With no successful provisioning there is no provisioning whose outputs could be
        // reported, so the question is refused rather than answered with an empty list.
        let (status, refused) = call(
            &router,
            "GetProvisionedProductOutputs",
            &json!({"ProvisionedProductId": started.provisioned_product_id}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the outputs question to be refused, got {status} with \
             {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidParametersException",
            "case {case:?}: expected the modelled parameter error, got {refused}"
        );
        assert_eq!(
            message(&refused),
            NO_SUCCESSFUL_RECORD,
            "case {case:?}: expected AWS's sentence, got {refused}"
        );
    }
}

/// The control: a launch whose deployment CloudFormation ACCEPTS settles the other way —
/// `AVAILABLE` with no status message, a `SUCCEEDED` record carrying no errors, and the
/// outputs of the stack it stood up.
#[tokio::test]
async fn a_launch_cloudformation_accepts_settles_as_a_success() {
    let case = "a value the admitted set covers";
    let name = "launch-inside-allowed-values";
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;

    // The same declaration the refused rows are held to, with a value it admits: the one
    // difference between a settled failure and a settled success is what the deployment did.
    let mut document: Value = serde_json::from_str(&template_with_parameters(
        name,
        &json!({"Env": {"Type": "String", "AllowedValues": ["dev", "prod"], "Default": "dev"}}),
    ))
    .expect("the staged template is JSON");
    document["Outputs"] = json!({"EnvEcho": {"Value": {"Ref": "Env"}}});
    let started = launch(
        &router,
        case,
        name,
        &document.to_string(),
        &[("Env", "prod")],
    )
    .await;

    let detail = settles_to(&router, case, name, AVAILABLE).await;
    assert!(
        detail["StatusMessage"].is_null(),
        "case {case:?}: expected a settled success to report no status message, got {detail}"
    );
    assert_eq!(
        detail["LastSuccessfulProvisioningRecordId"].as_str(),
        Some(started.record_id.as_str()),
        "case {case:?}: expected the launch's record to be the last successful one, got {detail}"
    );

    let (status, described) =
        call(&router, "DescribeRecord", &json!({"Id": started.record_id})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the record to be described, got {status} with {described}"
    );
    assert_eq!(
        described["RecordDetail"]["Status"].as_str(),
        Some(SUCCEEDED),
        "case {case:?}: expected the record to settle {SUCCEEDED}, got {described}"
    );
    assert_eq!(
        described["RecordDetail"]["RecordErrors"].as_array().map(Vec::len),
        Some(0),
        "case {case:?}: expected a settled success to carry no errors, got {described}"
    );

    // What the launch produced, read back through both operations that report it: the value
    // the request supplied, echoed by the output the template declares for it.
    assert_eq!(
        echoed_value(&described["RecordOutputs"]),
        Some("prod".to_string()),
        "case {case:?}: expected the record to report what the launch produced, got {described}"
    );
    let (status, outputs) = call(
        &router,
        "GetProvisionedProductOutputs",
        &json!({"ProvisionedProductId": started.provisioned_product_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the outputs to be reported, got {status} with {outputs}"
    );
    assert_eq!(
        echoed_value(&outputs["Outputs"]),
        Some("prod".to_string()),
        "case {case:?}: expected the supplied value to be echoed, got {outputs}"
    );

    // The resource the stack declared really exists in the service that owns it, so the
    // launch really deployed rather than reporting that it had.
    let (status, held) = s3(&router, "HEAD", &format!("/{name}"), b"").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the stack's bucket to exist, got {status} with {held}"
    );

    // `PageSize`/`PageToken` page over what the record reports, without dropping or
    // repeating one: the record carries the template's own output and the stack beside it.
    let whole: Vec<Value> = described["RecordOutputs"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no RecordOutputs list in {described}"))
        .clone();
    assert!(
        whole.len() > 1,
        "case {case:?}: expected more outputs than one page of this size, got {described}"
    );
    let mut paged: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({"Id": started.record_id, "PageSize": 1});
        if let Some(token) = &token {
            request["PageToken"] = json!(token);
        }
        let (status, page) = call(&router, "DescribeRecord", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a page of the record's outputs, got {status} with {page}"
        );
        let outputs = page["RecordOutputs"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no RecordOutputs list in {page}"));
        assert_eq!(
            outputs.len(),
            1,
            "case {case:?}: expected the page size asked for, got {page}"
        );
        paged.extend(outputs.iter().cloned());
        token = page["NextPageToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            paged.len() <= whole.len(),
            "case {case:?}: paging reported more outputs than the record holds: {paged:?}"
        );
    }
    assert_eq!(
        paged, whole,
        "case {case:?}: expected paging to report every output once and in one order, got \
         {paged:?} against {whole:?}"
    );

    // A value that is no page token of this service's addresses no position in that list,
    // so the read is refused rather than answered with the first page again.
    let (status, refused) = call(
        &router,
        "DescribeRecord",
        &json!({"Id": started.record_id, "PageToken": "not-a-page-token"}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a foreign page token to be refused, got {status} with {refused}"
    );
}

/// The value of the `EnvEcho` output in a reported `RecordOutputs`/`Outputs` list — the
/// output the control's template declares for the parameter the request supplied.
fn echoed_value(outputs: &Value) -> Option<String> {
    outputs
        .as_array()?
        .iter()
        .find(|output| output["OutputKey"].as_str() == Some("EnvEcho"))
        .and_then(|output| output["OutputValue"].as_str().map(str::to_string))
}

/// A refusal quotes the template's own declarations, so its length is the SUBMITTED
/// template's: what one failed launch keeps of it is held to this emulator's own bound
/// rather than to the body a caller staged.
#[tokio::test]
async fn what_a_failed_launch_keeps_of_a_refusal_is_bounded() {
    // The emulator's own bound on a stored failure's sentence
    // (`state::MAX_FAILURE_DESCRIPTION`). A declaration whose pattern alone is longer than
    // that makes CloudFormation quote back more than the bound admits.
    const BOUND: usize =
        service_com_amazonaws_servicecatalog_aws242servicecatalogservice::state::MAX_FAILURE_DESCRIPTION;

    let case = "a refusal longer than the bound";
    let name = "launch-with-a-long-refusal";
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;

    // `ConstraintDescription` is "a string that explains a constraint when the constraint is
    // violated", which CloudFormation reports in place of the constraint's own clause — so a
    // template can make the sentence it quotes back as long as its own body allows.
    let explanation = "x".repeat(BOUND * 2);
    let template = template_with_parameters(
        name,
        &json!({"Owner": {
            "Type": "String",
            "AllowedValues": ["acme"],
            "ConstraintDescription": explanation,
            "Default": "acme",
        }}),
    );
    let started = launch(&router, case, name, &template, &[("Owner", "other")]).await;

    let detail = settles_to(&router, case, name, ERROR).await;
    let status_message = detail["StatusMessage"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no StatusMessage in {detail}"));
    assert_eq!(
        status_message.chars().count(),
        BOUND,
        "case {case:?}: expected the stored sentence to be held to {BOUND} characters, got {}",
        status_message.chars().count()
    );
    // What is kept is the START of the refusal: the class, then CloudFormation's own
    // sentence as far as the bound admits.
    assert!(
        status_message.starts_with(&format!(
            "{CLOUDFORMATION_EXCEPTION}  Parameter Owner failed to satisfy constraint: "
        )),
        "case {case:?}: expected the refusal's opening to be kept, got {status_message:?}"
    );

    // The record reports the same bounded sentence, so neither read stores or returns more
    // than the other.
    let (status, described) =
        call(&router, "DescribeRecord", &json!({"Id": started.record_id})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the record to be described, got {status} with {described}"
    );
    assert_eq!(
        described["RecordDetail"]["RecordErrors"][0]["Description"].as_str(),
        Some(status_message),
        "case {case:?}: expected the record to carry the same bounded sentence, got {described}"
    );
}

/// A record identifier nothing stored carries addresses no record, which is the one failure
/// `DescribeRecord` models.
#[tokio::test]
async fn a_record_identifier_that_resolves_to_nothing_is_refused() {
    let router = emulator_with_template_services();
    // Rows differ along the FORM of the identifier: one of the shape this service mints and
    // one of another shape entirely. Neither names a record, and both are refused the same
    // way.
    for id in ["rec-abcdefghijkl", "not-a-record"] {
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
}
