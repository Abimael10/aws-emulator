// SPDX-License-Identifier: CC0-1.0

//! The deployment package of a stack-deployed `AWS::Lambda::Function`, DOWNLOADED from the
//! address AWS Lambda announces for it.
//!
//! RULE under test: **a URL an operation hands the caller to fetch later is addressed to the
//! endpoint this emulator is reached on, and following it serves exactly the bytes the service
//! that owns the object holds for it.** For `Code$Location` — "A presigned URL that you can use
//! to download the deployment package" (`smithy_query(query="doc",
//! shape="FunctionCodeLocation", member="Location",
//! model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/\
//! com-amazonaws-lambda-awsgirapiservice.json")`) — those bytes are the .zip package the
//! deployment stored, and their digest and their length are what the same read reports as
//! `CodeSha256` and `CodeSize`. This emulator answers on its own endpoint alone, so a location
//! on any other host — a production AWS hostname among them — is one no client of it can
//! download anything from.
//!
//! What this suite reaches, which the owning crate's own download suites cannot, is the
//! TEMPLATE-DEPLOYED function: a template states a function's SOURCE TEXT in `Code.ZipFile`
//! while AWS Lambda's member of that name is the bytes of an archive, so the deployment
//! archives the declared source itself — and the download then has to serve THAT archive, with
//! the declared source under the module file the declared `Handler` resolves. That is the read a
//! client performs when it downloads a deployed function's code to inspect, diff or re-publish
//! it.
//!
//! The rows differ along what the rule generalises over: the runtime family the declaration
//! names (and so the extension of the module the source is archived under), the module stem the
//! declared `Handler` resolves, the length of the declared source, and whether the package is
//! the archive the deployment wrote from inline source or an Amazon S3 object a client staged
//! itself (whose bytes the download has to serve back byte for byte). A last test drives one
//! deployment over emulators reached at base URLs of three different forms, because the
//! announced address has to FOLLOW the endpoint rather than be a constant.
//!
//! The first emulator is reached at a loopback address with a port, the form a client of one is
//! handed, so a location that named anything else would fail the first assertion of every row.
//! Every expected value is computed here from what the row declares or stages; nothing is read
//! out of the answer being checked.

use std::io::Read as _;

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::harness::{
    create_stack, router_with_engine, settled_stack_status, stack_event_reasons, REGION,
};
use crate::hotswapped_function_code::{lambda, package};
use crate::stack_declared_function_identity::trust_policy;
use crate::stack_function_package_identity::expected_digest;
use crate::stack_lifecycle::acknowledging_iam;
use crate::stack_resource_provisioning::s3;

/// The base URL the emulator of this suite is reached on: a loopback address with a port, which
/// is the form a client of an emulator is handed. No URL it announces may disagree with it.
const BASE: &str = "https://127.0.0.1:34711";

/// The status a healthy create settles on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The JSON pointers a `GetFunction` answer carries the download link and the package identity
/// at.
const CODE_LOCATION: &str = "/Code/Location";
const CODE_SHA256: &str = "/Configuration/CodeSha256";
const CODE_SIZE: &str = "/Configuration/CodeSize";

/// The logical ids every template below declares its two resources under.
const FUNCTION_LOGICAL_ID: &str = "Fn";
const ROLE_LOGICAL_ID: &str = "ExecRole";

/// The sources the rows declare inline, each written in the language of the runtime its row
/// names and entered at the name that row's `Handler` resolves.
const PYTHON_HANDLER_SOURCE: &str = "def handler(event, context):\n    return {'ok': True}\n";
const PYTHON_LAUNCHER_SOURCE: &str = "import json\n\n\ndef main(event, context):\n    \
                                      return {'started': json.dumps(event)}\n";
const NODE_HANDLER_SOURCE: &str = "exports.handler = async (event) => ({ ok: true });\n";
const RUBY_HANDLER_SOURCE: &str = "def handler(event:, context:)\n  { ok: true }\nend\n";

/// The source the Amazon S3 row stages inside the archive it uploads, which is a package a
/// client built rather than one the deployment wrote.
const STAGED_SOURCE: &str =
    "# staged by the client\ndef handler(event, context):\n    return {'staged': True}\n";

/// The Amazon S3 bucket and key the staged row names as its function's package.
const STAGED_BUCKET: &str = "pkgdl-staged-code";
const STAGED_KEY: &str = "builds/2026/launcher.zip";

/// Where one row's function takes its package from.
#[derive(Clone, Copy, Debug)]
enum Code {
    /// The template's own `Code.ZipFile` source text, which the deployment archives itself.
    InlineSource,
    /// An Amazon S3 object a client staged before the stack was submitted, the way `sam
    /// package` and `cdk deploy` upload one.
    StagedObject,
}

