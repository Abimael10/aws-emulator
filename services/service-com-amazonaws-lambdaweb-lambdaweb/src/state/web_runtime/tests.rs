// SPDX-License-Identifier: CC0-1.0

//! What the engine of a web function run answers, and what bounds and contains it.
//!
//! Every case drives [`super::run_bounded`] — the ONE path a request to a published
//! endpoint takes — over a package staged as the registry holds one, so a rule proved here
//! is the rule the data plane applies.

use std::collections::BTreeMap;
use std::time::Duration;

use super::super::code_package::DeploymentPackage;
use super::{run_bounded, Bounds, HandlerOutcome, HandlerRequest, RUN_ENGINE_CHECKS};

/// The runtime identifier the cases that go through [`super::serve_handler`] run under, as a
/// build configuration names one.
const RUNTIME: &str = "nodejs24.x";

/// A package of the files `entries`.
fn package(entries: &[(&str, &str)]) -> DeploymentPackage {
    DeploymentPackage {
        files: entries
            .iter()
            .map(|(path, source)| ((*path).to_string(), (*source).to_string()))
            .collect(),
    }
}

/// A `GET` of `url`, as a client sends one to a published address.
fn get(url: &str) -> HandlerRequest {
    HandlerRequest {
        method: "GET".to_string(),
        url: url.to_string(),
        headers: vec![("host".to_string(), "127.0.0.1:8080".to_string())],
        body: String::new(),
    }
}

/// Run `files` against `request` with the budget one request has.
fn served(files: &[(&str, &str)], request: &HandlerRequest) -> HandlerOutcome {
    run_bounded(
        &package(files),
        &BTreeMap::new(),
        "24.0.0",
        Bounds::new(Duration::from_secs(5), RUN_ENGINE_CHECKS),
        request,
    )
}

/// The status and body of an answer, or a panic naming what the run came to instead.
fn answered(case: &str, outcome: &HandlerOutcome) -> (u16, String) {
    match outcome {
        HandlerOutcome::Answered(answer) => (
            answer.status,
            String::from_utf8_lossy(&answer.body).to_string(),
        ),
        other => panic!("case {case:?}: expected the web function to answer, got {other:?}"),
    }
}

/// One row of the served table: the case, the package's files, and the status and body the
/// code answers with.
type ServedRow<'a> = (&'a str, Vec<(&'a str, &'a str)>, u16, &'a str);

/// One row of the no-response table: the case, the package's files, and what the outcome's
/// clause must name.
type NoResponseRow<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str);

