// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the LIFECYCLE of a function whose deployment package is a
//! CONTAINER IMAGE: the create, the provisioning the clock owes it, the configuration a
//! read reports, and the delete.
//!
//! RULE: "If the deployment package is a container image, then you set the package type to
//! Image. For a container image, the code property must include the URI of a container
//! image in the Amazon ECR registry. You do not need to specify the handler and runtime
//! properties" (`CreateFunction`) — so a create whose `Code.ImageUri` resolves to an image
//! that Region's registry HOLDS is served (201), whichever way the reference addresses that
//! image and whether or not the request names a handler or a runtime. The function it
//! creates is an ordinary function from that instant on: it provisions ("Lambda provisions
//! an instance of the function and its supporting resources … During this time, you can't
//! invoke or modify the function", so `State` leaves `Pending` only once the clock owes the
//! move), `GetFunctionConfiguration` answers 200 with the stored configuration of a package
//! that carries its own runtime — `PackageType` `Image`, no `Runtime` and no `Handler`, a
//! `CodeSize` of 0 because Lambda stores no archive for it, the image's own digest as
//! `CodeSha256` — together with the members every function reports (the version's ARN,
//! `Version` `$LATEST`, the configuration members the request named or their documented
//! defaults, `SnapStart`, `LoggingConfig`), and `DeleteFunction` answers 204, after which
//! the same read is the modelled 404. A reference that resolves to no image creates
//! nothing, so the read and the delete of the function it would have been are 404 as well.
//!
//! The rows differ along what the rule generalises over: how the reference addresses the
//! image (a tag, no tag at all, the digest the registry minted, a namespaced repository) and
//! which configuration members the create names beside the package (none, the description
//! and the two sizes, the architecture and the ephemeral storage, a log group of its own).
//!
//! Every image is pushed through Amazon ECR's OWN `CreateRepository`/`PutImage` on the SAME
//! engine (`crate::function_lifecycle::seed_image`), so the digest asserted here is the one
//! the registry minted rather than one this suite computed. The sentence a reference that
//! resolves to nothing is refused with, and the resolution table behind it, are
//! `container_image_package_source.rs`; what this suite adds is the whole lifecycle around
//! a reference that DOES resolve. The clock-owed activation is brought forward instead of
//! waited out, so nothing here sleeps.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::function_lifecycle::{
    create_function, function_arn, json_call, make_activation_due, seed_image, Answer, REGION,
    ROLE,
};

/// The account id every emulator ARN and registry URI carries
/// (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The error code a create that names no usable source image is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The error code a read or a delete of a function that does not exist is refused with.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The prefix of the log group a function whose `LoggingConfig` names none logs to: "By
/// default, Lambda functions send logs to a default log group named `/aws/lambda/<function
/// name>`" (`smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`).
const DEFAULT_LOG_GROUP_PREFIX: &str = "/aws/lambda/";

/// How a row's `Code.ImageUri` addresses the image it pushed.
#[derive(Debug, Clone, Copy)]
enum Reference {
    /// `<repository>:<tag>`, the tag the image was pushed under.
    Tag,
    /// The repository alone, which names the registry's default tag.
    DefaultTag,
    /// `<repository>@<digest>`, the manifest digest the registry minted.
    Digest,
}

/// What the configuration of a row's function must report: the members the create named,
/// or the documented default of each member it did not.
struct Expected {
    description: &'static str,
    timeout: i64,
    memory_size: i64,
    ephemeral_storage: i64,
    architectures: &'static [&'static str],
    /// The group the row's `LoggingConfig` names, or `None` to take the default one.
    log_group: Option<&'static str>,
}

