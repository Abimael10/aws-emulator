// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the IDENTITY and the runtime metadata a function read reports,
//! and for which members a LISTING of function configurations carries.
//!
//! The RULES asserted here, each over a table of inputs:
//!
//! 1. **the runtime version is named by its whole digest.** `RuntimeVersionConfig`
//!    reports "the ARN of the runtime version you want the function to use"
//!    (`smithy_query(query="shape", shape="RuntimeVersionConfig")`), and the first form
//!    `RuntimeVersionArn`'s pattern admits names the runtime BUILD in its resource
//!    position: `arn:aws:lambda:<region>::runtime:<digest>`, where the digest is the full
//!    SHA-256 of that build — 64 lowercase hexadecimal digits, the width a client that
//!    pins or compares a runtime version reads. Every surface that reports a function
//!    configuration reports the same digest for the same runtime, and a different one for
//!    a different runtime.
//! 2. **`FunctionArn` names the identity the request addressed.** The member targets
//!    `NameSpacedFunctionArn`, whose pattern admits a trailing qualifier
//!    (`…:function:<name>(:($LATEST(.PUBLISHED)?|<alias-or-version>))?`), so a read
//!    reports the qualified ARN exactly when the request qualified the identity it asked
//!    about — in the `FunctionName` or in the `Qualifier` parameter, `$LATEST` named
//!    explicitly included — and the function's own unqualified ARN when it named none.
//! 3. **a listing reports the configuration SUBSET.** "The ListFunctions operation returns
//!    a subset of the FunctionConfiguration fields. To get the additional fields (State,
//!    StateReasonCode, StateReason, LastUpdateStatus, LastUpdateStatusReason,
//!    LastUpdateStatusReasonCode, RuntimeVersionConfig) for a function or version, use
//!    GetFunction" — so an entry of the `FunctionList` both `ListFunctions` and
//!    `ListVersionsByFunction` return carries none of those members, while the read of one
//!    identity reports them.
//! 4. **a member the package type does not use is refused by name.** A container image
//!    brings its own runtime and entry point ("You do not need to specify the handler and
//!    runtime properties") and a .zip archive has no image to override, so such a member is
//!    refused `400 InvalidParameterValueException` / `The <Member> parameter is not
//!    supported for functions created with <container images|deployment packages>.`,
//!    whichever write named it.
//!
//! The rows differ along what those rules generalise over: the runtime identifier, the
//! surface the configuration is read from, how the request addresses the identity (each
//! documented `FunctionName` form, each place a qualifier can be named, and a version, an
//! alias and the unpublished version as the qualifier), how many versions a function has,
//! and which member a write names against which package type.
//!
//! Every assertion is over real HTTP requests through the router, and the clock-driven half
//! is brought forward rather than waited out, so nothing here sleeps. The harness (the
//! `Answer` wrapper, `json_call`, the activation window and the image seeding) is the one
//! `function_lifecycle.rs` publishes.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, function_arn, json_call, make_activation_due, make_update_due, package_bytes,
    router_on, seed_image, zip_file, Answer, ACCOUNT, REGION, ROLE,
};

/// Where `CreateFunction` and the function reads are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The unpublished version's label, from the model's `Version` pattern
/// (`smithy_query(query="shape", shape="Version")`: `^(\$LATEST|[0-9]+)$`).
const VERSION_LATEST: &str = "$LATEST";

/// How many hexadecimal digits a runtime-version digest carries: the whole SHA-256, which
/// is what AWS reports in the resource position of a runtime-version ARN
/// (`arn:aws:lambda:<region>::runtime:<64 hex digits>`).
const RUNTIME_DIGEST_DIGITS: usize = 64;

/// The error code every refusal in this suite carries.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// Runtime identifiers from the model's own catalog
/// (`smithy_query(query="shape", shape="Runtime")`), spanning its families — the managed
/// runtimes of four languages, the OS-only runtime, and one entry past its deprecation —
/// so the rule is asserted on identifiers of different shapes and lengths rather than on
/// one.
const RUNTIMES: [&str; 6] = [
    "python3.13",
    "nodejs22.x",
    "java21",
    "ruby3.4",
    "provided.al2023",
    "python3.9",
];