/// One row: a stack declaring one function, and what its package has to hold.
#[derive(Clone, Copy, Debug)]
struct Row {
    case: &'static str,
    /// The stack name, which the function name is derived from, so one row's resources are
    /// never another's.
    stack: &'static str,
    runtime: &'static str,
    handler: &'static str,
    /// The module file inside the package the declared `Handler` resolves: its stem is the
    /// handler's first segment and its extension is the runtime family's default source
    /// extension — `index.py` for Python, `index.js` for Node.js, `index.rb` for Ruby.
    module: &'static str,
    /// The source that module has to carry.
    source: &'static str,
    /// The module file CloudFormation SUPPLIES beside the declared source, where it publishes
    /// one for the runtime family the row names: "the cfn-response module is available only
    /// when you use the ZipFile property to write your function source code inline", and the
    /// module AWS documents for Python is imported as `cfnresponse`. `None` for a row whose
    /// package is the client's own — nothing is supplied into an archive a client uploaded.
    supplied_module: Option<&'static str>,
    code: Code,
}

/// The class the rule covers, where the package is the archive the DEPLOYMENT wrote from the
/// template's inline source.
const INLINE_ROWS: &[Row] = &[
    Row {
        case: "inline Python source under the handler's own module",
        stack: "pkgdl-python",
        runtime: "python3.12",
        handler: "index.handler",
        module: "index.py",
        source: PYTHON_HANDLER_SOURCE,
        supplied_module: Some("cfnresponse.py"),
        code: Code::InlineSource,
    },
    Row {
        case: "inline Python source whose handler names another module",
        stack: "pkgdl-launcher",
        runtime: "python3.12",
        handler: "launcher.main",
        module: "launcher.py",
        source: PYTHON_LAUNCHER_SOURCE,
        supplied_module: Some("cfnresponse.py"),
        code: Code::InlineSource,
    },
    Row {
        case: "inline Node.js source",
        stack: "pkgdl-node",
        runtime: "nodejs20.x",
        handler: "index.handler",
        module: "index.js",
        source: NODE_HANDLER_SOURCE,
        supplied_module: None,
        code: Code::InlineSource,
    },
    Row {
        case: "inline Ruby source",
        stack: "pkgdl-ruby",
        runtime: "ruby3.3",
        handler: "index.handler",
        module: "index.rb",
        source: RUBY_HANDLER_SOURCE,
        supplied_module: None,
        code: Code::InlineSource,
    },
];

/// The same rule where the package is the CLIENT's own: an archive staged in Amazon S3 before
/// the stack that names it was submitted, which the download has to serve back byte for byte.
const STAGED_ROW: Row = Row {
    case: "an Amazon S3 object a client staged itself",
    stack: "pkgdl-staged",
    runtime: "python3.12",
    handler: "index.handler",
    module: "index.py",
    source: STAGED_SOURCE,
    supplied_module: None,
    code: Code::StagedObject,
};

/// The function name a row's template declares, and so the physical id the stack publishes.
fn function_name(row: &Row) -> String {
    format!("{}-fn", row.stack)
}

/// The emulator this suite drives: every service a row's stack provisions into, reached at
/// `base` — the endpoint every URL it announces has to lead back to.
fn emulator_at(base: &str) -> MultiServiceRouter {
    let (mut router, _engine) = router_with_engine();
    router.set_url_base(EndpointUrl::parse(base).expect("the suite's base URL must parse"));
    router
}

/// The template one row deploys: the execution role the function runs as, and the function with
/// the `Code` the row states.
fn template(row: &Row, code: &Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE_LOGICAL_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": { "AssumeRolePolicyDocument": trust_policy() },
            },
            FUNCTION_LOGICAL_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(row),
                    "Runtime": row.runtime,
                    "Handler": row.handler,
                    "Role": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                    "Code": code,
                },
            },
        },
    })
    .to_string()
}

/// Stage the Amazon S3 object the staged row names, and answer the bytes that were stored: a
/// client uploads its own .zip package before submitting the stack that points at it.
async fn stage(router: &MultiServiceRouter, case: &str, row: &Row) -> Vec<u8> {
    let created = s3(router, "PUT", &format!("/{STAGED_BUCKET}"), b"").await;
    assert!(
        (200..300).contains(&created.status),
        "case {case:?}: expected Amazon S3 to hold the bucket {STAGED_BUCKET}, got {} {}",
        created.status,
        created.body
    );
    let archive = package(row.module, row.source);
    let staged = s3(
        router,
        "PUT",
        &format!("/{STAGED_BUCKET}/{STAGED_KEY}"),
        &archive,
    )
    .await;
    assert!(
        (200..300).contains(&staged.status),
        "case {case:?}: expected the package {STAGED_KEY} to be staged, got {} {}",
        staged.status,
        staged.body
    );
    archive
}

