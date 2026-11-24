// SPDX-License-Identifier: CC0-1.0

//! `AWS::Lambda::Function` as Cloud Control API resolves it: in the AWS Lambda service that
//! OWNS the function, whichever API created it.
//!
//! Cloud Control API is the uniform read path over a resource type's own provider, so a
//! function AWS Lambda holds is one `GetResource` reports — and a `ResourceNotFoundException`
//! is for an identifier that service holds no function under, never for one it does. The
//! model a read reports is the type's READABLE properties: every member the schema declares
//! for the deployment package is one a caller SETS, so the package is reported EMPTY, and a
//! function whose code has been replaced reads back exactly as it did before.
//!
//! The suites below drive both sides the way a client does: AWS Lambda's own REST surface
//! (`CreateFunction`, `GetFunction`, `UpdateFunctionCode`, `DeleteFunction`) for the owning
//! service, and this service's own wire for the Cloud Control calls.

use std::io::Write;

use serde_json::{json, Value};

use crate::emulator::{code, descriptions, message, progress, properties_of, text, Emulator};

/// The type under test.
const TYPE_NAME: &str = "AWS::Lambda::Function";

/// The role a function created here runs as. AWS Lambda serves a create naming a role no IAM
/// record is held for — an execution role is normally provisioned outside the client that
/// creates the function — so nothing has to be seeded for these cases.
const ROLE: &str = "arn:aws:iam::000000000000:role/a-function-role";

/// The property that carries the type's primary identifier.
const FUNCTION_NAME: &str = "FunctionName";

/// The property that carries the deployment package, whose every member the type's schema
/// declares write-only — so a reported model carries the property with no member at all.
const CODE: &str = "Code";

/// The type's read-only property: the ARN AWS Lambda assigns the function.
const ARN: &str = "Arn";

/// One deployment package: the stated source as the only file of a .zip archive, under
/// `file`, base64-encoded as a JSON request carries a blob member.
///
/// Built with the maintained implementation of the format, entirely in memory.
fn package(file: &str, source: &str) -> String {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options: zip::write::SimpleFileOptions =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    archive
        .start_file(file, options)
        .expect("the archive takes an entry");
    archive
        .write_all(source.as_bytes())
        .expect("the entry takes the source");
    let bytes = archive
        .finish()
        .expect("the archive finishes")
        .into_inner();
    aws_smithy_types::base64::encode(&bytes)
}

/// `CreateFunction` on AWS Lambda's own API, required to succeed.
async fn create_function(emulator: &Emulator, request: &Value) -> Value {
    emulator
        .lambda_ok("POST", "/2015-03-31/functions", Some(request))
        .await
}

/// `GetFunction` on AWS Lambda's own API, as a `(status, body)` pair.
async fn get_function(emulator: &Emulator, name: &str) -> (u16, Value) {
    emulator
        .lambda("GET", &format!("/2015-03-31/functions/{name}"), None)
        .await
}

/// The `FunctionArn` AWS Lambda's own read publishes for `name`.
async fn published_arn(emulator: &Emulator, name: &str) -> String {
    let (status, body) = get_function(emulator, name).await;
    assert_eq!(
        status, 200,
        "function {name:?}: expected AWS Lambda's own read to report it, got {status} with \
         body {body:?}"
    );
    text(&body["Configuration"], "FunctionArn")
}

/// How long a wait for AWS Lambda to report a function out of the window it provisions one
/// in runs before the case fails, and how often that wait re-reads.
///
/// `CreateFunction` answers the function `Pending`, and in that window that service refuses
/// every modification of it — a span the clock owes, so a case that modifies the function
/// next waits for the STATE it needs and never for a span of its own.
const ACTIVE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
const ACTIVE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Wait until AWS Lambda reports `name` `Active`, which is when it takes modifications of
/// the function again.
async fn await_active(emulator: &Emulator, name: &str) {
    let started = std::time::Instant::now();
    loop {
        let (status, body) = get_function(emulator, name).await;
        let state = text(&body["Configuration"], "State");
        if status == 200 && state == "Active" {
            return;
        }
        assert!(
            started.elapsed() < ACTIVE_BUDGET,
            "function {name:?}: expected AWS Lambda to report it Active within \
             {ACTIVE_BUDGET:?}, it is still {state:?} (HTTP {status}, body {body:?})"
        );
        tokio::time::sleep(ACTIVE_INTERVAL).await;
    }
}