/// RULE: what a client is served at an endpoint's address is the response the revision's
/// own code produced — its status, its headers and its body — whatever module form the
/// package ships that code in and however the code answers.
///
/// The rows span the forms a Node.js package takes: a CommonJS entry, an ES module entry, an
/// entry named by `package.json`'s `main`, a `require` of a sibling file, an `import` of
/// one, a bundled dependency under `node_modules`, a JSON import, an asynchronous handler
/// and a handler that answers from a timer.
#[test]
fn a_request_is_answered_by_the_code_the_package_ships() {
    // (case, the package's files, the expected status, the expected body)
    let rows: Vec<ServedRow<'_>> = vec![
        (
            "a CommonJS entry",
            vec![(
                "index.js",
                "const http = require('http');\n\
                 http.createServer((req, res) => {\n\
                   res.writeHead(200, { 'content-type': 'text/plain' });\n\
                   res.end('hello from commonjs');\n\
                 }).listen(8080);\n",
            )],
            200,
            "hello from commonjs",
        ),
        (
            "an ES module entry",
            vec![(
                "index.mjs",
                "import http from 'node:http';\n\
                 http.createServer((req, res) => {\n\
                   res.statusCode = 201;\n\
                   res.end('hello from esm');\n\
                 }).listen(8080);\n",
            )],
            201,
            "hello from esm",
        ),
        (
            "an entry named by package.json main",
            vec![
                ("package.json", "{\"main\": \"server/app.js\"}"),
                (
                    "server/app.js",
                    "require('http').createServer((req, res) => res.end('from main')).listen(3000);",
                ),
            ],
            200,
            "from main",
        ),
        (
            "a .js entry read as an ES module because the package says so",
            vec![
                ("package.json", "{\"type\": \"module\"}"),
                (
                    "index.js",
                    "import { createServer } from 'http';\n\
                     createServer((req, res) => res.end('typed module')).listen(8080);\n",
                ),
            ],
            200,
            "typed module",
        ),
        (
            "a require of a sibling file",
            vec![
                (
                    "index.js",
                    "const { greet } = require('./lib/greet');\n\
                     require('http').createServer((req, res) => res.end(greet())).listen(8080);\n",
                ),
                (
                    "lib/greet.js",
                    "exports.greet = () => 'from a sibling';",
                ),
            ],
            200,
            "from a sibling",
        ),
        (
            "an import of a sibling ES module",
            vec![
                (
                    "index.mjs",
                    "import http from 'http';\n\
                     import { greet } from './lib/greet.mjs';\n\
                     http.createServer((req, res) => res.end(greet())).listen(8080);\n",
                ),
                (
                    "lib/greet.mjs",
                    "export const greet = () => 'from an imported module';",
                ),
            ],
            200,
            "from an imported module",
        ),
        (
            "an import of a CommonJS sibling",
            vec![
                (
                    "index.mjs",
                    "import http from 'http';\n\
                     import greeter from './lib/greet.cjs';\n\
                     http.createServer((req, res) => res.end(greeter.greet())).listen(8080);\n",
                ),
                ("lib/greet.cjs", "module.exports.greet = () => 'from cjs';"),
            ],
            200,
            "from cjs",
        ),
        (
            "a bundled dependency under node_modules",
            vec![
                (
                    "index.js",
                    "const { body } = require('tiny-app');\n\
                     require('http').createServer((req, res) => res.end(body)).listen(8080);\n",
                ),
                (
                    "node_modules/tiny-app/package.json",
                    "{\"main\": \"lib/index.js\"}",
                ),
                (
                    "node_modules/tiny-app/lib/index.js",
                    "module.exports = { body: 'from a dependency' };",
                ),
            ],
            200,
            "from a dependency",
        ),
        (
            "a JSON file the code reads",
            vec![
                (
                    "index.js",
                    "const held = require('./data.json');\n\
                     require('http').createServer((req, res) => res.end(held.body)).listen(8080);\n",
                ),
                ("data.json", "{\"body\": \"from json\"}"),
            ],
            200,
            "from json",
        ),
        (
            "an asynchronous handler",
            vec![(
                "index.js",
                "const http = require('http');\n\
                 const later = () => new Promise((resolve) => resolve('after a promise'));\n\
                 http.createServer(async (req, res) => {\n\
                   res.end(await later());\n\
                 }).listen(8080);\n",
            )],
            200,
            "after a promise",
        ),
        (
            "a handler that answers from a timer",
            vec![(
                "index.js",
                "require('http').createServer((req, res) => {\n\
                   setTimeout(() => res.end('after a timer'), 1500);\n\
                 }).listen(8080);\n",
            )],
            200,
            "after a timer",
        ),
        (
            "a server registered from a settled promise",
            vec![(
                "index.mjs",
                "const http = await import('node:http');\n\
                 http.default.createServer((req, res) => res.end('late server')).listen(8080);\n",
            )],
            200,
            "late server",
        ),
    ];
    for (case, files, status, body) in rows {
        let outcome = served(&files, &get("/"));
        let (answered_status, answered_body) = answered(case, &outcome);
        assert_eq!(
            (answered_status, answered_body.as_str()),
            (status, body),
            "case {case:?}: expected {status} {body:?}, got {answered_status} {answered_body:?}"
        );
    }
}