/// One row: the image pushed, how the create references it, what else the create names, and
/// the configuration that leaves the function with.
struct Case {
    case: &'static str,
    /// The repository the image is pushed to, which may itself be namespaced.
    repository: &'static str,
    /// The tag it is pushed under.
    tag: &'static str,
    reference: Reference,
    /// The members the create carries beside `FunctionName`, `Role`, `PackageType` and
    /// `Code`.
    named: Value,
    expected: Expected,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "a pushed tag, with nothing but the package named",
            repository: "image-lifecycle-held",
            tag: "v1",
            reference: Reference::Tag,
            named: json!({}),
            expected: Expected {
                description: "",
                timeout: 3,
                memory_size: 128,
                ephemeral_storage: 512,
                architectures: &["x86_64"],
                log_group: None,
            },
        },
        Case {
            case: "no tag at all, which names the registry's default tag, with a \
                   description and both sizes",
            repository: "image-lifecycle-defaulted",
            tag: "latest",
            reference: Reference::DefaultTag,
            named: json!({ "Description": "the container probe", "MemorySize": 256, "Timeout": 10 }),
            expected: Expected {
                description: "the container probe",
                timeout: 10,
                memory_size: 256,
                ephemeral_storage: 512,
                architectures: &["x86_64"],
                log_group: None,
            },
        },
        Case {
            case: "the digest the registry minted, on the other architecture and with \
                   ephemeral storage of its own",
            repository: "image-lifecycle-pinned",
            tag: "v3",
            reference: Reference::Digest,
            named: json!({ "Architectures": ["arm64"], "EphemeralStorage": { "Size": 1024 } }),
            expected: Expected {
                description: "",
                timeout: 3,
                memory_size: 128,
                ephemeral_storage: 1024,
                architectures: &["arm64"],
                log_group: None,
            },
        },
        Case {
            case: "a namespaced repository's tag, logging to a group of its own",
            repository: "image-lifecycle/team-app",
            tag: "v1",
            reference: Reference::Tag,
            named: json!({
                "LoggingConfig": { "LogFormat": "Text", "LogGroup": "/aws/lambda/image-group" },
            }),
            expected: Expected {
                description: "",
                timeout: 3,
                memory_size: 128,
                ephemeral_storage: 512,
                architectures: &["x86_64"],
                log_group: Some("/aws/lambda/image-group"),
            },
        },
    ]
}

/// The registry URI of `repository` in the Region this harness signs for, which is the
/// Region a function's package is resolved in.
fn repository_uri(repository: &str) -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{repository}")
}

/// A `CreateFunction` body for a container-image package: the members a container image
/// needs and nothing else — no `Handler` and no `Runtime`, which the model says such a
/// package does not carry.
fn image_create(name: &str, image_uri: &str, named: &Value) -> Value {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "PackageType": "Image",
        "Code": { "ImageUri": image_uri },
    });
    if let Some(members) = named.as_object() {
        for (member, value) in members {
            body[member.as_str()] = value.clone();
        }
    }
    body
}