/// The `message` of a wire error, under either casing the JSON envelopes use.
fn message(answer: &Answer) -> Option<String> {
    answer.string("message").or_else(|| answer.string("Message"))
}

/// The ARN of `version` of the function `name`.
fn version_arn(name: &str, version: &str) -> String {
    format!("{}:{version}", function_arn(name))
}

/// `GetFunction`, with the `Qualifier` parameter when one is given.
async fn get_function(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
) -> Answer {
    let uri = match qualifier {
        Some(qualifier) => format!("{FUNCTIONS_URI}/{reference}?Qualifier={qualifier}"),
        None => format!("{FUNCTIONS_URI}/{reference}"),
    };
    json_call(router, "GET", &uri, &Value::Null).await
}

/// `GetFunctionConfiguration`, with the `Qualifier` parameter when one is given.
async fn get_function_configuration(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
) -> Answer {
    let uri = match qualifier {
        Some(qualifier) => {
            format!("{FUNCTIONS_URI}/{reference}/configuration?Qualifier={qualifier}")
        }
        None => format!("{FUNCTIONS_URI}/{reference}/configuration"),
    };
    json_call(router, "GET", &uri, &Value::Null).await
}

/// `UpdateFunctionConfiguration` with the given request body.
async fn update_function_configuration(
    router: &MultiServiceRouter,
    reference: &str,
    body: &Value,
) -> Answer {
    let uri = format!("{FUNCTIONS_URI}/{reference}/configuration");
    json_call(router, "PUT", &uri, body).await
}

/// `PublishVersion`, carrying a description so each call publishes a configuration the
/// function has not published yet.
async fn publish_version(router: &MultiServiceRouter, reference: &str, description: &str) -> Answer {
    let uri = format!("{FUNCTIONS_URI}/{reference}/versions");
    json_call(router, "POST", &uri, &json!({ "Description": description })).await
}

/// `CreateAlias` pointing `alias` at `version`.
async fn create_alias(
    router: &MultiServiceRouter,
    reference: &str,
    alias: &str,
    version: &str,
) -> Answer {
    let uri = format!("{FUNCTIONS_URI}/{reference}/aliases");
    json_call(
        router,
        "POST",
        &uri,
        &json!({ "Name": alias, "FunctionVersion": version }),
    )
    .await
}

/// `ListVersionsByFunction`.
async fn list_versions(router: &MultiServiceRouter, reference: &str) -> Answer {
    let uri = format!("{FUNCTIONS_URI}/{reference}/versions");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// A `CreateFunction` body for a .zip function on `runtime`.
fn zip_create(name: &str, runtime: &str, bytes: usize) -> Value {
    json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": runtime,
        "Handler": "index.handler",
        "Code": { "ZipFile": zip_file(&package_bytes(bytes)) },
    })
}

/// A `CreateFunction` body for a container-image function.
fn image_create(name: &str, image_uri: &str) -> Value {
    json!({
        "FunctionName": name,
        "Role": ROLE,
        "PackageType": "Image",
        "Code": { "ImageUri": image_uri },
    })
}

/// Assert `answer` carries `status`, naming the case.
fn assert_status(case: &str, answer: &Answer, status: u16) {
    assert_eq!(
        answer.status,
        status,
        "case {case:?}: expected HTTP {status}, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// The runtime-version digest `arn` carries, asserted to be the ARN form AWS reports:
/// `arn:aws:lambda:<region>::runtime:<64 lowercase hexadecimal digits>`.
fn runtime_version_digest(case: &str, surface: &str, arn: &Value) -> String {
    let reported = arn.as_str().unwrap_or_default().to_string();
    let prefix = format!("arn:aws:lambda:{REGION}::runtime:");
    let digest = reported.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!(
            "case {case:?}: {surface} must report a runtime-version ARN under {prefix:?}, got \
             {reported:?}"
        )
    });
    assert_eq!(
        digest.len(),
        RUNTIME_DIGEST_DIGITS,
        "case {case:?}: {surface} must name the runtime build by its whole SHA-256 \
         ({RUNTIME_DIGEST_DIGITS} digits), got {digest:?}"
    );
    assert!(
        digest
            .chars()
            .all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_uppercase()),
        "case {case:?}: {surface} must name the digest in lowercase hexadecimal, got {digest:?}"
    );
    digest.to_string()
}