/// RULE: the request the code is handed is the request the client sent — its method, its
/// path and query, its headers and its body — so a handler that answers from them answers
/// about that request.
#[test]
fn the_code_is_handed_the_request_the_client_sent() {
    let files = [(
        "index.js",
        "const http = require('http');\n\
         http.createServer((req, res) => {\n\
           let body = '';\n\
           req.on('data', (chunk) => { body += chunk; });\n\
           req.on('end', () => {\n\
             res.setHeader('content-type', 'application/json');\n\
             res.end(JSON.stringify({ method: req.method, url: req.url,\n\
                                      host: req.headers.host, sent: body }));\n\
           });\n\
         }).listen(8080);\n",
    )];
    // (case, the method, the path and query, the body)
    let rows: [(&str, &str, &str, &str); 5] = [
        ("a GET of the root", "GET", "/", ""),
        ("a GET deeper in the path", "GET", "/items/42", ""),
        ("a GET with a query", "GET", "/items?page=2&q=a+b", ""),
        ("a POST with a body", "POST", "/orders", "{\"id\":7}"),
        ("a DELETE", "DELETE", "/sessions/9", ""),
    ];
    for (case, method, url, body) in rows {
        let request = HandlerRequest {
            method: method.to_string(),
            url: url.to_string(),
            headers: vec![("host".to_string(), "127.0.0.1:8080".to_string())],
            body: body.to_string(),
        };
        let outcome = served(&files, &request);
        let (status, answered_body) = answered(case, &outcome);
        let read: ::serde_json::Value =
            ::serde_json::from_str(&answered_body).unwrap_or_default();
        let member = |name: &str| {
            read.get(name)
                .and_then(::serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status}");
        assert_eq!(
            (
                member("method"),
                member("url"),
                member("host"),
                member("sent")
            ),
            (
                method.to_string(),
                url.to_string(),
                "127.0.0.1:8080".to_string(),
                body.to_string()
            ),
            "case {case:?}: the handler saw {read:?}"
        );
    }
}

/// RULE: the headers the code set are the headers the client is served, in the order it set
/// them, and a header it replaced is served once.
#[test]
fn the_headers_the_code_set_are_the_headers_served() {
    let outcome = served(
        &[(
            "index.js",
            "require('http').createServer((req, res) => {\n\
               res.setHeader('x-first', 'one');\n\
               res.setHeader('X-Second', 'two');\n\
               res.setHeader('x-first', 'replaced');\n\
               res.end('headers');\n\
             }).listen(8080);\n",
        )],
        &get("/"),
    );
    let HandlerOutcome::Answered(answer) = outcome else {
        panic!("expected the web function to answer, got {outcome:?}");
    };
    assert_eq!(
        answer.headers,
        vec![
            ("X-Second".to_string(), "two".to_string()),
            ("x-first".to_string(), "replaced".to_string()),
        ],
        "expected the headers the code set, got {:?}",
        answer.headers
    );
}

/// RULE: the environment variables a revision configured are what the code reads, nothing of
/// the process this emulator runs in is reachable through them, and what the code reads about
/// the process it IS — its version, its architecture and the module it was started on — is
/// what this runtime was told rather than a fixed value.
#[test]
fn the_code_reads_the_revisions_own_environment() {
    let mut environment = BTreeMap::new();
    environment.insert("STAGE".to_string(), "beta".to_string());
    environment.insert("GREETING".to_string(), "hei".to_string());
    let outcome = run_bounded(
        &package(&[
            ("package.json", "{\"main\": \"src/server.js\"}"),
            (
                "src/server.js",
                "require('http').createServer((req, res) => {\n\
                   res.end(JSON.stringify({ stage: process.env.STAGE,\n\
                                            greeting: process.env.GREETING,\n\
                                            path: process.env.PATH === undefined,\n\
                                            home: process.env.HOME === undefined,\n\
                                            version: process.version,\n\
                                            arch: process.arch,\n\
                                            started: process.argv[1] }));\n\
                 }).listen(8080);\n",
            ),
        ]),
        &environment,
        "24.0.0",
        Bounds::new(Duration::from_secs(5), RUN_ENGINE_CHECKS),
        &get("/"),
    );
    let (_, body) = answered("the revision's environment", &outcome);
    let read: ::serde_json::Value = ::serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(
        read,
        ::serde_json::json!({
            "stage": "beta",
            "greeting": "hei",
            "path": true,
            "home": true,
            "version": "v24.0.0",
            // The architecture Node itself reports, which is not the machine's own spelling
            // of it.
            "arch": "x64",
            // The module Node was pointed at, which is the entry this package resolved to.
            "started": "/var/task/src/server.js",
        }),
        "expected the revision's own environment, got {read:?}"
    );
}

/// RULE: a run that produces no response is reported as the run it was, and never as a
/// response — a package that registers no server, code that raises, code that answers
/// nothing, and a module that is not there.
#[test]
fn a_run_that_answers_nothing_is_reported_as_what_it_was() {
    // (case, the package's files, what the outcome's clause must name)
    let rows: Vec<NoResponseRow<'_>> = vec![
        (
            "a package that registers no server",
            vec![("index.js", "const held = 1 + 1;")],
            "registered no HTTP server",
        ),
        (
            "code that raises while starting",
            vec![("index.js", "throw new Error('no good');")],
            "no good",
        ),
        (
            "an ES module that raises while starting",
            vec![("index.mjs", "throw new Error('no good either');")],
            "no good either",
        ),
        (
            "a handler that raises",
            vec![(
                "index.js",
                "require('http').createServer(() => { throw new Error('handler failed'); })\n\
                   .listen(8080);\n",
            )],
            "handler failed",
        ),
        (
            "a handler that answers nothing",
            vec![(
                "index.js",
                "require('http').createServer((req, res) => { res.statusCode = 204; })\n\
                   .listen(8080);\n",
            )],
            "did not answer the request",
        ),
        (
            "a package with no entry module",
            vec![("readme.md", "no code here")],
            "no module to start",
        ),
        (
            "a require of a module that is not there",
            vec![("index.js", "require('./missing');")],
            "Cannot find module",
        ),
    ];
    for (case, files, expected) in rows {
        let outcome = served(&files, &get("/"));
        let HandlerOutcome::NoResponse(reason) = &outcome else {
            panic!("case {case:?}: expected no response, got {outcome:?}");
        };
        assert!(
            reason.contains(expected),
            "case {case:?}: expected a clause naming {expected:?}, got {reason:?}"
        );
    }
}