/// The `Code` property one row declares, with its Amazon S3 object staged first where it names
/// one — and the bytes the download then has to serve back verbatim, for a row that staged them.
async fn declared_code(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
) -> (Value, Option<Vec<u8>>) {
    match row.code {
        Code::InlineSource => (json!({ "ZipFile": row.source }), None),
        Code::StagedObject => {
            let archive = stage(router, case, row).await;
            (
                json!({ "S3Bucket": STAGED_BUCKET, "S3Key": STAGED_KEY }),
                Some(archive),
            )
        }
    }
}

/// What one request to the announced address answered.
struct Downloaded {
    status: u16,
    bytes: bytes::Bytes,
}

/// Follow a URL this emulator ANNOUNCED, the way a client follows a presigned one: at the
/// address the URL names, carrying no `Authorization` header at all.
async fn followed(router: &MultiServiceRouter, method: &str, url: &str) -> Downloaded {
    let host = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .unwrap_or_default()
        .to_string();
    let request = http::Request::builder()
        .method(method)
        .uri(url)
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("the download request must build");
    let response = router.dispatch(request).await;
    let (parts, bytes) = response.into_parts();
    Downloaded {
        status: parts.status.as_u16(),
        bytes,
    }
}

/// `GetFunction` on one function, which must succeed, as the document it answered.
async fn get_function(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let answered = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected AWS Lambda to hold the function {name:?}, got {} with body {}",
        answered.status, answered.body
    );
    answered.body
}

/// One string member of an answer, by JSON pointer.
fn text_at(case: &str, document: &Value, pointer: &str) -> String {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a string at {pointer}, got {document}")
        })
        .to_string()
}

/// The text one entry of a .zip archive carries, with the archive's own entry list named when it
/// holds no such entry — so a package missing the declared module says what it does hold.
fn entry_text(case: &str, archive: &[u8], file: &str) -> String {
    read_entry(case, archive, file).unwrap_or_else(|entries| {
        panic!("case {case:?}: expected the package to carry {file:?}, got {entries:?}")
    })
}

/// The text of one entry, or the archive's entry list when it holds no entry of that name.
fn read_entry(case: &str, archive: &[u8], file: &str) -> Result<String, Vec<String>> {
    let mut reader = zip::ZipArchive::new(std::io::Cursor::new(archive.to_vec()))
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected a .zip deployment package of {} bytes, got {error}",
                archive.len()
            )
        });
    // The names are read for the DIAGNOSTIC alone — what the package does hold when it does
    // not hold the module asked for — so a name the archive spells unreadably is left out of
    // the list rather than failing the read of the entry that was asked for.
    let entries: Vec<String> = reader
        .file_names()
        .filter_map(Result::ok)
        .map(|name| name.to_string())
        .collect();
    let Ok(mut entry) = reader.by_name(file) else {
        return Err(entries);
    };
    let mut text = String::new();
    entry.read_to_string(&mut text).unwrap_or_else(|error| {
        panic!("case {case:?}: expected {file:?} to read as text, got {error}")
    });
    Ok(text)
}

/// What the documented interface of the module CloudFormation supplies to inline code names:
/// `cfnresponse.send(event, context, responseStatus, responseData, ...)`, called with one of the
/// statuses it publishes — so a module that defines neither is not that module.
const SUPPLIED_MODULE_MARKERS: [&str; 2] = ["def send(", "SUCCESS"];