/// RULE 1: every surface that reports a function configuration names the runtime version
/// by the whole SHA-256 of the runtime build, and two runtimes never share one.
#[tokio::test]
async fn every_surface_reports_the_runtime_version_by_its_whole_digest() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let mut digests: BTreeMap<String, String> = BTreeMap::new();

    for (index, runtime) in RUNTIMES.iter().enumerate() {
        let case = (*runtime).to_string();
        let name = format!("runtime-version-{index}");

        let created = create_function(&router, &zip_create(&name, runtime, 32 + index)).await;
        assert_status(&case, &created, 201);
        make_activation_due(&engine);

        let read = get_function(&router, &name, None).await;
        assert_status(&case, &read, 200);
        let configuration = get_function_configuration(&router, &name, None).await;
        assert_status(&case, &configuration, 200);
        // The mutation answers the configuration it left behind, which a client may read
        // instead of re-reading the function.
        let updated = update_function_configuration(
            &router,
            &name,
            &json!({ "Description": "read back from the mutation" }),
        )
        .await;
        assert_status(&case, &updated, 200);
        make_update_due(&engine);
        let published = publish_version(&router, &name, "a snapshot").await;
        assert_status(&case, &published, 201);
        // And the same read, once more, reports the same runtime version: the digest is
        // derived from the runtime rather than drawn per call.
        let again = get_function(&router, &name, None).await;
        assert_status(&case, &again, 200);

        let member = "RuntimeVersionConfig";
        let surfaces = [
            ("CreateFunction", created.json()[member]["RuntimeVersionArn"].clone()),
            (
                "GetFunction",
                read.json()["Configuration"][member]["RuntimeVersionArn"].clone(),
            ),
            (
                "GetFunctionConfiguration",
                configuration.json()[member]["RuntimeVersionArn"].clone(),
            ),
            (
                "UpdateFunctionConfiguration",
                updated.json()[member]["RuntimeVersionArn"].clone(),
            ),
            (
                "PublishVersion",
                published.json()[member]["RuntimeVersionArn"].clone(),
            ),
            (
                "GetFunction again",
                again.json()["Configuration"][member]["RuntimeVersionArn"].clone(),
            ),
        ];
        let mut reported: Option<String> = None;
        for (surface, arn) in &surfaces {
            let digest = runtime_version_digest(&case, surface, arn);
            match &reported {
                Some(first) => assert_eq!(
                    &digest, first,
                    "case {case:?}: {surface} must report the runtime version the other \
                     surfaces report ({first:?}), got {digest:?}"
                ),
                None => reported = Some(digest),
            }
        }
        let digest = reported.unwrap_or_default();
        if let Some((other, shared)) = digests
            .iter()
            .find(|(_, held)| **held == digest)
            .map(|(other, held)| (other.clone(), held.clone()))
        {
            panic!(
                "case {case:?}: two runtimes must not share a runtime version, but {other:?} \
                 also reports {shared:?}"
            );
        }
        digests.insert(case, digest);
    }
}

/// One row of RULE 2: how the request addresses the identity, and the qualifier the
/// reported ARN must therefore carry.
struct AddressCase {
    label: &'static str,
    /// The `FunctionName` the request carries, built from the function's name.
    reference: fn(&str) -> String,
    /// The `Qualifier` parameter, when the row names one there.
    qualifier: Option<&'static str>,
    /// The qualifier the reported ARN must carry; `None` is the unqualified ARN.
    expected: Option<&'static str>,
}