/// RULE: the host reaches nothing from inside a run — no filesystem, no socket, no child
/// process, no host thread and no host library — so the modules that would open one are not
/// there at all, and a run that asks for one is refused from inside the engine.
///
/// Asserted FROM GUEST CODE: each row is the code's own attempt, and the answer it serves is
/// the refusal it got.
#[test]
fn no_host_surface_is_reachable_from_a_run() {
    // The module names Node registers for the host surfaces this runtime deliberately does
    // not implement.
    let refused = [
        "fs",
        "node:fs",
        "net",
        "child_process",
        "node:child_process",
        "worker_threads",
        "vm",
        "dgram",
        "tls",
        "cluster",
        "module",
        "inspector",
        "v8",
        "repl",
    ];
    for name in refused {
        let source = format!(
            "require('http').createServer((req, res) => {{\n\
               try {{ require('{name}'); res.end('reached'); }}\n\
               catch (error) {{ res.end('refused:' + error.code); }}\n\
             }}).listen(8080);\n"
        );
        let outcome = served(&[("index.js", source.as_str())], &get("/"));
        let (_, body) = answered(name, &outcome);
        assert_eq!(
            body, "refused:MODULE_NOT_FOUND",
            "case {name:?}: the module must not be reachable from a run, got {body:?}"
        );
    }
}

/// RULE: the host functions the runtime is built over are not reachable from the guest's
/// code: the response sink and the log writer are taken off the global object before any of
/// the package's code is compiled, so a package looking for them finds nothing.
#[test]
fn the_host_functions_are_not_reachable_from_a_run() {
    let outcome = served(
        &[(
            "index.js",
            "require('http').createServer((req, res) => {\n\
               const held = Object.keys(globalThis).filter((name) => /^__lambdaWebHost/.test(name));\n\
               res.end(JSON.stringify({ held, respond: typeof globalThis.__lambdaWebHostRespond,\n\
                                        log: typeof globalThis.__lambdaWebHostLog }));\n\
             }).listen(8080);\n",
        )],
        &get("/"),
    );
    let (_, body) = answered("the host seams", &outcome);
    let read: ::serde_json::Value = ::serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(
        read,
        ::serde_json::json!({ "held": [], "respond": "undefined", "log": "undefined" }),
        "no host function may be reachable from a run, got {read:?}"
    );
}