/// Deploy one row's stack against an emulator reached at `base`, download the package from the
/// address the read announces, and hold every half of the rule to it. The bytes served come
/// back, so a caller that staged the archive itself can compare them with its own.
///
/// One body for every case: each test below states WHICH cases it drives, so one failing case
/// does not hide another class's.
async fn served_package(base: &str, row: &Row) -> bytes::Bytes {
    let case = row.case;
    let router = emulator_at(base);
    let function = function_name(row);

    let (code, staged) = declared_code(&router, case, row).await;
    let (status, answered) =
        create_stack(&router, row.stack, &template(row, &code), &acknowledging_iam(&[])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateStack to be accepted, got {status} {answered}"
    );
    let settled = settled_stack_status(case, &router, REGION, row.stack).await;
    assert_eq!(
        settled,
        CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy, got {settled:?} with events {:?}",
        stack_event_reasons(&router, REGION, row.stack).await
    );

    // The address the read announces is this emulator's own, and names no host only AWS
    // serves: those are the only addresses a client of it can resolve.
    let read = get_function(&router, case, &function).await;
    let location = text_at(case, &read, CODE_LOCATION);
    assert!(
        location.starts_with(&format!("{base}/")),
        "case {case:?}: expected a download location on {base}, got {location:?}"
    );
    assert!(
        !location.contains("amazonaws.com"),
        "case {case:?}: a download location may not name a host only AWS serves, got {location:?}"
    );

    // Following it serves the package, under the digest and the length the same read reports.
    let served = followed(&router, "GET", &location).await;
    assert_eq!(
        served.status,
        200,
        "case {case:?}: expected 200 following {location:?}, got {} {}",
        served.status,
        String::from_utf8_lossy(&served.bytes)
    );
    let digest = expected_digest(&served.bytes);
    assert_eq!(
        text_at(case, &read, CODE_SHA256),
        digest,
        "case {case:?}: expected CodeSha256 to be the served bytes' digest {digest:?}, got {read}"
    );
    assert_eq!(
        read.pointer(CODE_SIZE).and_then(Value::as_i64),
        i64::try_from(served.bytes.len()).ok(),
        "case {case:?}: expected CodeSize to be the {} served bytes, got {read}",
        served.bytes.len()
    );

    // And the package it serves is the one the deployment stored: the declared source under
    // the module the declared handler resolves.
    let carried = entry_text(case, &served.bytes, row.module);
    assert_eq!(
        carried, row.source,
        "case {case:?}: expected {:?} to carry the declared source {:?}, got {carried:?}",
        row.module, row.source
    );
    // A package built from inline source carries the module CloudFormation supplies beside it,
    // where it publishes one for the declared runtime: an inline provider imports it to answer
    // its custom resource request, so a package carrying no such module — or a blank one — is
    // not one such a function can answer from.
    if let Some(supplied) = row.supplied_module {
        let module = read_entry(case, &served.bytes, supplied).unwrap_or_else(|entries| {
            panic!(
                "case {case:?}: expected the package to carry the supplied module \
                 {supplied:?}, got {entries:?}"
            )
        });
        for marker in SUPPLIED_MODULE_MARKERS {
            assert!(
                module.contains(marker),
                "case {case:?}: expected the supplied module {supplied:?} to define \
                 {marker:?}, got {module:?}"
            );
        }
    }
    // Where the client staged the archive itself, that archive comes back byte for byte.
    if let Some(staged) = staged.as_ref() {
        assert_eq!(
            served.bytes.as_ref(),
            staged.as_slice(),
            "case {case:?}: expected the {} staged bytes, got {} bytes",
            staged.len(),
            served.bytes.len()
        );
    }

    // A download is a pure READ: following the same address again serves the same bytes.
    let again = followed(&router, "GET", &location).await;
    assert_eq!(
        (again.status, again.bytes.as_ref()),
        (200, served.bytes.as_ref()),
        "case {case:?}: expected the same {} bytes on a second fetch, got {} with {} bytes",
        served.bytes.len(),
        again.status,
        again.bytes.len()
    );
    served.bytes
}

/// RULE: a stack-deployed function's `Code.Location` is an address this emulator serves, and
/// following it hands back the archive the deployment wrote from the template's inline source —
/// the declared text under the module the declared `Handler` resolves, beside the module
/// CloudFormation supplies for that runtime — under the digest and the length the same read
/// reports.
#[tokio::test]
async fn an_inline_sourced_package_is_downloaded_from_this_emulators_own_endpoint() {
    for row in INLINE_ROWS {
        served_package(BASE, row).await;
    }
}

/// The same rule for a package the CLIENT built: the archive it staged in Amazon S3 is what the
/// announced address serves, byte for byte, and no two of these packages are one object (the
/// staged archive is not the inline rows').
#[tokio::test]
async fn a_staged_package_is_downloaded_from_this_emulators_own_endpoint() {
    let staged = served_package(BASE, &STAGED_ROW).await;
    let inline = served_package(BASE, &INLINE_ROWS[0]).await;
    assert_ne!(
        staged, inline,
        "expected each function's location to address its own package, got {} bytes twice",
        staged.len()
    );
}

/// The announced address FOLLOWS the endpoint this emulator is reached on: the same deployment
/// over emulators with different base URLs announces its package on each one, and the package is
/// fetchable there. The rows differ along the base URL's form — a loopback address with a port,
/// a host name with a port, and a host name with no port at all.
#[tokio::test]
async fn the_announced_package_address_follows_the_endpoint_the_emulator_is_reached_on() {
    for base in [BASE, "https://cfn.emulator.test:9443", "https://emulator.test"] {
        let served = served_package(base, &INLINE_ROWS[0]).await;
        assert!(
            !served.is_empty(),
            "case {base:?}: expected the announced address to serve the package, got no bytes"
        );
    }
}