/// RULE 2: a read reports the ARN of the identity it was addressed by — on `GetFunction`
/// and on `GetFunctionConfiguration`, which answer the same `FunctionConfiguration`.
#[tokio::test]
async fn a_read_reports_the_arn_of_the_identity_it_addressed() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "addressed";
    let created = create_function(&router, &zip_create(name, "python3.13", 48)).await;
    assert_status("setup", &created, 201);
    make_activation_due(&engine);
    let published = publish_version(&router, name, "the pinned cut").await;
    assert_status("setup", &published, 201);
    make_update_due(&engine);
    let alias = create_alias(&router, name, "live", "1").await;
    assert_status("setup", &alias, 201);

    let cases = [
        AddressCase {
            label: "the bare name",
            reference: |name| name.to_string(),
            qualifier: None,
            expected: None,
        },
        AddressCase {
            label: "the bare function: prefix",
            reference: |name| format!("function:{name}"),
            qualifier: None,
            expected: None,
        },
        AddressCase {
            label: "a partial ARN",
            reference: |name| format!("{ACCOUNT}:function:{name}"),
            qualifier: None,
            expected: None,
        },
        AddressCase {
            label: "the full ARN",
            reference: function_arn,
            qualifier: None,
            expected: None,
        },
        AddressCase {
            label: "the unpublished version named in the name",
            reference: |name| format!("{name}:{VERSION_LATEST}"),
            qualifier: None,
            expected: Some(VERSION_LATEST),
        },
        AddressCase {
            label: "the unpublished version named in the Qualifier parameter",
            reference: |name| name.to_string(),
            qualifier: Some(VERSION_LATEST),
            expected: Some(VERSION_LATEST),
        },
        AddressCase {
            label: "the unpublished version named on the full ARN",
            reference: |name| format!("{}:{VERSION_LATEST}", function_arn(name)),
            qualifier: None,
            expected: Some(VERSION_LATEST),
        },
        AddressCase {
            label: "a published version named in the name",
            reference: |name| format!("{name}:1"),
            qualifier: None,
            expected: Some("1"),
        },
        AddressCase {
            label: "a published version named in the Qualifier parameter",
            reference: |name| name.to_string(),
            qualifier: Some("1"),
            expected: Some("1"),
        },
        AddressCase {
            label: "an alias named in the name",
            reference: |name| format!("{name}:live"),
            qualifier: None,
            expected: Some("live"),
        },
        AddressCase {
            label: "an alias named in the Qualifier parameter",
            reference: |name| name.to_string(),
            qualifier: Some("live"),
            expected: Some("live"),
        },
    ];

    for case in &cases {
        let reference = (case.reference)(name);
        let expected = match case.expected {
            Some(qualifier) => version_arn(name, qualifier),
            None => function_arn(name),
        };

        let read = get_function(&router, &reference, case.qualifier).await;
        assert_status(case.label, &read, 200);
        assert_eq!(
            read.json()["Configuration"]["FunctionArn"],
            json!(expected),
            "case {:?}: expected GetFunction to report {expected:?}, got {:?}",
            case.label,
            read.json()["Configuration"]["FunctionArn"]
        );

        let configuration = get_function_configuration(&router, &reference, case.qualifier).await;
        assert_status(case.label, &configuration, 200);
        assert_eq!(
            configuration.json()["FunctionArn"],
            json!(expected),
            "case {:?}: expected GetFunctionConfiguration to report {expected:?}, got {:?}",
            case.label,
            configuration.json()["FunctionArn"]
        );
    }
}

/// The members a listing entry must NOT carry, which the model names as the ones to read
/// "for a function or version" with `GetFunction`.
const LIVE_MEMBERS: [&str; 7] = [
    "State",
    "StateReason",
    "StateReasonCode",
    "LastUpdateStatus",
    "LastUpdateStatusReason",
    "LastUpdateStatusReasonCode",
    "RuntimeVersionConfig",
];