/// RULE: a run is stopped by the bounds this module holds — its wall clock and its step
/// budget — and the stop is LATCHED, so code that catches what the engine raises in it
/// cannot make the run report a response.
#[test]
fn a_run_past_its_bounds_is_stopped_whatever_the_code_catches() {
    // (case, the package's files, the wall clock, the step budget, what the stop must name)
    let rows: Vec<(&str, &str, Duration, u64, &str)> = vec![
        (
            "a handler that never returns",
            "require('http').createServer(() => { for (;;) { } }).listen(8080);",
            Duration::from_millis(250),
            RUN_ENGINE_CHECKS,
            "seconds without answering",
        ),
        (
            "a handler that catches the stop and answers anyway",
            "require('http').createServer((req, res) => {\n\
               try { for (;;) { } } catch (error) { res.end('caught'); }\n\
             }).listen(8080);",
            Duration::from_millis(250),
            RUN_ENGINE_CHECKS,
            "without answering",
        ),
        (
            "code that loops while the package is starting",
            "for (;;) { }",
            Duration::from_millis(250),
            RUN_ENGINE_CHECKS,
            "without answering",
        ),
        (
            "a step budget spent before the clock",
            "require('http').createServer(() => { for (;;) { } }).listen(8080);",
            Duration::from_secs(600),
            4_096,
            "engine steps without answering",
        ),
    ];
    for (case, source, clock, budget, expected) in rows {
        let outcome = run_bounded(
            &package(&[("index.js", source)]),
            &BTreeMap::new(),
            "24.0.0",
            Bounds::new(clock, budget),
            &get("/"),
        );
        let HandlerOutcome::Stopped(reason) = &outcome else {
            panic!("case {case:?}: expected the run to be stopped, got {outcome:?}");
        };
        assert!(
            reason.contains(expected),
            "case {case:?}: expected a stop naming {expected:?}, got {reason:?}"
        );
    }
}

/// RULE: a run cannot outlast its bounds by waiting, because nothing a guest can reach
/// sleeps: its timers run on its own clock, so a handler that waits an hour before
/// answering costs the client no wall-clock time at all.
#[test]
fn a_waiting_run_spends_the_guests_clock_and_not_the_hosts() {
    let outcome = run_bounded(
        &package(&[(
            "index.js",
            "require('http').createServer((req, res) => {\n\
               setTimeout(() => res.end('an hour later'), 3600000);\n\
             }).listen(8080);\n",
        )]),
        &BTreeMap::new(),
        "24.0.0",
        // A clock far shorter than the wait the guest asked for: the run still answers,
        // because the guest's wait is virtual and nothing of the host's clock is spent on
        // it.
        Bounds::new(Duration::from_secs(5), RUN_ENGINE_CHECKS),
        &get("/"),
    );
    let (status, body) = answered("a virtual wait", &outcome);
    assert_eq!(
        (status, body.as_str()),
        (200, "an hour later"),
        "expected the handler's own answer, got {status} {body:?}"
    );
}

/// RULE: the response one request may answer with is capped, and a response over the cap is
/// not served at all — a PREFIX of what the code wrote is not the response it wrote, so the
/// run is reported as having produced none.
#[test]
fn a_response_past_the_ceiling_is_not_served() {
    let outcome = served(
        &[(
            "index.js",
            "require('http').createServer((req, res) => {\n\
               let chunk = 'x'.repeat(1024 * 1024);\n\
               for (let i = 0; i < 16; i += 1) { res.write(chunk); }\n\
               res.end();\n\
             }).listen(8080);\n",
        )],
        &get("/"),
    );
    let HandlerOutcome::NoResponse(reason) = &outcome else {
        panic!("expected the oversize response not to be served, got {outcome:?}");
    };
    assert!(
        reason.contains(&super::MAX_RESPONSE_BYTES.to_string()),
        "expected a clause naming the {} byte ceiling, got {reason:?}",
        super::MAX_RESPONSE_BYTES
    );
}

