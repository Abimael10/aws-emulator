// SPDX-License-Identifier: CC0-1.0

//! The `child_process` module a managed node runtime supplies, end to end over the wire:
//! Lambda and Amazon DynamoDB on one engine, a `nodejs*` function whose MODULE BODY imports
//! the child-process entry points — the shape every bundled custom-resource provider takes,
//! since the installer such a bundle carries imports them at module scope — and what each of
//! those entry points reports when it is called.
//!
//! RULE: a managed node runtime's standard library is part of what the runtime promises, so
//! every module of it is importable whatever capability it needs; a runtime that has none of
//! the host capability a module needs refuses the CALL that needs it, in that platform's own
//! failure, and never the import. Applied to the one capability no runtime of this emulator
//! has — starting a child process, whose Python half is this crate's `subprocess` — that
//! means `require('child_process')` loads, every entry point is there, and a call that would
//! start a child reports the failure node reports for a spawn that did not start (`ENOSYS`,
//! the number a system call that is not there answers with) through that entry point's own
//! channel, with no process id, exit status, signal or output invented for it.
//!
//! The rows differ along what the rule generalises over: WHICH entry point is called, whether
//! it is the synchronous or the asynchronous form, HOW each reports a child that did not start
//! (thrown, carried in a result, handed to a callback, emitted on the child), and which file
//! would have been executed (a command line's shell, a named shell, a program with arguments,
//! the interpreter a fork runs).
//!
//! The last test is the consequence the rule exists for: a handler shaped like the provider a
//! CDK custom resource deploys — module-scope child-process import, an attempt to install the
//! latest SDK, the fallback to the SDK the runtime itself supplies, and then the API call the
//! resource was declared for — runs to completion, and the write it made is read back through
//! the owning service's own API.

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and the one the function runs in.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` these functions carry: a managed node release, which is what supplies the
/// module under test.
const RUNTIME: &str = "nodejs22.x";

/// How often this suite looks again for a status the CLOCK owes, and how long it keeps looking
/// — generous, because what is waited for is a condition and not a span.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const ACTIVE_DEADLINE: Duration = Duration::from_secs(60);

/// The interpreter a managed node runtime runs, which is the file a `fork` would execute.
const INTERPRETER: &str = "/var/lang/bin/node";

/// The shell a command line is run by when the caller names no other, and the argument that
/// hands it the line.
const SHELL: &str = "/bin/sh";

/// Lambda and Amazon DynamoDB over ONE shared engine, as `core/router/src/lib.rs` mounts them:
/// the handler's SDK call is dispatched back into this same router, so the table it writes is
/// the table this suite reads.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Amazon DynamoDB call that must answer 200.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("authorization", authorization("dynamodb"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    assert_eq!(
        parts.status.as_u16(),
        200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {} with {parsed}",
        parts.status.as_u16()
    );
    parsed
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
        .header("authorization", authorization("lambda"))
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

/// A .zip deployment package holding `source` as `index.js` — one CommonJS module, and nothing
/// else: no `node_modules`, so every module it imports is the runtime's to supply.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer.start_file("index.js", options).expect("the entry starts");
        writer.write_all(source.as_bytes()).expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create a node function whose code is `source` and wait until Lambda reports it `Active` —
/// which is what a client's own waiter does: "you can't invoke or modify a function" until the
/// state the clock owes has arrived.
async fn create_function(router: &MultiServiceRouter, case: &str, name: &str, source: &str) {
    let (status, _headers, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": 20,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    let deadline = std::time::Instant::now() + ACTIVE_DEADLINE;
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
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `Invoke` the function synchronously with `event`, answering the `FunctionError` signal the
/// response carried and the payload the run produced.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    event: &Value,
) -> (Option<String>, Value) {
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
    (signal, payload)
}

/// The handler every row of the first table runs: its MODULE BODY imports the child-process
/// entry points, and the event names which one to call and how. What it reports is read off
/// the failure that entry point handed it and off the child object it was given — nothing
/// here states a code or a message of its own.
const ENTRY_POINT_HANDLER: &str = r#"
'use strict';

// At MODULE SCOPE, which is where a bundle states the import of the installer it carries: a
// runtime that supplied no such module would fail HERE, before the handler ever ran.
const child_process = require('child_process');
const {
    ChildProcess,
    exec,
    execFile,
    execFileSync,
    execSync,
    fork,
    spawn,
    spawnSync,
} = child_process;

function reported(error) {
    if (error === null || error === undefined) {
        return null;
    }
    return {
        message: error.message,
        code: error.code,
        errno: error.errno,
        syscall: error.syscall,
        path: error.path,
        spawnargs: error.spawnargs,
        isError: error instanceof Error,
    };
}

exports.handler = async (event) => {
    const answer = {
        loaded: true,
        names: {
            ChildProcess: typeof ChildProcess,
            exec: typeof exec,
            execFile: typeof execFile,
            execFileSync: typeof execFileSync,
            execSync: typeof execSync,
            fork: typeof fork,
            spawn: typeof spawn,
            spawnSync: typeof spawnSync,
        },
    };
    const options = event.options || {};
    if (event.form === 'names') {
        answer.outcome = 'names';
        return answer;
    }
    if (event.form === 'execSync' || event.form === 'execFileSync') {
        try {
            if (event.form === 'execSync') {
                execSync(event.command, options);
            } else {
                execFileSync(event.file, event.args, options);
            }
            answer.outcome = 'returned';
        } catch (error) {
            answer.outcome = 'threw';
            answer.reported = reported(error);
        }
        return answer;
    }
    if (event.form === 'spawnSync') {
        const result = spawnSync(event.file, event.args, options);
        answer.outcome = 'result';
        answer.status = result.status;
        answer.signal = result.signal;
        answer.stdout = result.stdout;
        answer.stderr = result.stderr;
        answer.pid = result.pid === undefined ? 'undefined' : result.pid;
        answer.reported = reported(result.error);
        return answer;
    }
    if (event.form === 'exec' || event.form === 'execFile') {
        answer.outcome = 'callback';
        await new Promise((resolve) => {
            const seen = (error, stdout, stderr) => {
                answer.reported = reported(error);
                answer.stdout = stdout;
                answer.stderr = stderr;
                resolve();
            };
            if (event.form === 'exec') {
                exec(event.command, options, seen);
            } else {
                execFile(event.file, event.args, options, seen);
            }
        });
        return answer;
    }
    answer.outcome = 'event';
    const child = event.form === 'fork'
        ? fork(event.file, event.args, options)
        : spawn(event.file, event.args, options);
    answer.pid = child.pid === undefined ? 'undefined' : child.pid;
    answer.streams = [child.stdin, child.stdout, child.stderr].every((held) => held === null);
    answer.spawnfile = child.spawnfile;
    answer.isChildProcess = child instanceof ChildProcess;
    await new Promise((resolve) => {
        child.on('error', (error) => {
            answer.reported = reported(error);
        });
        child.on('close', (code, signal) => {
            answer.closed = { code: code, signal: signal };
            resolve();
        });
    });
    answer.killed = child.kill();
    return answer;
};
"#;

/// One row of the first table: which entry point the handler calls, with what, how the failure
/// it reports is expected to arrive, and which file would have been executed.
struct Row {
    case: &'static str,
    form: &'static str,
    /// The command line a row that takes one states.
    command: &'static str,
    /// The file and arguments a row that names a program states.
    file: &'static str,
    args: &'static [&'static str],
    /// The options the row passes, as the handler is given them.
    options: Value,
    /// How the failure arrives: `threw`, `result`, `callback`, `event` — or `names` for the row
    /// that calls nothing at all.
    outcome: &'static str,
    /// The file the failure names as the one that would have been executed.
    path: &'static str,
    /// The arguments it names beside that file.
    spawnargs: &'static [&'static str],
    /// The syscall the failure names, which is also what opens its message.
    syscall: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the module's own names, with nothing called",
            form: "names",
            command: "",
            file: "",
            args: &[],
            options: json!({}),
            outcome: "names",
            path: "",
            spawnargs: &[],
            syscall: "",
        },
        // The call a bundled custom-resource provider makes: a command line, synchronously,
        // through the default shell.
        Row {
            case: "a command line run synchronously through the default shell",
            form: "execSync",
            command: "HOME=/tmp npm install @aws-sdk/client-dynamodb --prefix /tmp",
            file: "",
            args: &[],
            options: json!({}),
            outcome: "threw",
            path: SHELL,
            spawnargs: &["-c", "HOME=/tmp npm install @aws-sdk/client-dynamodb --prefix /tmp"],
            syscall: "spawnSync /bin/sh",
        },
        Row {
            case: "the same command line through the shell the options name",
            form: "execSync",
            command: "ls -l /tmp",
            file: "",
            args: &[],
            options: json!({ "shell": "/bin/bash" }),
            outcome: "threw",
            path: "/bin/bash",
            spawnargs: &["-c", "ls -l /tmp"],
            syscall: "spawnSync /bin/bash",
        },
        Row {
            case: "a program and its arguments, synchronously",
            form: "execFileSync",
            command: "",
            file: "/usr/bin/npm",
            args: &["install", "@aws-sdk/client-s3"],
            options: json!({}),
            outcome: "threw",
            path: "/usr/bin/npm",
            spawnargs: &["install", "@aws-sdk/client-s3"],
            syscall: "spawnSync /usr/bin/npm",
        },
        // The one synchronous entry point that does NOT throw: what did not start is carried
        // in the result it answers with.
        Row {
            case: "a program spawned synchronously, whose result carries the failure",
            form: "spawnSync",
            command: "",
            file: "npm",
            args: &["--version"],
            options: json!({}),
            outcome: "result",
            path: "npm",
            spawnargs: &["--version"],
            syscall: "spawnSync npm",
        },
        Row {
            case: "a command line with a callback",
            form: "exec",
            command: "npm --version",
            file: "",
            args: &[],
            options: json!({}),
            outcome: "callback",
            path: SHELL,
            spawnargs: &["-c", "npm --version"],
            syscall: "spawn /bin/sh",
        },
        Row {
            case: "a program with arguments and a callback",
            form: "execFile",
            command: "",
            file: "git",
            args: &["status", "--short"],
            options: json!({}),
            outcome: "callback",
            path: "git",
            spawnargs: &["status", "--short"],
            syscall: "spawn git",
        },
        Row {
            case: "a program spawned asynchronously, reported on the child itself",
            form: "spawn",
            command: "",
            file: "python3",
            args: &["-c", "print(1)"],
            options: json!({}),
            outcome: "event",
            path: "python3",
            spawnargs: &["-c", "print(1)"],
            syscall: "spawn python3",
        },
        // `options.shell` makes the program and its arguments ONE command line, so the shell
        // is the file that would have been executed.
        Row {
            case: "a program spawned asynchronously through a shell",
            form: "spawn",
            command: "",
            file: "echo",
            args: &["hello"],
            options: json!({ "shell": true }),
            outcome: "event",
            path: SHELL,
            spawnargs: &["-c", "echo hello"],
            syscall: "spawn /bin/sh",
        },
        // A fork runs the node interpreter itself on the module it names.
        Row {
            case: "a module forked, which would run the interpreter itself",
            form: "fork",
            command: "",
            file: "./worker.js",
            args: &["--once"],
            options: json!({}),
            outcome: "event",
            path: INTERPRETER,
            spawnargs: &["./worker.js", "--once"],
            syscall: "spawn /var/lang/bin/node",
        },
    ]
}

/// The failure of a spawn, as every row expects to read it: the code of a system call that is
/// not there, the negated number beside it, and the message node words such a failure with.
const CODE: &str = "ENOSYS";
const ERRNO: i64 = -38;

/// Every entry point of `child_process` is importable at module scope and reports a child that
/// did not start through its own channel — and nothing reports a child that ran.
#[tokio::test]
async fn every_child_process_entry_point_reports_a_child_that_did_not_start() {
    let router = router();
    let function = "child-process-caller";
    create_function(
        &router,
        "the module imported at module scope",
        function,
        ENTRY_POINT_HANDLER,
    )
    .await;
    for row in rows() {
        let case = row.case;
        let (signal, payload) = invoke(
            &router,
            case,
            function,
            &json!({
                "form": row.form,
                "command": row.command,
                "file": row.file,
                "args": row.args,
                "options": row.options,
            }),
        )
        .await;
        // The import itself is what must not fail: a runtime supplying no such module fails
        // the initialization phase instead of running the handler at all.
        assert_eq!(
            signal, None,
            "case {case:?}: expected the handler to run to completion, got {signal:?} with \
             {payload}"
        );
        assert_eq!(
            payload["loaded"],
            json!(true),
            "case {case:?}: expected the module to have been imported at module scope, got \
             {payload}"
        );
        for name in [
            "ChildProcess",
            "exec",
            "execFile",
            "execFileSync",
            "execSync",
            "fork",
            "spawn",
            "spawnSync",
        ] {
            assert_eq!(
                payload["names"][name],
                json!("function"),
                "case {case:?}: expected the module to export {name:?}, got {payload}"
            );
        }
        assert_eq!(
            payload["outcome"],
            json!(row.outcome),
            "case {case:?}: expected the failure to arrive as {:?}, got {payload}",
            row.outcome
        );
        if row.outcome == "names" {
            continue;
        }
        let reported = &payload["reported"];
        assert_eq!(
            reported["code"],
            json!(CODE),
            "case {case:?}: expected the code of a system call that is not there, got {payload}"
        );
        assert_eq!(
            reported["errno"],
            json!(ERRNO),
            "case {case:?}: expected the negated number beside the code, got {payload}"
        );
        assert_eq!(
            reported["syscall"],
            json!(row.syscall),
            "case {case:?}: expected the failing syscall to name the file, got {payload}"
        );
        assert_eq!(
            reported["message"],
            json!(format!("{} {CODE}", row.syscall)),
            "case {case:?}: expected the message node words a spawn failure with, got {payload}"
        );
        assert_eq!(
            reported["path"],
            json!(row.path),
            "case {case:?}: expected the file that would have been executed, got {payload}"
        );
        assert_eq!(
            reported["spawnargs"],
            json!(row.spawnargs),
            "case {case:?}: expected the arguments it would have been executed with, got \
             {payload}"
        );
        assert_eq!(
            reported["isError"],
            json!(true),
            "case {case:?}: expected the failure to be an Error, got {payload}"
        );
        match row.outcome {
            // Nothing ran, so there is no status, no signal and no output to report.
            "result" => {
                for absent in ["status", "signal", "stdout", "stderr"] {
                    assert_eq!(
                        payload[absent],
                        Value::Null,
                        "case {case:?}: expected no {absent} for a child that did not start, \
                         got {payload}"
                    );
                }
                assert_eq!(
                    payload["pid"],
                    json!("undefined"),
                    "case {case:?}: expected no process id, got {payload}"
                );
            }
            // The callback is handed the failure and the empty output of each stream, which
            // is what a node program reads off one.
            "callback" => {
                for empty in ["stdout", "stderr"] {
                    assert_eq!(
                        payload[empty],
                        json!(""),
                        "case {case:?}: expected no {empty} for a child that did not start, got \
                         {payload}"
                    );
                }
            }
            // A synchronous form throws, and the failure asserted above is the whole of what
            // it reports.
            "threw" => {}
            // The child emits the failure and then closes with no code and no signal, and
            // holds none of the three streams a started child has.
            _ => {
                assert_eq!(
                    payload["closed"],
                    json!({ "code": Value::Null, "signal": Value::Null }),
                    "case {case:?}: expected the child to close with no code and no signal, got \
                     {payload}"
                );
                assert_eq!(
                    payload["streams"],
                    json!(true),
                    "case {case:?}: expected a child that did not start to hold no streams, got \
                     {payload}"
                );
                assert_eq!(
                    payload["pid"],
                    json!("undefined"),
                    "case {case:?}: expected no process id, got {payload}"
                );
                assert_eq!(
                    payload["spawnfile"],
                    json!(row.path),
                    "case {case:?}: expected the child to report the file it was built for, got \
                     {payload}"
                );
                assert_eq!(
                    payload["isChildProcess"],
                    json!(true),
                    "case {case:?}: expected the child to be a ChildProcess, got {payload}"
                );
                assert_eq!(
                    payload["killed"],
                    json!(false),
                    "case {case:?}: nothing was started, so no signal is delivered, got \
                     {payload}"
                );
            }
        }
    }
}

/// The handler the second table runs: the shape a bundled custom-resource provider takes.
///
/// Its module body imports the child-process entry point its SDK installer uses, the handler
/// tries that install, falls back to the SDK the runtime itself supplies when the install is
/// refused, and then makes the API call the resource was declared for — reading the client's
/// own resolved configuration the way a v3 consumer reads it.
const PROVIDER_HANDLER: &str = r#"
'use strict';

const { execSync } = require('child_process');
const { existsSync } = require('fs');

// The SDK an installer-carrying provider calls with: the latest release where it could be
// installed, and the one the runtime supplies otherwise.
async function loadAwsSdk(packageName, installLatestAwsSdk, loader) {
    const load = (specifier) => (loader === 'require'
        ? Promise.resolve(require(specifier))
        : import(specifier));
    try {
        if (!installLatestAwsSdk) {
            return { sdk: await load(packageName), installed: 'not-asked-for' };
        }
        execSync(
            'HOME=/tmp npm install ' + packageName + ' --omit=dev --no-save --prefix /tmp'
        );
        return { sdk: await load('/tmp/node_modules/' + packageName), installed: 'installed' };
    } catch (error) {
        // The fallback the installer takes, which is why the import must not fail.
        return { sdk: await load(packageName), installed: String(error.code) };
    }
}

exports.handler = async (event) => {
    const loaded = await loadAwsSdk(event.package, event.installLatestAwsSdk, event.loader);
    const entries = Object.entries(loaded.sdk);
    const client = entries.find(([name]) => name.endsWith('Client') && name !== 'Client');
    const wanted = (event.action.charAt(0).toUpperCase() + event.action.slice(1) + 'Command')
        .toLowerCase();
    const command = entries.find(([name]) => name.toLowerCase() === wanted);
    const calling = new client[1]({ apiVersion: event.apiVersion, region: event.region });
    const answered = await calling.send(new command[1](event.parameters));
    return {
        installed: loaded.installed,
        installedPackage: existsSync('/tmp/node_modules/' + event.package),
        client: client[0],
        command: command[0],
        apiVersion: calling.config.apiVersion,
        regionProvider: typeof calling.config.region,
        region: await calling.config.region().catch(() => 'unresolved'),
        status: answered.$metadata.httpStatusCode,
    };
};
"#;

/// One row of the second table: whether the provider was asked to install the latest SDK, and
/// how it loads the package.
struct ProviderRow {
    case: &'static str,
    install_latest: bool,
    loader: &'static str,
    /// The key the item it writes carries, so each row's write is its own.
    key: &'static str,
    /// What the row expects the install attempt to have come to.
    installed: &'static str,
}

fn provider_rows() -> Vec<ProviderRow> {
    vec![
        ProviderRow {
            case: "a provider asked to install the latest SDK, loading with require",
            install_latest: true,
            loader: "require",
            key: "installed-require",
            installed: CODE,
        },
        ProviderRow {
            case: "a provider asked to install the latest SDK, loading with a dynamic import",
            install_latest: true,
            loader: "import",
            key: "installed-import",
            installed: CODE,
        },
        ProviderRow {
            case: "a provider that was not asked to install anything, loading with require",
            install_latest: false,
            loader: "require",
            key: "preinstalled-require",
            installed: "not-asked-for",
        },
        ProviderRow {
            case: "a provider that was not asked to install anything, loading with an import",
            install_latest: false,
            loader: "import",
            key: "preinstalled-import",
            installed: "not-asked-for",
        },
    ]
}

/// The table the provider-shaped handler seeds, as its own declaration states it.
const TABLE: &str = "child-process-provider-items";

/// A handler shaped like a bundled custom-resource provider — module-scope child-process
/// import, an install attempt, the fallback to the runtime's own SDK — runs to completion and
/// the write it makes is held by the owning service.
#[tokio::test]
async fn a_provider_shaped_handler_falls_back_to_the_runtime_sdk_and_writes_through_it() {
    let router = router();
    let case = "the provider-shaped handler";
    dynamodb(
        &router,
        case,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let function = "child-process-provider";
    create_function(&router, case, function, PROVIDER_HANDLER).await;
    // "You can perform read and write operations only on an ACTIVE table", so the write waits
    // for the status the clock owes rather than racing it.
    let deadline = std::time::Instant::now() + ACTIVE_DEADLINE;
    loop {
        let described = dynamodb(&router, case, "DescribeTable", &json!({ "TableName": TABLE })).await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the table never became ACTIVE: {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    for row in provider_rows() {
        let case = row.case;
        let (signal, payload) = invoke(
            &router,
            case,
            function,
            &json!({
                "package": "@aws-sdk/client-dynamodb",
                "installLatestAwsSdk": row.install_latest,
                "loader": row.loader,
                "action": "putItem",
                "apiVersion": "2012-08-10",
                "region": REGION,
                "parameters": {
                    "TableName": TABLE,
                    "Item": {
                        "id": { "S": row.key },
                        "status": { "S": "active" },
                    },
                },
            }),
        )
        .await;
        assert_eq!(
            signal, None,
            "case {case:?}: expected the provider-shaped handler to run to completion, got \
             {signal:?} with {payload}"
        );
        assert_eq!(
            payload["installed"],
            json!(row.installed),
            "case {case:?}: expected the install attempt to come to {:?}, got {payload}",
            row.installed
        );
        // Nothing was installed, so nothing is there: the fallback is the runtime's own SDK.
        assert_eq!(
            payload["installedPackage"],
            json!(false),
            "case {case:?}: nothing was installed, so no package is on the filesystem, got \
             {payload}"
        );
        assert_eq!(
            payload["client"],
            json!("DynamoDBClient"),
            "case {case:?}: expected the client the package exports, got {payload}"
        );
        assert_eq!(
            payload["command"],
            json!("PutItemCommand"),
            "case {case:?}: expected the command class of the declared action, got {payload}"
        );
        // A v3 client's resolved configuration: the options it was built with, with the
        // Region normalised into the provider `await client.config.region()` reads.
        assert_eq!(
            payload["apiVersion"],
            json!("2012-08-10"),
            "case {case:?}: expected the client to report the apiVersion it was built with, got \
             {payload}"
        );
        assert_eq!(
            payload["regionProvider"],
            json!("function"),
            "case {case:?}: expected config.region to be the provider a v3 client normalises it \
             into, got {payload}"
        );
        assert_eq!(
            payload["region"],
            json!(REGION),
            "case {case:?}: expected the provider to answer the Region the client was built \
             with, got {payload}"
        );
        assert_eq!(
            payload["status"],
            json!(200),
            "case {case:?}: expected the API call to have been answered, got {payload}"
        );
        // What the TABLE holds, read where a client reads it: a handler that answered without
        // doing the work fails here.
        let read = dynamodb(
            &router,
            case,
            "GetItem",
            &json!({ "TableName": TABLE, "Key": { "id": { "S": row.key } } }),
        )
        .await;
        assert_eq!(
            read["Item"]["status"],
            json!({ "S": "active" }),
            "case {case:?}: expected the item the provider wrote to be on the table, got {read}"
        );
    }
}