/// The `ResourceDescription` `GetResource` reports for `identifier`, as
/// `(identifier, properties)`.
async fn described(emulator: &Emulator, identifier: &str) -> (String, Value) {
    let (status, body) = emulator.get(TYPE_NAME, identifier).await;
    assert_eq!(
        status, 200,
        "GetResource {identifier:?}: expected HTTP 200, got {status} with body {body:?}"
    );
    let description = body["ResourceDescription"].clone();
    (text(&description, "Identifier"), properties_of(&description))
}

/// The model `GetResource` reports must carry every property `expected` names, with the
/// value it names, and must carry the deployment package with none of the members the schema
/// declares write-only — which, for this type, is all of them.
fn assert_reports(case: &str, properties: &Value, expected: &Value) {
    for (property, value) in expected.as_object().expect("a model of properties") {
        assert_eq!(
            properties.get(property),
            Some(value),
            "case {case:?}: expected the read to report {property} as {value:?}, got {:?}",
            properties.get(property)
        );
    }
    assert_eq!(
        properties.get(CODE),
        Some(&json!({})),
        "case {case:?}: the write-only members of {CODE} are part of no model this service \
         reports, leaving the property empty, got {:?}",
        properties.get(CODE)
    );
}

/// A function AWS Lambda holds reads back through Cloud Control API with its readable
/// properties — and goes on doing so once the write-only members of its `Code` have been
/// replaced, which changes nothing a read of the function reports.
///
/// The rows are functions that differ along what the rule generalises over: the runtime and
/// the handler the package is loaded through, which properties the create states at all, and
/// the forms those properties take (text, numbers, a map of variables, a tag set).
#[tokio::test]
async fn a_function_aws_lambda_holds_reads_back_without_its_write_only_code() {
    let emulator = Emulator::with_lambda();
    let rows = [
        (
            "a python function stating only the members a create requires",
            json!({
                FUNCTION_NAME: "plain-python-fn",
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                CODE: { "ZipFile": package("index.py", "def handler(event, context):\n    return {}\n") },
            }),
            json!({
                FUNCTION_NAME: "plain-python-fn",
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
            }),
            ("index.py", "def handler(event, context):\n    return {'v': 2}\n"),
        ),
        (
            "a node function carrying a description, a timeout and a memory size",
            json!({
                FUNCTION_NAME: "configured-node-fn",
                "Role": ROLE,
                "Runtime": "nodejs20.x",
                "Handler": "app.run",
                "Description": "the one a client reads back",
                "Timeout": 45,
                "MemorySize": 512,
                CODE: { "ZipFile": package("app.js", "exports.run = async () => ({});\n") },
            }),
            json!({
                FUNCTION_NAME: "configured-node-fn",
                "Role": ROLE,
                "Runtime": "nodejs20.x",
                "Handler": "app.run",
                "Description": "the one a client reads back",
                "Timeout": 45,
                "MemorySize": 512,
            }),
            ("app.js", "exports.run = async () => ({ v: 2 });\n"),
        ),
        (
            "a ruby function carrying an environment and a tag set",
            json!({
                FUNCTION_NAME: "tagged-ruby-fn",
                "Role": ROLE,
                "Runtime": "ruby3.3",
                "Handler": "index.handler",
                "Environment": { "Variables": { "STAGE": "beta", "REGION_LABEL": "home" } },
                "Tags": { "owner": "platform" },
                CODE: { "ZipFile": package("index.rb", "def handler(event:, context:)\n  {}\nend\n") },
            }),
            json!({
                FUNCTION_NAME: "tagged-ruby-fn",
                "Role": ROLE,
                "Runtime": "ruby3.3",
                "Handler": "index.handler",
                "Environment": { "Variables": { "STAGE": "beta", "REGION_LABEL": "home" } },
                "Tags": [{ "Key": "owner", "Value": "platform" }],
            }),
            ("index.rb", "def handler(event:, context:)\n  { v: 2 }\nend\n"),
        ),
    ];
    for (case, create, expected, replacement) in rows {
        let name = text(&create, FUNCTION_NAME);
        create_function(&emulator, &create).await;
        let arn = published_arn(&emulator, &name).await;

        let (identifier, properties) = described(&emulator, &name).await;
        assert_eq!(
            identifier, name,
            "case {case:?}: expected the resource to be reported under its own identifier"
        );
        assert_reports(case, &properties, &expected);
        assert_eq!(
            properties.get(ARN).and_then(Value::as_str),
            Some(arn.as_str()),
            "case {case:?}: expected the read to report the ARN AWS Lambda published, got {:?}",
            properties.get(ARN)
        );

        // The write-only update this suite is about: the function's code, replaced through
        // the owning service's own call — which it takes once it reports the function out of
        // the window it provisions one in.
        let (file, source) = replacement;
        await_active(&emulator, &name).await;
        emulator
            .lambda_ok(
                "PUT",
                &format!("/2015-03-31/functions/{name}/code"),
                Some(&json!({ "ZipFile": package(file, source) })),
            )
            .await;

        let (after_identifier, after) = described(&emulator, &name).await;
        assert_eq!(
            after_identifier, name,
            "case {case:?}: expected the function to keep reading back under its own \
             identifier after its code was replaced"
        );
        assert_eq!(
            after, properties,
            "case {case:?}: a replaced deployment package changes nothing a read of the \
             function reports, got {after:?}"
        );
    }
}