/// RULE: a response WITHIN the ceiling is served whole, so the cap refuses only what is over
/// it.
#[test]
fn a_response_within_the_ceiling_is_served_whole() {
    let outcome = served(
        &[(
            "index.js",
            "require('http').createServer((req, res) => {\n\
               res.end('y'.repeat(64 * 1024));\n\
             }).listen(8080);\n",
        )],
        &get("/"),
    );
    let (status, body) = answered("a response within the ceiling", &outcome);
    assert_eq!(
        (status, body.len()),
        (200, 64 * 1024),
        "expected the whole body the code wrote, got {status} and {} bytes",
        body.len()
    );
}

/// RULE: a run that answered twice answered once — the first response is the one the client
/// reads, as a server that has already flushed its headers cannot take them back.
#[test]
fn a_run_that_answers_twice_serves_its_first_answer() {
    let outcome = served(
        &[(
            "index.js",
            "require('http').createServer((req, res) => {\n\
               res.end('first');\n\
               res.finished = false;\n\
               res.end('second');\n\
             }).listen(8080);\n",
        )],
        &get("/"),
    );
    let (status, body) = answered("two answers", &outcome);
    assert_eq!(
        (status, body.as_str()),
        (200, "first"),
        "expected the first answer, got {status} {body:?}"
    );
}

/// RULE: the GUEST's timeout is the revision's own `timeoutSeconds`, clamped to the range
/// the model allows (3..900) and to this module's own ceiling, so a caller-chosen timeout
/// decides when the code is stopped and never how long this process holds a thread.
///
/// The rows span the published bounds and one past each of them.
#[test]
fn a_runs_clock_is_the_configured_timeout_within_the_modelled_range() {
    // (case, the stored timeoutSeconds, the clock the run gets)
    let rows: [(&str, i32, Duration); 7] = [
        ("the modelled default", 30, super::RUN_WALL_CLOCK_CEILING),
        ("the smallest value the model allows", 3, Duration::from_secs(3)),
        ("the largest value the model allows", 900, super::RUN_WALL_CLOCK_CEILING),
        ("a value under the modelled range", 0, Duration::from_secs(3)),
        ("a negative value", -5, Duration::from_secs(3)),
        ("a value over the modelled range", 10_000, super::RUN_WALL_CLOCK_CEILING),
        ("a value under this module's ceiling", 5, Duration::from_secs(5)),
    ];
    for (case, configured, expected) in rows {
        let clock = super::run_clock(configured);
        assert_eq!(
            clock, expected,
            "case {case:?}: expected a clock of {expected:?}, got {clock:?}"
        );
    }
}

/// RULE: this emulator runs an engine for the Node.js runtime family and says so for any
/// other — a revision naming a runtime nothing here executes reports that nothing ran,
/// rather than a response nothing produced.
#[tokio::test]
async fn only_a_runtime_this_emulator_runs_an_engine_for_is_run() {
    // (case, the runtime identifier, whether a run happens)
    let rows: [(&str, &str, bool); 6] = [
        ("a Node.js runtime", "nodejs24.x", true),
        ("an older Node.js runtime", "nodejs18.x", true),
        ("a Python runtime", "python3.13", false),
        ("a Java runtime", "java21", false),
        ("a runtime naming no version", "nodejs", false),
        ("a provided runtime", "provided.al2023", false),
    ];
    let files = package(&[(
        "index.js",
        "require('http').createServer((req, res) => res.end('ran')).listen(8080);",
    )]);
    for (case, runtime, runs) in rows {
        let outcome = super::serve_handler(
            ::std::sync::Arc::new(files.clone()),
            BTreeMap::new(),
            3,
            runtime.to_string(),
            get("/"),
        )
        .await;
        let answered = matches!(&outcome, HandlerOutcome::Answered(answer) if answer.body == b"ran");
        assert_eq!(
            answered, runs,
            "case {case:?}: expected a run {runs}, got {outcome:?}"
        );
        if !runs {
            let HandlerOutcome::Unavailable(reason) = &outcome else {
                panic!("case {case:?}: expected nothing to have run, got {outcome:?}");
            };
            assert!(
                reason.contains("no engine"),
                "case {case:?}: expected a clause naming the missing engine, got {reason:?}"
            );
        }
    }
}