/// RULE 3: every entry of the `FunctionList` a listing answers carries the configuration
/// subset — no live member — while the read of one identity reports them; and each entry
/// names its own version, with its own description.
#[tokio::test]
async fn a_function_listing_reports_the_configuration_subset() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // Two functions, one with published versions and one with none, so the rows differ in
    // how many versions a listing walks.
    let descriptions = ["the first cut", "the second cut"];
    for (index, name) in ["listed-versions", "listed-plain"].iter().enumerate() {
        let created = create_function(&router, &zip_create(name, "nodejs22.x", 64 + index)).await;
        assert_status(name, &created, 201);
        make_activation_due(&engine);
        if *name == "listed-versions" {
            for description in descriptions {
                let published = publish_version(&router, name, description).await;
                assert_status(name, &published, 201);
                make_update_due(&engine);
            }
        }
    }

    for (name, expected_labels) in [
        ("listed-versions", vec![VERSION_LATEST, "1", "2"]),
        ("listed-plain", vec![VERSION_LATEST]),
    ] {
        let listed = list_versions(&router, name).await;
        assert_status(name, &listed, 200);
        let entries = listed.json()["Versions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let labels: Vec<String> = entries
            .iter()
            .map(|entry| entry["Version"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            labels, expected_labels,
            "case {name:?}: expected the unpublished version then every published one, got \
             {labels:?}"
        );

        for entry in &entries {
            let label = entry["Version"].as_str().unwrap_or_default().to_string();
            let case = format!("{name} version {label}");
            for member in LIVE_MEMBERS {
                assert!(
                    entry[member].is_null(),
                    "case {case:?}: a listing entry reports the configuration subset, so it \
                     carries no {member}, got {:?}",
                    entry[member]
                );
            }
            // Every entry of this listing names a VERSION, so every entry's ARN is
            // qualified — the unpublished one included.
            assert_eq!(
                entry["FunctionArn"],
                json!(version_arn(name, &label)),
                "case {case:?}: expected the version's own ARN, got {:?}",
                entry["FunctionArn"]
            );
            // A published version reports the description it was published with, and the
            // unpublished one the function's own (empty here).
            let expected_description = label
                .parse::<usize>()
                .ok()
                .and_then(|number| descriptions.get(number - 1).copied())
                .unwrap_or_default();
            assert_eq!(
                entry["Description"],
                json!(expected_description),
                "case {case:?}: expected the version's own Description \
                 {expected_description:?}, got {:?}",
                entry["Description"]
            );

            // The same identity, read one at a time, DOES report the live members.
            let read = get_function_configuration(&router, name, Some(&label)).await;
            assert_status(&case, &read, 200);
            for member in ["State", "LastUpdateStatus", "RuntimeVersionConfig"] {
                assert!(
                    !read.json()[member].is_null(),
                    "case {case:?}: GetFunctionConfiguration reports {member}, got {:?}",
                    read.json()[member]
                );
            }
        }
    }

    // `ListFunctions` answers entries of that same `FunctionList`, with
    // `FunctionVersion=ALL` listing every published version beside each function's
    // unpublished one, so the rule holds there too.
    let functions = json_call(
        &router,
        "GET",
        &format!("{FUNCTIONS_URI}?FunctionVersion=ALL"),
        &Value::Null,
    )
    .await;
    assert_status("ListFunctions", &functions, 200);
    let listed = functions.json()["Functions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        listed.len() >= 4,
        "ListFunctions with FunctionVersion=ALL must report both functions and their \
         versions, got {listed:?}"
    );
    for entry in &listed {
        let case = format!(
            "ListFunctions {} {}",
            entry["FunctionName"], entry["Version"]
        );
        for member in LIVE_MEMBERS {
            assert!(
                entry[member].is_null(),
                "case {case:?}: a listing entry reports the configuration subset, so it \
                 carries no {member}, got {:?}",
                entry[member]
            );
        }
    }
}

/// RULE 4: a member the function's package type does not use is refused by naming that
/// member and the packaging it does not belong to, on each write that accepts it.
#[tokio::test]
async fn a_member_the_package_type_does_not_use_is_refused_by_name() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let image_uri = format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/read-identity:latest");
    seed_image(&engine, &image_uri).await;

    // A .zip function and a container-image function, so both directions of the mismatch
    // are driven on the update as well as on the create.
    let zipped = create_function(&router, &zip_create("zip-packaged", "python3.13", 32)).await;
    assert_status("setup", &zipped, 201);
    let imaged = create_function(&router, &image_create("image-packaged", &image_uri)).await;
    assert_status("setup", &imaged, 201);
    make_activation_due(&engine);

    let image_config = json!({ "Command": ["app.handler"] });
    // (case, the create body or the (function, update body), the sentence it is refused
    // with) — one row per member each packaging has no use for, on each write.
    let creates: Vec<(&str, Value, &str)> = vec![
        (
            "CreateFunction naming Runtime on a container image",
            json!({
                "FunctionName": "image-runtime",
                "Role": ROLE,
                "PackageType": "Image",
                "Runtime": "python3.13",
                "Code": { "ImageUri": image_uri.clone() },
            }),
            "The Runtime parameter is not supported for functions created with container \
             images.",
        ),
        (
            "CreateFunction naming Handler on a container image",
            json!({
                "FunctionName": "image-handler",
                "Role": ROLE,
                "PackageType": "Image",
                "Handler": "index.handler",
                "Code": { "ImageUri": image_uri.clone() },
            }),
            "The Handler parameter is not supported for functions created with container \
             images.",
        ),
        (
            "CreateFunction naming ImageConfig on a deployment package",
            json!({
                "FunctionName": "zip-image-config",
                "Role": ROLE,
                "Runtime": "python3.13",
                "Handler": "index.handler",
                "ImageConfig": image_config.clone(),
                "Code": { "ZipFile": zip_file(&package_bytes(32)) },
            }),
            "The ImageConfig parameter is not supported for functions created with deployment \
             packages.",
        ),
    ];
    for (case, request, sentence) in &creates {
        let answer = create_function(&router, request).await;
        assert_status(case, &answer, 400);
        assert_eq!(
            answer.error_code(),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {}",
            answer.error_code()
        );
        assert_eq!(
            message(&answer).as_deref(),
            Some(*sentence),
            "case {case:?}: expected {sentence:?}, got {:?}",
            message(&answer)
        );
        // A refused create persists nothing.
        let name = request["FunctionName"].as_str().unwrap_or_default();
        let read = get_function(&router, name, None).await;
        assert_status(case, &read, 404);
    }

    let updates: Vec<(&str, &str, Value, &str)> = vec![
        (
            "UpdateFunctionConfiguration naming Runtime on a container-image function",
            "image-packaged",
            json!({ "Runtime": "python3.13" }),
            "The Runtime parameter is not supported for functions created with container \
             images.",
        ),
        (
            "UpdateFunctionConfiguration naming Handler on a container-image function",
            "image-packaged",
            json!({ "Handler": "index.handler" }),
            "The Handler parameter is not supported for functions created with container \
             images.",
        ),
        (
            "UpdateFunctionConfiguration naming ImageConfig on a .zip function",
            "zip-packaged",
            json!({ "ImageConfig": image_config }),
            "The ImageConfig parameter is not supported for functions created with deployment \
             packages.",
        ),
    ];
    for (case, name, request, sentence) in &updates {
        let answer = update_function_configuration(&router, name, request).await;
        assert_status(case, &answer, 400);
        assert_eq!(
            answer.error_code(),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {}",
            answer.error_code()
        );
        assert_eq!(
            message(&answer).as_deref(),
            Some(*sentence),
            "case {case:?}: expected {sentence:?}, got {:?}",
            message(&answer)
        );
    }
}