/// A function may be addressed by its primary identifier as a string or as the schema's
/// identifier map, which names the same function either way.
#[tokio::test]
async fn a_function_is_addressed_by_its_identifier_in_either_form() {
    let emulator = Emulator::with_lambda();
    let name = "either-form-fn";
    create_function(
        &emulator,
        &json!({
            FUNCTION_NAME: name,
            "Role": ROLE,
            "Runtime": "python3.13",
            "Handler": "index.handler",
            CODE: { "ZipFile": package("index.py", "def handler(event, context):\n    return {}\n") },
        }),
    )
    .await;
    let references = [
        ("the identifier as a string", name.to_string()),
        (
            "the identifier as the schema's identifier map",
            json!({ FUNCTION_NAME: name }).to_string(),
        ),
    ];
    for (case, reference) in references {
        let (identifier, properties) = described(&emulator, &reference).await;
        assert_eq!(
            identifier, name,
            "case {case:?}: expected the same function to be resolved, got {properties:?}"
        );
    }
}

/// A `ResourceNotFoundException` is for an identifier AWS Lambda holds no function under:
/// before the function is created, and again once that service has removed it — and never
/// for the function in between.
#[tokio::test]
async fn only_an_identifier_aws_lambda_holds_no_function_under_is_not_found() {
    let emulator = Emulator::with_lambda();
    let name = "a-function-of-its-own-lifecycle";
    let create = json!({
        FUNCTION_NAME: name,
        "Role": ROLE,
        "Runtime": "python3.12",
        "Handler": "index.handler",
        CODE: { "ZipFile": package("index.py", "def handler(event, context):\n    return {}\n") },
    });

    for case in ["before anything created it", "after it was removed"] {
        let (status, body) = emulator.get(TYPE_NAME, name).await;
        // RECORDED behaviour: the model annotates the not-found error shape `@httpError 404`,
        // and this awsJson1_0 service answers it with 400.
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert!(
            code(&body).contains("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            code(&body)
        );
        let reported = message(&body);
        assert!(
            reported.contains(&format!(
                "Resource of type '{TYPE_NAME}' with identifier '{name}' was not found."
            )) && reported.contains("HandlerErrorCode: NotFound"),
            "case {case:?}: unexpected wording {reported:?}"
        );

        if case == "before anything created it" {
            create_function(&emulator, &create).await;
            let (identifier, _) = described(&emulator, name).await;
            assert_eq!(
                identifier, name,
                "expected the function AWS Lambda now holds to read back"
            );
            emulator
                .lambda_ok("DELETE", &format!("/2015-03-31/functions/{name}"), None)
                .await;
        }
    }
}

/// A listing of the type reports the functions AWS Lambda holds, under the identifiers that
/// service answers to — including the ones Cloud Control API never created.
#[tokio::test]
async fn a_listing_reports_the_functions_aws_lambda_holds() {
    let emulator = Emulator::with_lambda();
    for name in ["listed-one-fn", "listed-two-fn"] {
        create_function(
            &emulator,
            &json!({
                FUNCTION_NAME: name,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                CODE: { "ZipFile": package("index.py", "def handler(event, context):\n    return {}\n") },
            }),
        )
        .await;
    }
    let listed = emulator.list(TYPE_NAME, None, None).await;
    let mut identifiers: Vec<String> = descriptions(&listed)
        .iter()
        .map(|description| text(description, "Identifier"))
        .collect();
    identifiers.sort();
    assert_eq!(
        identifiers,
        vec!["listed-one-fn".to_string(), "listed-two-fn".to_string()],
        "expected the listing to report both functions, got {listed:?}"
    );
    for description in descriptions(&listed) {
        let properties = properties_of(&description);
        assert!(
            properties.get(CODE) == Some(&json!({})) && properties.get(ARN).is_some(),
            "expected a listed function to carry its readable properties and an empty {CODE}, \
             got {properties:?}"
        );
    }
}

/// A function CREATED through Cloud Control API is one AWS Lambda itself holds: that
/// service's own read reports it, a patch of the write-only `Code` replaces the package it
/// holds, a patch of a configuration property changes it there, and a delete removes it from
/// that service.
#[tokio::test]
async fn a_function_created_through_cloud_control_is_one_aws_lambda_holds() {
    let emulator = Emulator::with_lambda();
    let name = "round-tripped-fn";
    let desired = json!({
        FUNCTION_NAME: name,
        "Role": ROLE,
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Description": "created through the uniform control plane",
        "Timeout": 15,
        CODE: { "ZipFile": "def handler(event, context):\n    return {'v': 1}\n" },
    });
    let identifier = emulator.provision(TYPE_NAME, &desired).await;
    assert_eq!(
        identifier, name,
        "expected the resource to answer to the name the desired state gave it"
    );

    // The owning service's own read is what says the function really exists.
    let (status, held) = get_function(&emulator, name).await;
    assert_eq!(
        status, 200,
        "expected AWS Lambda to hold the function the create reported, got {status} with body \
         {held:?}"
    );
    let configuration = held["Configuration"].clone();
    assert_eq!(
        text(&configuration, "Runtime"),
        "python3.12",
        "expected the function AWS Lambda holds to carry the runtime the model stated, got \
         {configuration:?}"
    );
    assert_eq!(
        configuration["Timeout"].as_i64(),
        Some(15),
        "expected the function AWS Lambda holds to carry the timeout the model stated, got \
         {configuration:?}"
    );
    let deployed = text(&configuration, "CodeSha256");

    // ...and the Cloud Control read reports it without the package it was created from.
    let (_, properties) = described(&emulator, name).await;
    assert_reports(
        "the created function",
        &properties,
        &json!({ FUNCTION_NAME: name, "Timeout": 15 }),
    );

    // A write-only property can only be `add`ed, and adding one replaces the package AWS
    // Lambda holds: the code is what changes, and the model a read reports does not.
    emulator
        .update_and_settle(
            TYPE_NAME,
            name,
            &json!([{
                "op": "add",
                "path": "/Code",
                "value": { "ZipFile": "def handler(event, context):\n    return {'v': 2}\n" },
            }]),
        )
        .await;
    let (_, after_code) = get_function(&emulator, name).await;
    assert_ne!(
        text(&after_code["Configuration"], "CodeSha256"),
        deployed,
        "expected the replaced package to be the one AWS Lambda now holds, got {after_code:?}"
    );
    let (_, after_properties) = described(&emulator, name).await;
    assert_reports(
        "the function whose code was replaced",
        &after_properties,
        &json!({ FUNCTION_NAME: name, "Timeout": 15 }),
    );

    // A configuration property is changed in that service too.
    emulator
        .update_and_settle(
            TYPE_NAME,
            name,
            &json!([{ "op": "replace", "path": "/Timeout", "value": 25 }]),
        )
        .await;
    let (_, reconfigured) = get_function(&emulator, name).await;
    assert_eq!(
        reconfigured["Configuration"]["Timeout"].as_i64(),
        Some(25),
        "expected the patched timeout to be the one AWS Lambda holds, got {reconfigured:?}"
    );

    // And a delete hands the function back to that service, which then holds none.
    emulator.delete_and_settle(TYPE_NAME, name).await;
    let (status, gone) = get_function(&emulator, name).await;
    assert_eq!(
        status, 404,
        "expected AWS Lambda to hold no function once the delete settled, got {status} with \
         body {gone:?}"
    );
    let (status, refused) = emulator.get(TYPE_NAME, name).await;
    assert_eq!(
        status, 400,
        "expected the deleted function to read as not found, got {status} with body {refused:?}"
    );
}

/// A write-only member of the type's model may only be `add`ed: it is one no read of the
/// resource reports, so there is nothing at that pointer for another operation to update, and
/// the refusal names it as the schema's own pointer.
///
/// The rows differ along the operation that names it, which member is named, how many are
/// named at once, and whether the pointer names a member of a structured property or a whole
/// property.
#[tokio::test]
async fn a_write_only_member_of_the_model_is_refused_every_operation_but_add() {
    let emulator = Emulator::with_lambda();
    let name = "patched-package-fn";
    create_function(
        &emulator,
        &json!({
            FUNCTION_NAME: name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            CODE: { "ZipFile": package("index.py", "def handler(event, context):\n    return {}\n") },
        }),
    )
    .await;
    // Catalog source: the `writeOnlyProperties` of the `AWS::Lambda::Function` resource
    // provider schema, which point at the members of the deployment package rather than at
    // the property that holds them, and at the SnapStart setting a request states.
    let rows = [
        (
            "replacing the inline source",
            json!([{ "op": "replace", "path": "/Code/ZipFile", "value": "later" }]),
            "/Code/ZipFile",
        ),
        (
            "removing the key of the Amazon S3 object holding the package",
            json!([{ "op": "remove", "path": "/Code/S3Key" }]),
            "/Code/S3Key",
        ),
        (
            "naming two members at once",
            json!([
                { "op": "replace", "path": "/Code/S3Bucket", "value": "a-bucket" },
                { "op": "remove", "path": "/Code/ZipFile" },
            ]),
            "/Code/S3Bucket, /Code/ZipFile",
        ),
        (
            "replacing a whole write-only property",
            json!([{ "op": "replace", "path": "/SnapStart", "value": { "ApplyOn": "None" } }]),
            "/SnapStart",
        ),
    ];
    for (case, patch, pointers) in rows {
        let (status, body) = emulator.update(TYPE_NAME, name, &patch, None).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert!(
            code(&body).contains("ValidationException"),
            "case {case:?}: expected ValidationException, got {:?}",
            code(&body)
        );
        assert_eq!(
            message(&body),
            format!(
                "Invalid patch update: writeOnlyProperties [{pointers}] can only be updated \
                 using 'add' operation"
            ),
            "case {case:?}: unexpected wording, got {:?}",
            message(&body)
        );
    }

    // `add` is the one operation that sets such a member, and the model a later read reports
    // still carries none of them.
    let settled = emulator
        .update_and_settle(
            TYPE_NAME,
            name,
            &json!([{
                "op": "add",
                "path": "/Code/ZipFile",
                "value": "def handler(event, context):\n    return {'v': 2}\n",
            }]),
        )
        .await;
    assert_eq!(
        text(&settled, "OperationStatus"),
        "SUCCESS",
        "an add of a write-only member must be taken, got {settled:?}"
    );
    let (_, properties) = described(&emulator, name).await;
    assert_reports("the patched function", &properties, &json!({ FUNCTION_NAME: name }));
}

/// A create whose desired state asks for a function AWS Lambda would not make is the OWNING
#[tokio::test]
async fn a_create_aws_lambda_refuses_fails_the_request_with_its_own_verdict() {
    let emulator = Emulator::with_lambda();
    let accepted = emulator
        .create_ok(
            TYPE_NAME,
            &json!({
                FUNCTION_NAME: "a-function-of-a-runtime-nothing-publishes",
                "Role": ROLE,
                "Runtime": "cobol1.0",
                "Handler": "index.handler",
                CODE: { "ZipFile": "nothing runs this\n" },
            }),
            None,
        )
        .await;
    let terminal = emulator
        .terminal(&text(&accepted, "RequestToken"))
        .await;
    assert_eq!(
        text(&terminal, "OperationStatus"),
        "FAILED",
        "expected the request to fail on the owning service's verdict, got {terminal:?}"
    );
    assert!(
        !text(&terminal, "StatusMessage").is_empty(),
        "expected the failed request to carry that service's own diagnostic, got {terminal:?}"
    );
    let (status, refused) = emulator
        .get(TYPE_NAME, "a-function-of-a-runtime-nothing-publishes")
        .await;
    assert_eq!(
        status, 400,
        "expected a refused create to leave no resource behind, got {status} with body \
         {refused:?}"
    );
    assert!(
        progress(&refused).is_null(),
        "a read mints no resource operation, got {refused:?}"
    );
}
