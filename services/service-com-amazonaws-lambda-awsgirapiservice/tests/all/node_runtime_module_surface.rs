// SPDX-License-Identifier: CC0-1.0

//! The MODULE and GLOBAL surface a managed `nodejs*` runtime gives a handler, end to end over
//! the wire: a function is created with a deployment package, invoked, and what its handler
//! read off its own runtime is asserted out of the response payload.
//!
//! RULE: a managed node runtime is Node.js, so what a handler reads off it is what the Node
//! release its `Runtime` names states — every module of the standard library resolves under
//! both the bare and the `node:` name, the globals that release defines are defined before the
//! first module of the package is evaluated, a path the runtime builds is a path (one separator
//! per step, no trailing one), and a bare specifier resolves through the dependency's own
//! `package.json` — including its `exports` subpath map, which is what a modern package
//! publishes its entry points as.
//!
//! The rows of each table differ along what the rule generalises over: WHICH module, WHICH
//! global, the wrapper and the form of a compression call, the shape of an `exports` map
//! (string shorthand, exact subpath, conditions, pattern), and where in the package a module
//! sits that reads its own directory.
//!
//! What a module needs a host capability this emulator has none of for is asserted where it
//! belongs: the import LOADS (this suite), and the call that needs the capability is refused
//! (`node_child_process_module` for the process half).

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` these functions carry: a managed node release, which is what supplies the
/// surface under test.
const RUNTIME: &str = "nodejs22.x";

/// How long this suite waits for a function to become `Active`, and how often it looks again:
/// the transition is owed by the CLOCK, so the wait is for that CONDITION and the deadline is
/// generous — a loaded host may take far longer than an idle one.
const ACTIVE_BUDGET: Duration = Duration::from_secs(60);

/// How often that wait looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Lambda alone: nothing here reaches another service.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names Lambda, which is what routes the
/// request. The signature is never checked here.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/lambda/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda `restJson1` call, as its status, headers and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// A .zip deployment package holding each `(path, source)` entry.
fn package(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        for (path, source) in entries {
            writer.start_file(*path, options).expect("the entry starts");
            writer
                .write_all(source.as_bytes())
                .expect("the entry's bytes");
        }
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create a node function whose package holds `entries` and wait until Lambda reports it
/// `Active` — which is what a client's own waiter does, because "you can't invoke or modify a
/// function" until the state the clock owes has arrived.
async fn create_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    entries: &[(&str, &str)],
) {
    let (status, _headers, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(entries)),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    let deadline = std::time::Instant::now() + ACTIVE_BUDGET;
    loop {
        let (status, _headers, reported) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            b"",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Lambda to hold the function, got {status} ({reported})"
        );
        if reported["State"] == json!("Active") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the function never became Active: {reported}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `Invoke` the function synchronously with `event`, asserting the handler SUCCEEDED — no
/// `FunctionError` signal — and answering the payload it returned.
async fn invoke_ok(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        signal, None,
        "case {case:?}: expected the handler to succeed, got the error signal {signal:?} with \
         {payload}"
    );
    payload
}

/// Every module of the standard library this runtime supplies, by the name a handler imports
/// it under.
///
/// Source: the node 22 documentation's own module index, narrowed to the modules this
/// emulator's runtime states ([`core_modules::CORE_MODULES`] in the crate). The rows the
/// issue's own check names are here — `http`, `https`, `net`, `dns`, `tls`, `zlib`,
/// `child_process`, `worker_threads`, `perf_hooks`, `async_hooks`, `readline`, `v8`, `vm` —
/// beside the ones it did not cite.
const MODULES: &[&str] = &[
    "assert",
    "assert/strict",
    "async_hooks",
    "buffer",
    "child_process",
    "crypto",
    "dns",
    "dns/promises",
    "events",
    "fs",
    "fs/promises",
    "http",
    "https",
    "net",
    "os",
    "path",
    "perf_hooks",
    "querystring",
    "readline",
    "readline/promises",
    "stream",
    "stream/promises",
    "string_decoder",
    "timers",
    "timers/promises",
    "tls",
    "url",
    "util",
    "v8",
    "vm",
    "worker_threads",
    "zlib",
];

/// RULE: every module of a managed node runtime's standard library is importable, under the
/// bare name and under the `node:` name a modern import spells it with, whatever host
/// capability the module's own calls need.
///
/// The rows are every module this runtime supplies ([`MODULES`]), each asked for both ways.
#[tokio::test]
async fn every_standard_library_module_imports_under_both_of_its_names() {
    let router = router();
    let case = "the standard library's own module index";
    create_function(
        &router,
        case,
        "module-surface",
        &[(
            "index.js",
            r#"
'use strict';

exports.handler = async (event) => {
    const loaded = [];
    const failed = [];
    for (const name of event.modules) {
        for (const specifier of [name, 'node:' + name]) {
            try {
                const held = require(specifier);
                if (held === null || held === undefined) {
                    failed.push({ specifier, error: 'the module resolved to nothing' });
                } else {
                    loaded.push(specifier);
                }
            } catch (error) {
                failed.push({ specifier, error: error.message });
            }
        }
    }
    return { loaded, failed };
};
"#,
        )],
    )
    .await;
    let answered = invoke_ok(
        &router,
        case,
        "module-surface",
        &json!({ "modules": MODULES }),
    )
    .await;
    assert_eq!(
        answered["failed"],
        json!([]),
        "case {case:?}: expected every module to import, got {}",
        answered["failed"]
    );
    assert_eq!(
        answered["loaded"].as_array().map(Vec::len),
        Some(MODULES.len() * 2),
        "case {case:?}: expected every module under both names, got {}",
        answered["loaded"]
    );
    // A module the runtime does not supply is still not resolvable: the surface is the
    // standard library's, not everything a specifier might name.
    let absent = invoke_ok(
        &router,
        "a module no node release has",
        "module-surface",
        &json!({ "modules": ["no_such_core_module"] }),
    )
    .await;
    assert_eq!(
        absent["loaded"],
        json!([]),
        "a specifier naming no module of the runtime must resolve to nothing, got {}",
        absent["loaded"]
    );
}

/// RULE: the `zlib` module computes one deflate stream under each of the wrappers a program
/// asks for it by, so bytes handed to a wrapper's compressor come back out of its
/// decompressor — in every form node states the call in.
///
/// The rows differ along the wrapper (`gzip`, `deflate`, raw deflate, and the automatic one
/// `unzip` reads off the stream), the form (synchronous, callback, stream), the level, and the
/// bytes themselves.
#[tokio::test]
async fn every_compression_wrapper_round_trips_a_payload() {
    let router = router();
    let case = "the deflate family";
    create_function(
        &router,
        case,
        "zlib-surface",
        &[(
            "index.js",
            r#"
'use strict';

const zlib = require('zlib');
const { promisify } = require('util');

exports.handler = async (event) => {
    const payload = Buffer.from(event.payload, 'utf8');
    const out = {};
    // The synchronous form of each wrapper.
    out.gzipSync = zlib.gunzipSync(zlib.gzipSync(payload, { level: event.level })).toString();
    out.deflateSync = zlib.inflateSync(zlib.deflateSync(payload)).toString();
    out.rawSync = zlib.inflateRawSync(zlib.deflateRawSync(payload)).toString();
    // Brotli is the other compressor the module states, with its own options.
    out.brotliSync = zlib.brotliDecompressSync(zlib.brotliCompressSync(payload)).toString();
    // The checksum of the same bytes, and the same checksum computed a chunk at a time.
    out.crc = zlib.crc32(payload);
    out.crcInChunks = zlib.crc32(
        payload.slice(Math.floor(payload.length / 2)),
        zlib.crc32(payload.slice(0, Math.floor(payload.length / 2)))
    );
    // `unzip` reads the wrapper off the stream itself.
    out.unzipGzip = zlib.unzipSync(zlib.gzipSync(payload)).toString();
    out.unzipDeflate = zlib.unzipSync(zlib.deflateSync(payload)).toString();
    // The callback form, through the promise adapter a program usually calls it by.
    const gzip = promisify(zlib.gzip);
    const gunzip = promisify(zlib.gunzip);
    out.callback = (await gunzip(await gzip(payload))).toString();
    // The stream form.
    out.stream = await new Promise((resolve, reject) => {
        const chunks = [];
        const deflate = zlib.createGzip();
        const inflate = zlib.createGunzip();
        deflate.on('data', (chunk) => inflate.write(chunk));
        deflate.on('end', () => inflate.end());
        inflate.on('data', (chunk) => chunks.push(chunk));
        inflate.on('end', () => resolve(Buffer.concat(chunks).toString()));
        deflate.on('error', reject);
        inflate.on('error', reject);
        deflate.end(payload);
    });
    // The compressed bytes are shorter than the text they compress, which is what says the
    // stream was computed rather than copied.
    out.compressedLength = zlib.gzipSync(payload).length;
    out.plainLength = payload.length;
    // A stream that is not the wrapper's own is the data error zlib reports for one.
    try {
        zlib.gunzipSync(Buffer.from('not a gzip member'));
        out.badData = 'returned';
    } catch (error) {
        out.badData = error.code;
    }
    out.constant = zlib.constants.Z_BEST_COMPRESSION;
    return out;
};
"#,
        )],
    )
    .await;
    // Each row: the case, the payload, and the level asked for.
    let cases: &[(&str, String, Value)] = &[
        ("text that compresses", "lambda ".repeat(64), json!(-1)),
        ("one byte", "x".to_string(), json!(9)),
        ("no bytes at all", String::new(), json!(0)),
        (
            "text with multi-byte characters",
            "λ×\u{1f680}".repeat(16),
            json!(6),
        ),
    ];
    for (named, payload, level) in cases {
        let answered = invoke_ok(
            &router,
            named,
            "zlib-surface",
            &json!({ "payload": payload, "level": level }),
        )
        .await;
        for form in [
            "gzipSync",
            "deflateSync",
            "rawSync",
            "brotliSync",
            "unzipGzip",
            "unzipDeflate",
            "callback",
            "stream",
        ] {
            assert_eq!(
                answered[form], json!(payload),
                "case {named:?} through {form}: expected the payload back, got {}",
                answered[form]
            );
        }
        assert_eq!(
            answered["badData"],
            json!("Z_DATA_ERROR"),
            "case {named:?}: a stream that is not a gzip member is the data error, got {}",
            answered["badData"]
        );
        assert_eq!(
            answered["constant"],
            json!(9),
            "case {named:?}: zlib's own constants are what a program reads a level by, got {}",
            answered["constant"]
        );
        assert_eq!(
            answered["crcInChunks"], answered["crc"],
            "case {named:?}: a checksum continued from the first chunk is the whole input's, \
             got {} against {}",
            answered["crcInChunks"], answered["crc"]
        );
    }
    // The wrapper really compresses: a payload with a lot of repetition is shorter compressed
    // than plain.
    let answered = invoke_ok(
        &router,
        "a payload that compresses",
        "zlib-surface",
        &json!({ "payload": "lambda ".repeat(256), "level": -1 }),
    )
    .await;
    let compressed = answered["compressedLength"].as_u64().unwrap_or(u64::MAX);
    let plain = answered["plainLength"].as_u64().unwrap_or(0);
    assert!(
        compressed < plain,
        "a repetitive payload compresses: expected fewer than {plain} bytes, got {compressed}"
    );
}

/// The globals a managed node 18-or-later runtime defines beside the language's own.
///
/// Source: the node 22 documentation's "Global objects" index, narrowed to the names that are
/// not the language's own. The row the issue's own check names — `structuredClone` — is here
/// beside the rest of that index.
const GLOBALS: &[(&str, &str)] = &[
    ("structuredClone", "function"),
    ("fetch", "function"),
    ("queueMicrotask", "function"),
    ("atob", "function"),
    ("btoa", "function"),
    ("setImmediate", "function"),
    ("clearImmediate", "function"),
    ("performance", "object"),
    ("navigator", "object"),
    ("global", "object"),
    ("process", "object"),
    ("Buffer", "function"),
    ("URL", "function"),
    ("URLSearchParams", "function"),
    ("TextEncoder", "function"),
    ("TextDecoder", "function"),
    ("AbortController", "function"),
    ("AbortSignal", "function"),
    ("Event", "function"),
    ("EventTarget", "function"),
    ("CustomEvent", "function"),
    ("MessageEvent", "function"),
    ("MessageChannel", "function"),
    ("MessagePort", "function"),
    ("Worker", "function"),
    ("DOMException", "function"),
    ("Blob", "function"),
    ("File", "function"),
    ("Headers", "function"),
    ("Request", "function"),
    ("Response", "function"),
    ("FormData", "function"),
    ("crypto", "object"),
];

/// RULE: the globals the Node release a runtime names defines are defined on the global object
/// before the first module of the deployment package is evaluated, so a handler reads them
/// without importing anything.
///
/// The rows are that whole index ([`GLOBALS`]); beside them the structured clone is asserted to
/// BE one — a deep copy that keeps the objects the algorithm states a form for and the shape a
/// reference seen twice has.
#[tokio::test]
async fn the_runtime_defines_the_globals_of_its_release() {
    let router = router();
    let case = "the global index";
    create_function(
        &router,
        case,
        "global-surface",
        &[(
            "index.js",
            r#"
'use strict';

// At MODULE SCOPE: a runtime that defined these only later would fail HERE.
const seenAtModuleScope = typeof structuredClone;

exports.handler = async (event) => {
    const kinds = {};
    for (const name of event.globals) {
        kinds[name] = typeof globalThis[name];
    }
    const source = {
        text: 'lambda',
        when: new Date(1700000000000),
        pattern: /a+/gi,
        list: [1, 2, { deep: true }],
        map: new Map([['key', { held: 1 }]]),
        set: new Set([1, 2]),
        bytes: Uint8Array.from([1, 2, 3]),
    };
    source.itself = source;
    const copied = structuredClone(source);
    copied.list[2].deep = false;
    let refused = null;
    try {
        structuredClone({ callback: () => 1 });
    } catch (error) {
        refused = error.name;
    }
    const target = new EventTarget();
    let delivered = null;
    target.addEventListener('ping', (event) => { delivered = event.detail; });
    target.dispatchEvent(new CustomEvent('ping', { detail: 'carried' }));
    return {
        seenAtModuleScope,
        kinds,
        clone: {
            text: copied.text,
            when: copied.when instanceof Date ? copied.when.getTime() : 'not a Date',
            pattern: copied.pattern instanceof RegExp ? copied.pattern.source : 'not a RegExp',
            mapped: copied.map instanceof Map ? copied.map.get('key').held : 'not a Map',
            setSize: copied.set instanceof Set ? copied.set.size : 'not a Set',
            bytes: Array.from(copied.bytes),
            cyclic: copied.itself === copied,
            independent: source.list[2].deep === true,
        },
        refused,
        delivered,
        clock: typeof performance.now() === 'number',
    };
};
"#,
        )],
    )
    .await;
    let names: Vec<&str> = GLOBALS.iter().map(|(name, _)| *name).collect();
    let answered = invoke_ok(
        &router,
        case,
        "global-surface",
        &json!({ "globals": names }),
    )
    .await;
    assert_eq!(
        answered["seenAtModuleScope"],
        json!("function"),
        "case {case:?}: the globals are defined before a module of the package runs, got {}",
        answered["seenAtModuleScope"]
    );
    for (name, kind) in GLOBALS {
        assert_eq!(
            answered["kinds"][name],
            json!(kind),
            "case {name:?}: expected the global to be a {kind}, got {}",
            answered["kinds"][name]
        );
    }
    let cloned = &answered["clone"];
    for (member, expected) in [
        ("text", json!("lambda")),
        ("when", json!(1_700_000_000_000_i64)),
        ("pattern", json!("a+")),
        ("mapped", json!(1)),
        ("setSize", json!(2)),
        ("bytes", json!([1, 2, 3])),
        ("cyclic", json!(true)),
        ("independent", json!(true)),
    ] {
        assert_eq!(
            cloned[member], expected,
            "case the structured clone's {member:?}: expected {expected}, got {}",
            cloned[member]
        );
    }
    assert_eq!(
        answered["refused"],
        json!("DataCloneError"),
        "a value the algorithm states no form for is refused, got {}",
        answered["refused"]
    );
    assert_eq!(
        answered["delivered"],
        json!("carried"),
        "an event dispatched at a target reaches the listener, got {}",
        answered["delivered"]
    );
    assert_eq!(
        answered["clock"],
        json!(true),
        "the performance clock answers a number, got {}",
        answered["clock"]
    );
}

/// One published PBKDF2 test vector: the inputs the standard states and the key it publishes
/// for them.
struct Vector {
    case: &'static str,
    password: &'static str,
    salt: &'static str,
    iterations: u32,
    length: usize,
    digest: &'static str,
    expected: &'static str,
}

/// RULE: the key `crypto`'s password-based derivation answers is the one PKCS #5 states for the
/// password, salt, iteration count and digest it was given, so a key derived in a handler is
/// the key any other implementation derives.
///
/// The rows are RFC 6070's published PBKDF2-HMAC-SHA-1 vectors and RFC 7914 §11's SHA-256 one,
/// which differ along the iteration count, the key length and the digest; beside them the key
/// is used to open a payload a cipher closed, which is the pattern a handler derives one for.
#[tokio::test]
async fn a_derived_key_is_the_one_its_construction_states() {
    let router = router();
    let case = "the password-based derivation";
    create_function(
        &router,
        case,
        "derivation-surface",
        &[(
            "index.js",
            r#"
'use strict';

const crypto = require('crypto');

exports.handler = async (event) => {
    const derived = crypto.pbkdf2Sync(
        event.password,
        event.salt,
        event.iterations,
        event.length,
        event.digest
    ).toString('hex');
    const asynchronous = await new Promise((resolve, reject) => {
        crypto.pbkdf2(
            event.password,
            event.salt,
            event.iterations,
            event.length,
            event.digest,
            (error, key) => (error ? reject(error) : resolve(key.toString('hex')))
        );
    });
    // The key a handler derives is the key it opens an envelope with: a round trip through the
    // cipher with the derived key says the bytes are usable, not only equal.
    const key = crypto.pbkdf2Sync('secret', 'salt', 1000, 32, 'sha256');
    const iv = Buffer.alloc(16, 7);
    const sealing = crypto.createCipheriv('aes-256-cbc', key, iv);
    const sealed = Buffer.concat([sealing.update('a payload', 'utf8'), sealing.final()]);
    const opening = crypto.createDecipheriv('aes-256-cbc', key, iv);
    const opened = Buffer.concat([opening.update(sealed), opening.final()]).toString('utf8');
    let refused = null;
    try {
        crypto.pbkdf2Sync('secret', 'salt', 0, 32, 'sha256');
    } catch (error) {
        refused = error.code;
    }
    return { derived, asynchronous, opened, refused };
};
"#,
        )],
    )
    .await;
    let cases: &[Vector] = &[
        Vector {
            case: "RFC 6070 case 1",
            password: "password",
            salt: "salt",
            iterations: 1,
            length: 20,
            digest: "sha1",
            expected: "0c60c80f961f0e71f3a9b524af6012062fe037a6",
        },
        Vector {
            case: "RFC 6070 case 3",
            password: "password",
            salt: "salt",
            iterations: 4096,
            length: 20,
            digest: "sha1",
            expected: "4b007901b765489abead49d926f721d065a429c1",
        },
        Vector {
            case: "RFC 6070 case 5",
            password: "passwordPASSWORDpassword",
            salt: "saltSALTsaltSALTsaltSALTsaltSALTsalt",
            iterations: 4096,
            length: 25,
            digest: "sha1",
            expected: "3d2eec4fe41c849b80c8d83662c0e44a8b291a964cf2f07038",
        },
        Vector {
            case: "RFC 7914 section 11, over SHA-256",
            password: "passwd",
            salt: "salt",
            iterations: 1,
            length: 64,
            digest: "sha256",
            expected: "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783",
        },
    ];
    for Vector {
        case: named,
        password,
        salt,
        iterations,
        length,
        digest,
        expected,
    } in cases
    {
        let answered = invoke_ok(
            &router,
            named,
            "derivation-surface",
            &json!({
                "password": password,
                "salt": salt,
                "iterations": iterations,
                "length": length,
                "digest": digest,
            }),
        )
        .await;
        assert_eq!(
            answered["derived"],
            json!(expected),
            "case {named:?}: expected the published derived key, got {}",
            answered["derived"]
        );
        assert_eq!(
            answered["asynchronous"],
            json!(expected),
            "case {named:?}: the callback form derives the same key, got {}",
            answered["asynchronous"]
        );
        assert_eq!(
            answered["opened"],
            json!("a payload"),
            "case {named:?}: a key derived here opens what it sealed, got {}",
            answered["opened"]
        );
        assert_eq!(
            answered["refused"],
            json!("ERR_OUT_OF_RANGE"),
            "case {named:?}: a count outside the range is refused, got {}",
            answered["refused"]
        );
    }
}

/// RULE: a bare specifier resolves through the dependency's own `package.json`, including its
/// `exports` map — the subpath it states, the conditions it states one under, and the pattern
/// it states a family of them with — and a package that declares the map is reached through it
/// alone.
///
/// The rows differ along the shape of the map: the string shorthand for the package itself, an
/// exact subpath, a conditional object, a pattern key, and a path the map does not state.
#[tokio::test]
async fn a_bare_specifier_resolves_through_the_packages_exports_map() {
    let router = router();
    let case = "a dependency publishing subpath exports";
    create_function(
        &router,
        case,
        "exports-surface",
        &[
            (
                "index.js",
                r#"
'use strict';

exports.handler = async (event) => {
    const out = {};
    for (const specifier of event.specifiers) {
        try {
            out[specifier] = require(specifier).name;
        } catch (error) {
            out[specifier] = 'failed: ' + error.code;
        }
    }
    out.messages = {};
    for (const specifier of event.refused) {
        try {
            require(specifier);
            out.messages[specifier] = 'loaded';
        } catch (error) {
            out.messages[specifier] = error.message;
        }
    }
    // The same subpath under the other KIND of load: a dependency that publishes a CommonJS
    // entry point beside an ES one is resolved to the one for the way it is being loaded.
    try {
        out.imported = (await import('expkg/dual')).name;
    } catch (error) {
        out.imported = 'failed: ' + error.message;
    }
    return out;
};
"#,
            ),
            (
                "node_modules/expkg/package.json",
                r#"{
  "name": "expkg",
  "version": "1.0.0",
  "exports": {
    ".": "./lib/main.js",
    "./sub": { "require": "./lib/sub.js", "default": "./lib/sub.mjs" },
    "./dual": { "import": "./lib/dual.mjs", "require": "./lib/dual.js" },
    "./deep/*": "./lib/deep/*.js"
  }
}
"#,
            ),
            ("node_modules/expkg/lib/main.js", "exports.name = 'main';\n"),
            ("node_modules/expkg/lib/sub.js", "exports.name = 'sub';\n"),
            (
                "node_modules/expkg/lib/dual.js",
                "exports.name = 'dual-cjs';\n",
            ),
            (
                "node_modules/expkg/lib/dual.mjs",
                "export const name = 'dual-esm';\n",
            ),
            (
                "node_modules/expkg/lib/deep/one.js",
                "exports.name = 'deep-one';\n",
            ),
            (
                "node_modules/expkg/lib/hidden.js",
                "exports.name = 'hidden';\n",
            ),
            (
                "node_modules/plainpkg/package.json",
                r#"{ "name": "plainpkg", "main": "./entry.js" }"#,
            ),
            ("node_modules/plainpkg/entry.js", "exports.name = 'plain';\n"),
            (
                "node_modules/shorthand/package.json",
                r#"{ "name": "shorthand", "exports": "./only.js" }"#,
            ),
            (
                "node_modules/shorthand/only.js",
                "exports.name = 'shorthand';\n",
            ),
        ],
    )
    .await;
    // Each row: the specifier, and what resolving it answers.
    let cases: &[(&str, &str)] = &[
        ("expkg", "main"),
        ("expkg/sub", "sub"),
        ("expkg/deep/one", "deep-one"),
        // A `require` takes the conditions node activates for one, so the CommonJS entry point
        // is the one it reaches even where the map lists the ES one first.
        ("expkg/dual", "dual-cjs"),
        ("shorthand", "shorthand"),
        // A package that declares no map is still resolved the legacy way, through `main`.
        ("plainpkg", "plain"),
        // A file the map does not state is not an entry of the package, even though the
        // package carries it: "the exports field … encapsulates the package". The package was
        // found, so the refusal is the one node states for a subpath it does not export —
        // which is not the same failure as a module that is nowhere.
        ("expkg/lib/hidden.js", "failed: ERR_PACKAGE_PATH_NOT_EXPORTED"),
        ("expkg/missing", "failed: ERR_PACKAGE_PATH_NOT_EXPORTED"),
        // A package that is nowhere is still the module-not-found it is.
        ("nosuchpkg", "failed: MODULE_NOT_FOUND"),
    ];
    let specifiers: Vec<&str> = cases.iter().map(|(specifier, _)| *specifier).collect();
    let answered = invoke_ok(
        &router,
        case,
        "exports-surface",
        &json!({
            "specifiers": specifiers,
            "refused": ["expkg/lib/hidden.js", "nosuchpkg"],
        }),
    )
    .await;
    for (specifier, expected) in cases {
        assert_eq!(
            answered[specifier],
            json!(expected),
            "case {specifier:?}: expected {expected:?}, got {}",
            answered[specifier]
        );
    }
    assert_eq!(
        answered["imported"],
        json!("dual-esm"),
        "an `import` of the same subpath takes the condition its own kind of load activates, \
         got {}",
        answered["imported"]
    );
    // Each refusal says WHY in the words node words it: the subpath that was asked for and the
    // manifest that does not state it, or the specifier that named no module at all.
    assert_eq!(
        answered["messages"]["expkg/lib/hidden.js"],
        json!(
            "Package subpath './lib/hidden.js' is not defined by \"exports\" in \
             /var/task/node_modules/expkg/package.json"
        ),
        "expected the subpath and the manifest to be named, got {}",
        answered["messages"]["expkg/lib/hidden.js"]
    );
    assert!(
        answered["messages"]["nosuchpkg"]
            .as_str()
            .is_some_and(|held| held.starts_with("Cannot find module 'nosuchpkg'")),
        "expected the module-not-found wording for a package that is nowhere, got {}",
        answered["messages"]["nosuchpkg"]
    );
}

/// RULE: a path this runtime reports carries one separator per step and none at its end, so the
/// directory a module sits in is that directory's own path — which is what a module joining an
/// asset's relative path onto `__dirname` builds from, and what `LAMBDA_TASK_ROOT` compares
/// equal to for a module at the root of the package.
///
/// The rows differ along WHERE in the package the module reading its own directory sits: the
/// root, a subdirectory, and a deeper one.
#[tokio::test]
async fn a_modules_directory_is_a_path_without_a_trailing_separator() {
    let router = router();
    let case = "a module reading its own directory";
    create_function(
        &router,
        case,
        "dirname-surface",
        &[
            (
                "index.js",
                r#"
'use strict';

const fs = require('fs');
const path = require('path');

exports.handler = async () => ({
    root: {
        dirname: __dirname,
        filename: __filename,
        taskRoot: process.env.LAMBDA_TASK_ROOT,
        matchesTaskRoot: __dirname === process.env.LAMBDA_TASK_ROOT,
        joined: path.join(__dirname, 'config.json'),
        read: JSON.parse(fs.readFileSync(path.join(__dirname, 'config.json'), 'utf8')).stage,
    },
    nested: require('./lib/where.js'),
    deeper: require('./lib/deeper/where.js'),
});
"#,
            ),
            ("config.json", r#"{ "stage": "test" }"#),
            (
                "lib/where.js",
                "module.exports = { dirname: __dirname, filename: __filename };\n",
            ),
            (
                "lib/deeper/where.js",
                "module.exports = { dirname: __dirname, filename: __filename };\n",
            ),
        ],
    )
    .await;
    let answered = invoke_ok(&router, case, "dirname-surface", &json!({})).await;
    // Each row: the module, its directory, and its own file.
    let cases: &[(&str, &str, &str)] = &[
        ("root", "/var/task", "/var/task/index.js"),
        ("nested", "/var/task/lib", "/var/task/lib/where.js"),
        (
            "deeper",
            "/var/task/lib/deeper",
            "/var/task/lib/deeper/where.js",
        ),
    ];
    for (member, directory, file) in cases {
        assert_eq!(
            answered[member]["dirname"],
            json!(directory),
            "case {member:?}: expected the directory {directory:?}, got {}",
            answered[member]["dirname"]
        );
        assert_eq!(
            answered[member]["filename"],
            json!(file),
            "case {member:?}: expected the file {file:?}, got {}",
            answered[member]["filename"]
        );
    }
    assert_eq!(
        answered["root"]["matchesTaskRoot"],
        json!(true),
        "the task root the runtime exports is the directory a module at the root reads, got {} \
         against {}",
        answered["root"]["dirname"],
        answered["root"]["taskRoot"]
    );
    assert_eq!(
        answered["root"]["joined"],
        json!("/var/task/config.json"),
        "a path joined onto that directory has one separator, got {}",
        answered["root"]["joined"]
    );
    assert_eq!(
        answered["root"]["read"],
        json!("test"),
        "the file at that path is the package's own, got {}",
        answered["root"]["read"]
    );
}

/// RULE: a worker's entry point RUNS — its code is evaluated in a module scope of its own, with
/// its own `workerData` and its own end of the channel — and what it posts back is what it
/// computed.
///
/// The rows differ along how the entry point was given (a file of the package, source passed
/// with `eval`), and along what the worker does with its port (posts once and ends, answers
/// each message it is sent).
#[tokio::test]
async fn a_workers_entry_point_runs_and_answers_what_it_computed() {
    let router = router();
    let case = "a worker of the deployment package";
    create_function(
        &router,
        case,
        "worker-surface",
        &[
            (
                "index.js",
                r#"
'use strict';

const path = require('path');
const { Worker, isMainThread, threadId } = require('worker_threads');

function once(worker) {
    return new Promise((resolve, reject) => {
        worker.on('message', resolve);
        worker.on('error', reject);
        worker.on('exit', (code) => {
            if (code !== 0) {
                reject(new Error('the worker exited with ' + code));
            }
        });
    });
}

exports.handler = async (event) => {
    const out = { isMainThread, threadId };
    // An entry point that is a file of the package, handed its own data.
    const fromFile = new Worker(path.join(__dirname, 'worker.js'), {
        workerData: { numbers: event.numbers },
    });
    out.fromFile = await once(fromFile);
    // An entry point passed as source.
    const fromSource = new Worker(
        "const { parentPort, workerData } = require('worker_threads');"
            + 'parentPort.postMessage({ doubled: workerData.value * 2, thread: '
            + "require('worker_threads').threadId });",
        { eval: true, workerData: { value: event.value } }
    );
    out.fromSource = await once(fromSource);
    // A worker that answers the messages it is sent. The wait is for the ANSWERS, not for a
    // span: the second one resolves it.
    const answering = new Worker(path.join(__dirname, 'echo.js'));
    const answers = [];
    const answered = new Promise((resolve, reject) => {
        answering.on('message', (message) => {
            answers.push(message);
            if (answers.length === 2) {
                resolve();
            }
        });
        answering.on('error', reject);
    });
    answering.postMessage('first');
    answering.postMessage('second');
    await answered;
    await answering.terminate();
    out.answers = answers;
    return out;
};
"#,
            ),
            (
                "worker.js",
                r#"
'use strict';

const { parentPort, workerData, isMainThread, threadId } = require('worker_threads');
const total = workerData.numbers.reduce((sum, held) => sum + held, 0);
parentPort.postMessage({ total, isMainThread, threadId });
"#,
            ),
            (
                "echo.js",
                r#"
'use strict';

const { parentPort } = require('worker_threads');
parentPort.on('message', (message) => parentPort.postMessage(message.toUpperCase()));
"#,
            ),
        ],
    )
    .await;
    // Each row: the numbers the file worker adds, the number the source worker doubles, and
    // what each must answer.
    let cases: &[(&str, Value, i64)] = &[
        ("a sum of three", json!([1, 2, 3]), 21),
        ("a sum of none", json!([]), 0),
        ("a longer sum", json!([10, 20, 30, 40]), -4),
    ];
    for (named, numbers, value) in cases {
        let answered = invoke_ok(
            &router,
            named,
            "worker-surface",
            &json!({ "numbers": numbers, "value": value }),
        )
        .await;
        let expected: i64 = numbers
            .as_array()
            .map(|held| held.iter().filter_map(Value::as_i64).sum())
            .unwrap_or_default();
        assert_eq!(
            answered["fromFile"]["total"],
            json!(expected),
            "case {named:?}: expected the worker to add its own data to {expected}, got {}",
            answered["fromFile"]
        );
        assert_eq!(
            answered["fromFile"]["isMainThread"],
            json!(false),
            "case {named:?}: a worker is not the main thread, got {}",
            answered["fromFile"]
        );
        assert_eq!(
            answered["fromSource"]["doubled"],
            json!(value * 2),
            "case {named:?}: expected the source worker to double {value}, got {}",
            answered["fromSource"]
        );
        assert_ne!(
            answered["fromSource"]["thread"],
            answered["fromFile"]["threadId"],
            "case {named:?}: each worker is a thread of its own, got {}",
            answered["fromSource"]
        );
        assert_eq!(
            answered["answers"],
            json!(["FIRST", "SECOND"]),
            "case {named:?}: a worker answers every message it is sent, got {}",
            answered["answers"]
        );
        assert_eq!(
            answered["isMainThread"],
            json!(true),
            "case {named:?}: the handler itself runs on the main thread, got {}",
            answered["isMainThread"]
        );
        assert_eq!(
            answered["threadId"],
            json!(0),
            "case {named:?}: the main thread is thread 0, got {}",
            answered["threadId"]
        );
    }
}

/// RULE: source a handler compiles at runtime RUNS, and a name a lookup resolves is answered by
/// the one resolver of the execution environment — which answers the addresses of the zones
/// this emulator can name an address in, and nothing for a name no such zone holds.
///
/// The rows differ along the source compiled (an expression, a sandbox the names of which it
/// reads and writes, a function body) and along the name resolved (an address, this machine,
/// a name a standard fixes, a name nothing holds).
#[tokio::test]
async fn compiled_source_runs_and_a_resolved_name_is_the_environments_own() {
    let router = router();
    let case = "the runtime's own compiler and resolver";
    create_function(
        &router,
        case,
        "scripting-surface",
        &[(
            "index.js",
            r#"
'use strict';

const vm = require('vm');
const dns = require('dns');
const v8 = require('v8');

exports.handler = async (event) => {
    const out = {};
    out.expression = vm.runInThisContext('1 + 2');
    const sandbox = { x: Number(event.x), y: 4 };
    out.sandboxed = vm.runInNewContext('x = x + y; x * 2', sandbox);
    out.written = sandbox.x;
    out.compiled = vm.compileFunction('return a * b;', ['a', 'b'])(6, 7);
    out.script = new vm.Script('"held " + (3 + 4)').runInThisContext();
    // The serialisation round-trips a value through this runtime's own encoding of it.
    out.serialized = v8.deserialize(v8.serialize({ held: [1, 'two', { three: true }] }));
    out.resolved = {};
    for (const name of event.names) {
        out.resolved[name] = await new Promise((resolve) => {
            dns.lookup(name, { all: true }, (error, addresses) => {
                resolve(error ? 'failed: ' + error.code : addresses.map((a) => a.address));
            });
        });
    }
    // A RECORD query answers records, which carry the type a program branches on.
    out.records = await new Promise((resolve) => {
        dns.resolveAny('127.0.0.1', (error, records) => {
            resolve(error ? 'failed: ' + error.code : records.map((held) => held.type));
        });
    });
    out.promised = await dns.promises.lookup('127.0.0.1').then(
        (held) => held.address,
        (error) => 'failed: ' + error.code
    );
    return out;
};
"#,
        )],
    )
    .await;
    let answered = invoke_ok(
        &router,
        case,
        "scripting-surface",
        &json!({
            "x": 3,
            "names": ["127.0.0.1", "localhost", "example.com", "nothing.invalid"],
        }),
    )
    .await;
    for (member, expected) in [
        ("expression", json!(3)),
        // `x = x + y` is 7 inside the sandbox, and the expression's own value is 14.
        ("sandboxed", json!(14)),
        ("written", json!(7)),
        ("compiled", json!(42)),
        ("script", json!("held 7")),
        ("serialized", json!({ "held": [1, "two", { "three": true }] })),
        ("promised", json!("127.0.0.1")),
        // `resolveAny` answers RECORDS, so the type is what a program reads off each one.
        ("records", json!(["A"])),
    ] {
        assert_eq!(
            answered[member], expected,
            "case {member:?}: expected {expected}, got {}",
            answered[member]
        );
    }
    // Each row: the name, and what the resolver of the execution environment answers for it.
    let resolved = &answered["resolved"];
    assert_eq!(
        resolved["127.0.0.1"],
        json!(["127.0.0.1"]),
        "an address resolves to itself, got {}",
        resolved["127.0.0.1"]
    );
    assert_eq!(
        resolved["localhost"],
        json!(["127.0.0.1"]),
        "this machine's own name resolves to its loopback, got {}",
        resolved["localhost"]
    );
    assert_eq!(
        resolved["nothing.invalid"],
        json!("failed: ENOTFOUND"),
        "a name no zone holds resolves to nothing, got {}",
        resolved["nothing.invalid"]
    );
    assert!(
        resolved["example.com"]
            .as_array()
            .is_some_and(|held| !held.is_empty()),
        "a name a standard fixes resolves to the addresses that standard reserves, got {}",
        resolved["example.com"]
    );
}