/// `GetFunctionConfiguration` (`GET /2015-03-31/functions/{FunctionName}/configuration`).
async fn get_configuration(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Answer {
    let uri = format!("/2015-03-31/functions/{name}/configuration");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// `DeleteFunction` (`DELETE /2015-03-31/functions/{FunctionName}`).
async fn delete_function(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Answer {
    let uri = format!("/2015-03-31/functions/{name}");
    json_call(router, "DELETE", &uri, &Value::Null).await
}

/// Assert one member of a body, naming the case and the value seen.
fn assert_member(case: &str, body: &Value, pointer: &str, expected: &Value) {
    let actual = body.pointer(pointer).cloned().unwrap_or(Value::Null);
    assert_eq!(
        &actual, expected,
        "case {case:?}: expected {pointer} {expected}, got {actual}"
    );
}

/// The hexadecimal half of a `sha256:<hex>` manifest digest, which is what an image
/// package's `CodeSha256` reports: the image IS the deployment package, so its digest is
/// the digest of the code.
fn digest_hexadecimal(case: &str, digest: &str) -> String {
    digest
        .split_once(':')
        .map(|(_algorithm, hexadecimal)| hexadecimal.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: expected a sha256:<hex> digest, got {digest:?}"))
}

/// A container-image create whose reference resolves is served, provisions, reports the
/// configuration of an image package, and is deletable.
#[tokio::test]
async fn an_image_packaged_function_is_created_read_and_deleted() {
    for (index, case) in cases().into_iter().enumerate() {
        // One engine per row, so each row's activation is brought forward on its own and
        // nothing another row created can answer for it.
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = crate::function_lifecycle::router_on(&engine);
        let label = case.case;
        let name = format!("image-lifecycle-{index}");

        let pushed = format!("{}:{}", repository_uri(case.repository), case.tag);
        let digest = seed_image(&engine, &pushed).await;
        let hexadecimal = digest_hexadecimal(label, &digest);
        let image_uri = match case.reference {
            Reference::Tag => pushed.clone(),
            Reference::DefaultTag => repository_uri(case.repository),
            Reference::Digest => format!("{}@{digest}", repository_uri(case.repository)),
        };

        let created = create_function(&router, &image_create(&name, &image_uri, &case.named)).await;
        assert_eq!(
            created.status,
            201,
            "case {label:?}: expected 201 from CreateFunction, got {} {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        let answered = created.json();
        // The package the create stored, answered straight back: an image package carries
        // its own runtime, so neither member a .zip archive needs is reported.
        assert_member(label, &answered, "/PackageType", &json!("Image"));
        assert_member(label, &answered, "/Runtime", &Value::Null);
        assert_member(label, &answered, "/Handler", &Value::Null);
        assert_member(label, &answered, "/CodeSize", &json!(0));
        assert_member(label, &answered, "/CodeSha256", &json!(hexadecimal));
        assert_member(label, &answered, "/Version", &json!("$LATEST"));
        assert_member(label, &answered, "/FunctionName", &json!(name));
        assert_member(label, &answered, "/FunctionArn", &json!(function_arn(&name)));
        assert_member(label, &answered, "/Role", &json!(ROLE));
        // "During this time, you can't invoke or modify the function": the function is
        // still provisioning when the create answers.
        assert_member(label, &answered, "/State", &json!("Pending"));

        // The read of a function that is still provisioning is the function, in that state.
        let provisioning = get_configuration(&router, &name).await;
        assert_eq!(
            provisioning.status,
            200,
            "case {label:?}: expected 200 from GetFunctionConfiguration while provisioning, \
             got {} {}",
            provisioning.status,
            String::from_utf8_lossy(&provisioning.body)
        );
        assert_member(label, &provisioning.json(), "/State", &json!("Pending"));

        // Once the move the clock owes is due, the same read reports the settled function.
        make_activation_due(&engine);
        let read = get_configuration(&router, &name).await;
        assert_eq!(
            read.status,
            200,
            "case {label:?}: expected 200 from GetFunctionConfiguration, got {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        let body = read.json();
        let expected = &case.expected;
        let log_group = expected
            .log_group
            .map_or_else(|| format!("{DEFAULT_LOG_GROUP_PREFIX}{name}"), str::to_string);
        for (pointer, value) in [
            ("/FunctionName", json!(name)),
            ("/FunctionArn", json!(function_arn(&name))),
            ("/Role", json!(ROLE)),
            ("/Version", json!("$LATEST")),
            ("/PackageType", json!("Image")),
            ("/Runtime", Value::Null),
            ("/Handler", Value::Null),
            ("/CodeSize", json!(0)),
            ("/CodeSha256", json!(hexadecimal)),
            ("/State", json!("Active")),
            ("/LastUpdateStatus", json!("Successful")),
            ("/StateReason", Value::Null),
            ("/StateReasonCode", Value::Null),
            ("/Description", json!(expected.description)),
            ("/Timeout", json!(expected.timeout)),
            ("/MemorySize", json!(expected.memory_size)),
            ("/EphemeralStorage/Size", json!(expected.ephemeral_storage)),
            ("/Architectures", json!(expected.architectures)),
            ("/TracingConfig/Mode", json!("PassThrough")),
            ("/SnapStart/ApplyOn", json!("None")),
            ("/SnapStart/OptimizationStatus", json!("Off")),
            ("/LoggingConfig/LogFormat", json!("Text")),
            ("/LoggingConfig/LogGroup", json!(log_group)),
        ] {
            assert_member(label, &body, pointer, &value);
        }
        // Stored per configuration rather than invented: both are reported, and a second
        // read of the same function reports the same values.
        for pointer in ["/RevisionId", "/LastModified"] {
            let value = body.pointer(pointer).and_then(Value::as_str);
            assert!(
                value.is_some_and(|value| !value.is_empty()),
                "case {label:?}: expected a stored {pointer}, got {:?}",
                body.pointer(pointer)
            );
        }
        let again = get_configuration(&router, &name).await;
        assert_eq!(
            (again.status, again.json()),
            (read.status, body.clone()),
            "case {label:?}: reading the configuration changed what a later caller sees"
        );

        // And the function is deletable, after which the same read finds nothing.
        let deleted = delete_function(&router, &name).await;
        assert_eq!(
            deleted.status,
            204,
            "case {label:?}: expected 204 from DeleteFunction, got {} {}",
            deleted.status,
            String::from_utf8_lossy(&deleted.body)
        );
        let gone = get_configuration(&router, &name).await;
        assert_eq!(
            gone.status,
            404,
            "case {label:?}: expected 404 for the deleted function, got {} {}",
            gone.status,
            String::from_utf8_lossy(&gone.body)
        );
        assert_eq!(
            gone.error_code(),
            NOT_FOUND,
            "case {label:?}: expected {NOT_FOUND} for the deleted function, got {}",
            gone.error_code()
        );
    }
}

/// A reference that resolves to no image of this Region's registry creates nothing, so the
/// READ and the DELETE of the function it would have been both find nothing.
///
/// The refusal itself — the status, the error code and the sentence it is worded with over
/// the whole resolution table — is `container_image_package_source.rs`; what is asserted
/// here is the other half of the cascade the create's refusal leaves behind, over one row of
/// each refusal class (a reference into this registry that names nothing pushed, and a
/// reference that is no image URI of this Region's registry at all).
#[tokio::test]
async fn a_reference_that_resolves_to_nothing_creates_no_function() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = crate::function_lifecycle::router_on(&engine);
    // One image this registry really holds, so a row differs from a reference that resolves
    // only in the part it names.
    let held = format!("{}:v1", repository_uri("image-refusal-held"));
    seed_image(&engine, &held).await;

    // (case, the reference the create names)
    let rows: Vec<(&str, String)> = vec![
        (
            "a tag the registry holds nothing for",
            format!("{}:absent", repository_uri("image-refusal-held")),
        ),
        (
            "another Region's registry",
            format!("{ACCOUNT}.dkr.ecr.eu-west-1.amazonaws.com/image-refusal-held:v1"),
        ),
    ];

    for (index, (case, image_uri)) in rows.iter().enumerate() {
        let name = format!("image-refusal-{index}");
        let refused = create_function(&router, &image_create(&name, image_uri, &json!({}))).await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected 400 from CreateFunction, got {} {}",
            refused.status,
            String::from_utf8_lossy(&refused.body)
        );
        assert_eq!(
            refused.error_code(),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {}",
            refused.error_code()
        );

        for (operation, answer) in [
            ("GetFunctionConfiguration", get_configuration(&router, &name).await),
            ("DeleteFunction", delete_function(&router, &name).await),
        ] {
            assert_eq!(
                answer.status,
                404,
                "case {case:?}: expected 404 from {operation} for the function the refused \
                 create did not make, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_code(),
                NOT_FOUND,
                "case {case:?}: expected {NOT_FOUND} from {operation}, got {}",
                answer.error_code()
            );
        }
    }
}