/// RULE: the version the runtime identifier names is the one the code reads, and an
/// identifier that names no Node.js release names no engine.
#[test]
fn the_runtime_identifier_states_the_release() {
    // (case, the identifier, the release it names)
    let rows: [(&str, &str, Option<&str>); 6] = [
        ("a major-line identifier", "nodejs24.x", Some("24.0.0")),
        ("another major line", "nodejs18.x", Some("18.0.0")),
        ("a two-part line", "nodejs8.10", Some("8.10.0")),
        ("no version at all", "nodejs", None),
        ("another language", "python3.13", None),
        ("a provided runtime", "provided.al2023", None),
    ];
    for (case, runtime, expected) in rows {
        let named = super::node_version(runtime);
        assert_eq!(
            named.as_deref(),
            expected,
            "case {case:?}: expected {expected:?}, got {named:?}"
        );
    }
}

/// RULE: what the runs of this process hold at once is bounded by a count of their THREADS,
/// taken before a run starts and given back where its thread ends — so a run that was given
/// up on holds exactly one place for as long as it lasts, a request that finds every place
/// live is refused rather than started, and a place is never leaked on any exit path.
#[test]
fn a_runs_place_is_held_by_its_thread_and_given_back_when_it_ends() {
    let admission = ::std::sync::Arc::new(super::RunAdmission::new(2));
    assert_eq!(admission.live(), 0, "a fresh admission holds no run");

    let first = admission.admit();
    let second = admission.admit();
    assert!(
        first.is_some() && second.is_some(),
        "the places under the ceiling must be admitted"
    );
    assert_eq!(
        admission.live(),
        2,
        "expected both places held, got {}",
        admission.live()
    );
    assert!(
        admission.admit().is_none(),
        "a run past the ceiling must be refused rather than started"
    );

    // A run whose thread ended gives its place back, and the next run takes it.
    drop(first);
    assert_eq!(
        admission.live(),
        1,
        "expected the ended run's place to be back, got {}",
        admission.live()
    );
    let third = admission.admit();
    assert!(
        third.is_some(),
        "the place an ended run gave back must be admitted again"
    );
    assert!(
        admission.admit().is_none(),
        "the ceiling still holds once the freed place is taken"
    );
    drop(second);
    drop(third);
    assert_eq!(
        admission.live(),
        0,
        "every place must be back once every run has ended, got {}",
        admission.live()
    );
}

/// RULE: a run is SERVED under its own ceiling, and the two bounds are separate — the
/// serving slot is held only while a caller is being served, so a run that was given up on
/// does not hold one.
#[tokio::test]
async fn a_served_run_holds_its_slot_only_while_the_caller_is_served() {
    let files = package(&[(
        "index.js",
        "require('http').createServer((req, res) => res.end('served')).listen(8080);",
    )]);
    // Every run here answers, so the slot each took is back before the next one starts: more
    // runs than the ceiling are served one after another.
    for round in 0..(super::MAX_LIVE_RUNS + 2) {
        let outcome = super::serve_handler(
            ::std::sync::Arc::new(files.clone()),
            BTreeMap::new(),
            3,
            RUNTIME.to_string(),
            get("/"),
        )
        .await;
        let HandlerOutcome::Answered(answer) = &outcome else {
            panic!("round {round}: expected the web function to answer, got {outcome:?}");
        };
        assert_eq!(
            answer.body, b"served",
            "round {round}: expected the code's own answer, got {outcome:?}"
        );
    }
}
