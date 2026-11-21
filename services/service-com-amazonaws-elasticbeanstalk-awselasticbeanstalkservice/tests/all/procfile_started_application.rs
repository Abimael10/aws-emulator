// SPDX-License-Identifier: CC0-1.0

//! The application a bundle's `Procfile` starts, over the service's own HTTP
//! surface.
//!
//! The rule under test: an Elastic Beanstalk platform starts the application with
//! the command the source bundle's `Procfile` gives its `web` process, where the
//! bundle carries one, instead of with the platform's own default start command —
//! so a bundle whose web command RUNS A PROGRAM has that program run, and the
//! server that program starts for itself is what answers the address the
//! environment publishes and what writes the output the environment streams to
//! CloudWatch Logs.
//!
//! The table spans what that rule generalises over: the form of the command (the
//! interpreter over a file, with and without an option of its own, and the module
//! form `-m`), the server the program starts (the standard library's own HTTP
//! server and its WSGI server), the command that names a WSGI application for a
//! server to serve rather than a program to run, the command of another platform's
//! runtime, a command this emulator starts nothing from, and a `Procfile` that
//! states no web process at all — which leaves the platform's default command
//! starting the application.
//!
//! The harness (the router, the form encoder, the XML element reader, the
//! object-storage upload and the unsigned GET) is the one `lifecycle.rs` owns in
//! this same test binary, and the CloudWatch Logs reader is the one
//! `deployed_application.rs` owns beside it.

use std::time::{Duration, Instant};

use crate::application_version_processing::zip_of;
use crate::deployed_application::{logs, solution_stack_of};
use crate::lifecycle::{wait_for_status, Answer, Emulator, POLL_INTERVAL, TEST_REGION};

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// The application every environment below is launched under, and the version it
/// deploys.
const APPLICATION: &str = "procfile-app";
const VERSION: &str = "v1";

/// The bucket the suites upload a source bundle into, and the key under it.
const BUNDLE_BUCKET: &str = "eb-procfile-bundles";
const BUNDLE_KEY: &str = "bundles/app.zip";

/// The environment property every launch below configures, and its value: what the
/// deployed code reads out of its own process environment and answers with, so the
/// body a client reads is one the program computed rather than one this emulator
/// could have written.
const PROPERTY_NAME: &str = "GREETING";
const PROPERTY_VALUE: &str = "hello from the environment";

/// The line every program below writes to its own standard output while starting,
/// which is what an instance's `/var/log/web.stdout.log` carries.
const LOG_MARKER: &str = "started: the Procfile's own web process";

/// The line every application below writes while SERVING one request, which is
/// what that file gains from that request.
const SERVED_MARKER: &str = "served: /";

/// How long a suite waits for the address to be served by a RUN of the deployed
/// program.
///
/// Generous, and waited out by LOOKING AGAIN rather than by sleeping: one run
/// imports the standard library the program's server is built on into a fresh
/// interpreter, and a request may wait for another run already in flight, so the
/// condition is polled until it holds.
const SERVED_DEADLINE: Duration = Duration::from_secs(120);

/// The statuses the plane RENDERS ITSELF for a request no run of the application
/// answered: an address with nothing behind it, and one whose serving did not
/// finish inside the span the socket allows. Neither is an answer of the
/// application's, so a suite polling for the program's own answer looks again
/// past them.
const NOT_THE_APPLICATIONS: &[u16] = &[502, 504];

/// How long the delivery of a program's output into its log group is waited for,
/// the same way: by reading the group again until it carries an event.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// A program that starts the standard library's own HTTP server and answers every
/// request from its handler.
const HTTP_SERVER_PROGRAM: &[u8] = br#"
import os
from http.server import BaseHTTPRequestHandler, HTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        path, _, query = self.path.partition("?")
        print("served: %s" % path, flush=True)
        if path not in ("/", "/items"):
            self.answer(404, ("no such path: %s" % path).encode("utf-8"))
            return
        greeting = os.environ.get("GREETING", "")
        self.answer(200, ("%s at %s query=%s" % (greeting, path, query)).encode("utf-8"))

    def answer(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


print("started: the Procfile's own web process", flush=True)
HTTPServer(("", int(os.environ.get("PORT", "8000"))), Handler).serve_forever()
"#;

/// A program that starts the standard library's own WSGI server over a callable of
/// its own.
const WSGI_SERVER_PROGRAM: &[u8] = br#"
import os
from wsgiref.simple_server import make_server


def application(environ, start_response):
    path = environ.get("PATH_INFO") or "/"
    query = environ.get("QUERY_STRING") or ""
    print("served: %s" % path)
    if path not in ("/", "/items"):
        status = "404 Not Found"
        body = ("no such path: %s" % path).encode("utf-8")
    else:
        status = "200 OK"
        greeting = os.environ.get("GREETING", "")
        body = ("%s at %s query=%s" % (greeting, path, query)).encode("utf-8")
    start_response(
        status,
        [("Content-Type", "text/plain; charset=utf-8"), ("Content-Length", str(len(body)))],
    )
    return [body]


print("started: the Procfile's own web process")
with make_server("", int(os.environ.get("PORT", "8000")), application) as httpd:
    httpd.serve_forever()
"#;

/// A program that starts a server on the connection interface itself — the
/// `socketserver` the standard library's own HTTP server is built on — and writes
/// the response its handler composed, inside a package of the deployment, reading
/// the greeting through a module the deployment carries beside it: so a program
/// run by the module form of the command is one whose own imports resolve against
/// the deployment.
const PACKAGE_PROGRAM: &[u8] = br#"
import os
import socketserver

from greeting import greeting


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        requested = self.rfile.readline().decode("iso-8859-1").split(" ")
        while True:
            header = self.rfile.readline()
            if header in (b"\r\n", b"\n", b""):
                break
        target = requested[1] if len(requested) > 1 else "/"
        path, _, query = target.partition("?")
        print("served: %s" % path, flush=True)
        if path not in ("/", "/items"):
            status = "404 Not Found"
            body = ("no such path: %s" % path).encode("utf-8")
        else:
            status = "200 OK"
            body = ("%s at %s query=%s" % (greeting(), path, query)).encode("utf-8")
        self.wfile.write(("HTTP/1.1 %s\r\n" % status).encode("iso-8859-1"))
        self.wfile.write(b"Content-Type: text/plain; charset=utf-8\r\n")
        self.wfile.write(("Content-Length: %d\r\n\r\n" % len(body)).encode("iso-8859-1"))
        self.wfile.write(body)


print("started: the Procfile's own web process", flush=True)
socketserver.TCPServer(("", int(os.environ.get("PORT", "8000"))), Handler).serve_forever()
"#;

/// The module that package's program reads the greeting through.
const GREETING_MODULE: &[u8] = br#"
import os


def greeting():
    return os.environ.get("GREETING", "")
"#;

/// The same server on the connection interface itself, at the root of the
/// deployment: the program the suites that assert what reaches CloudWatch Logs
/// deploy, since what they are about is the output a run produces rather than
/// which of the library's servers produced it.
const SOCKET_SERVER_PROGRAM: &[u8] = br#"
import os
import socketserver


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        requested = self.rfile.readline().decode("iso-8859-1").split(" ")
        while True:
            header = self.rfile.readline()
            if header in (b"\r\n", b"\n", b""):
                break
        target = requested[1] if len(requested) > 1 else "/"
        path, _, query = target.partition("?")
        print("served: %s" % path, flush=True)
        if path not in ("/", "/items"):
            status = "404 Not Found"
            body = ("no such path: %s" % path).encode("utf-8")
        else:
            greeting = os.environ.get("GREETING", "")
            status = "200 OK"
            body = ("%s at %s query=%s" % (greeting, path, query)).encode("utf-8")
        self.wfile.write(("HTTP/1.1 %s\r\n" % status).encode("iso-8859-1"))
        self.wfile.write(b"Content-Type: text/plain; charset=utf-8\r\n")
        self.wfile.write(("Content-Length: %d\r\n\r\n" % len(body)).encode("iso-8859-1"))
        self.wfile.write(body)


print("started: the Procfile's own web process", flush=True)
socketserver.TCPServer(("", int(os.environ.get("PORT", "8000"))), Handler).serve_forever()
"#;

/// A WSGI application a server command names rather than runs: what
/// `gunicorn <module>:<callable>` serves.
const WSGI_APPLICATION: &[u8] = br#"
import os

print("started: the Procfile's own web process")


def served(environ, start_response):
    greeting = os.environ.get("GREETING", "")
    path = environ.get("PATH_INFO") or "/"
    print("served: %s" % path)
    body = ("%s at %s" % (greeting, path)).encode("utf-8")
    start_response(
        "200 OK",
        [("Content-Type", "text/plain; charset=utf-8"), ("Content-Length", str(len(body)))],
    )
    return [body]
"#;

/// The module the Python platform's own default start command serves, for the
/// bundle whose `Procfile` states no web process at all.
const DEFAULT_APPLICATION: &[u8] = br#"
import os

print("started: the Procfile's own web process")


def application(environ, start_response):
    greeting = os.environ.get("GREETING", "")
    body = ("%s at %s" % (greeting, environ.get("PATH_INFO") or "/")).encode("utf-8")
    start_response(
        "200 OK",
        [("Content-Type", "text/plain; charset=utf-8"), ("Content-Length", str(len(body)))],
    )
    return [body]
"#;

/// A Node.js program, for the bundle whose web command runs the other runtime this
/// emulator embeds an engine for.
const NODE_PROGRAM: &[u8] = br#"
const http = require('http');
console.log("started: the Procfile's own web process");
http.createServer(function (req, res) {
  console.log('served: ' + req.url);
  res.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' });
  res.end(process.env.GREETING + ' at ' + req.url);
}).listen(process.env.PORT);
"#;

/// The document a bundle with no application the platform could start serves its
/// root from.
const INDEX_HTML: &[u8] = b"<!doctype html><title>procfile</title><p>no application\n";

/// One bundle whose `Procfile` names a program, and what its server answers: the
/// subject of [`the_programs_own_server_answers`].
///
/// One suite per row rather than one over all of them: a run of a deployed program
/// imports the standard library its server is built on, so a suite per row is what
/// keeps each one's own time to the three runs it drives.
struct ProgramRow {
    case: &'static str,
    /// The `Procfile` the bundle carries.
    procfile: &'static [u8],
    /// The files the bundle carries beside it.
    files: &'static [(&'static str, &'static [u8])],
}

/// The interpreter over a file of the deployment, started unbuffered, which starts
/// the standard library's own HTTP server.
const HTTP_SERVER_ROW: ProgramRow = ProgramRow {
    case: "the interpreter over a file, started unbuffered",
    procfile: b"web: python3 -u server.py\n",
    files: &[("server.py", HTTP_SERVER_PROGRAM)],
};

/// The same command over a file that starts the standard library's own WSGI
/// server instead.
const WSGI_SERVER_ROW: ProgramRow = ProgramRow {
    case: "the interpreter over a file that starts the WSGI server",
    procfile: b"web: python3 serve.py\n",
    files: &[("serve.py", WSGI_SERVER_PROGRAM)],
};

/// The module form of the command, over a module inside a package of the
/// deployment that reads a module beside it.
const PACKAGE_MODULE_ROW: ProgramRow = ProgramRow {
    case: "the module form of the command, inside a package",
    procfile: b"web: python3 -m serving.serve\n",
    files: &[
        ("serving/__init__.py", b""),
        ("serving/serve.py", PACKAGE_PROGRAM),
        ("greeting.py", GREETING_MODULE),
    ],
};

/// One bundle whose `Procfile` states something other than a Python program, and
/// the body the address then answers: a row of
/// [`the_start_command_decides_what_the_platform_starts`].
struct CommandRow {
    case: &'static str,
    /// The `Procfile` the bundle carries, and the files beside it.
    procfile: &'static [u8],
    files: &'static [(&'static str, &'static [u8])],
    /// The platform category the environment is launched on.
    platform_category: &'static str,
    /// The body a GET of the application's root then answers with.
    body: &'static str,
}

/// One row per kind of start command, including the two that leave the platform
/// starting no program of the deployment at all.
fn command_rows() -> Vec<CommandRow> {
    vec![
        CommandRow {
            case: "a command naming a WSGI application for a server to serve",
            procfile: b"web: gunicorn --bind :8000 --workers 3 project.wsgi:served\n",
            files: &[("project/__init__.py", b""), ("project/wsgi.py", WSGI_APPLICATION)],
            platform_category: eb::state::PLATFORM_CATEGORY_PYTHON,
            body: "hello from the environment at /",
        },
        CommandRow {
            case: "a command running the other runtime this emulator embeds",
            procfile: b"web: node app.js\n",
            files: &[("app.js", NODE_PROGRAM)],
            platform_category: eb::state::PLATFORM_CATEGORY_NODEJS,
            body: "hello from the environment at /",
        },
        CommandRow {
            case: "a command this emulator starts no application from",
            procfile: b"web: /usr/bin/env false\n",
            files: &[("index.html", INDEX_HTML), ("server.py", SOCKET_SERVER_PROGRAM)],
            platform_category: eb::state::PLATFORM_CATEGORY_PYTHON,
            // No application was started, so the address serves the FILES the
            // bundle carries — the document a request for the root addresses.
            body: "<!doctype html><title>procfile</title><p>no application\n",
        },
        CommandRow {
            case: "a Procfile stating no web process at all",
            procfile: b"worker: python3 -u worker.py\n",
            files: &[
                ("application.py", DEFAULT_APPLICATION),
                ("worker.py", SOCKET_SERVER_PROGRAM),
            ],
            platform_category: eb::state::PLATFORM_CATEGORY_PYTHON,
            // The platform's own default command, which serves the module its
            // published `WSGIPath` default names.
            body: "hello from the environment at /",
        },
    ]
}

/// The URL a client dials for the environment with the CNAME prefix `prefix`: the
/// address the environment publishes, as it was given it.
fn dialled(emulator: &Emulator, prefix: &str, path: &str) -> String {
    format!("http://{}{path}", emulator.environment_address(TEST_REGION, prefix))
}

/// Call `action`, failing the named case unless it answered 200.
async fn ok(emulator: &Emulator, case: &str, action: &str, params: &[(&str, &str)]) -> Answer {
    let answer = emulator.call(action, params).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from {action}, got {} and body {}",
        answer.status, answer.body
    );
    answer
}

/// Upload a bundle carrying `procfile` and `files`, create the application and a
/// version from it, and launch an environment on a platform of
/// `platform_category` whose configuration is the environment property plus
/// `settings` as `(namespace, option, value)` triples.
///
/// The environment's name and its CNAME prefix are both `prefix`.
async fn launched(
    emulator: &Emulator,
    case: &str,
    prefix: &str,
    procfile: &[u8],
    files: &[(&str, &[u8])],
    platform_category: &str,
    settings: &[(&str, &str, &str)],
) {
    let mut entries: Vec<(&str, &[u8])> = vec![("Procfile", procfile)];
    entries.extend_from_slice(files);
    emulator
        .put_object(BUNDLE_BUCKET, BUNDLE_KEY, &zip_of(&entries))
        .await;
    ok(
        emulator,
        case,
        "CreateApplication",
        &[("ApplicationName", APPLICATION)],
    )
    .await;
    ok(
        emulator,
        case,
        "CreateApplicationVersion",
        &[
            ("ApplicationName", APPLICATION),
            ("VersionLabel", VERSION),
            ("SourceBundle.S3Bucket", BUNDLE_BUCKET),
            ("SourceBundle.S3Key", BUNDLE_KEY),
        ],
    )
    .await;
    let mut configured: Vec<(&str, &str, &str)> = vec![(
        eb::state::NAMESPACE_APPLICATION_ENVIRONMENT,
        PROPERTY_NAME,
        PROPERTY_VALUE,
    )];
    configured.extend_from_slice(settings);
    // The configuration goes on the wire as the awsQuery list `OptionSettings`
    // is, one member per setting.
    let members: Vec<(String, String)> = configured
        .iter()
        .enumerate()
        .flat_map(|(index, (namespace, option, value))| {
            let member = index + 1;
            [
                (
                    format!("OptionSettings.member.{member}.Namespace"),
                    (*namespace).to_string(),
                ),
                (
                    format!("OptionSettings.member.{member}.OptionName"),
                    (*option).to_string(),
                ),
                (
                    format!("OptionSettings.member.{member}.Value"),
                    (*value).to_string(),
                ),
            ]
        })
        .collect();
    let solution_stack = solution_stack_of(platform_category);
    let mut params: Vec<(&str, &str)> = vec![
        ("ApplicationName", APPLICATION),
        ("EnvironmentName", prefix),
        ("CNAMEPrefix", prefix),
        ("SolutionStackName", &solution_stack),
        ("VersionLabel", VERSION),
    ];
    params.extend(
        members
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    ok(emulator, case, "CreateEnvironment", &params).await;
    // The launch the clock owes is waited for by LOOKING AGAIN until the
    // environment reports it is ready to serve, never by waiting a fixed time.
    wait_for_status(emulator, case, prefix, eb::state::STATUS_READY).await;
}

/// GET `path` of the environment's own address, looking again until the deployed
/// application has ANSWERED — a request this emulator had no run slot for is
/// answered as an address with nothing behind it, which is not an answer of the
/// application's.
async fn answered(emulator: &Emulator, case: &str, prefix: &str, path: &str) -> Answer {
    let url = dialled(emulator, prefix, path);
    let deadline = Instant::now() + SERVED_DEADLINE;
    loop {
        let served = emulator.get_url(&url).await;
        if !NOT_THE_APPLICATIONS.contains(&served.status) || Instant::now() >= deadline {
            assert!(
                !NOT_THE_APPLICATIONS.contains(&served.status),
                "case {case:?}: expected {url} to be answered by the deployed application, \
                 got {} and body {}",
                served.status,
                served.body
            );
            return served;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the application an environment runs is the one the bundle's `Procfile`
/// starts — the program its web command runs — so the address the environment
/// publishes is served by the server THAT PROGRAM started, with the status, the
/// body and the request's own path and query the program's handler computed.
///
/// Driven once per form the command and the program's server take
/// ([`HTTP_SERVER_ROW`], [`WSGI_SERVER_ROW`], [`PACKAGE_MODULE_ROW`]), by the
/// suites below, over the `targets` each asks for as
/// `(path, status, body)`.
async fn the_programs_own_server_answers(row: &ProgramRow, targets: &[(&str, u16, String)]) {
    let case = row.case;
    let emulator = Emulator::with_object_storage();
    launched(
        &emulator,
        case,
        "procfile-env-1",
        row.procfile,
        row.files,
        eb::state::PLATFORM_CATEGORY_PYTHON,
        &[],
    )
    .await;

    for (path, status, body) in targets {
        let served = answered(&emulator, case, "procfile-env-1", path).await;
        assert_eq!(
            served.status, *status,
            "case {case:?}: expected {status} for {path}, got {} and body {}",
            served.status, served.body
        );
        assert_eq!(
            served.body, *body,
            "case {case:?}: expected the program's own answer for {path}, got {:?}",
            served.body
        );
    }
}

/// The target every suite below asks for, and the answer the program computes from
/// it: the environment property it was configured with, the path it was asked for
/// and the query string beside that path.
fn computed_target() -> (&'static str, u16, String) {
    (
        "/items?page=2",
        200,
        format!("{PROPERTY_VALUE} at /items query=page=2"),
    )
}

/// A target the program's own handler REFUSES, so the status a client reads is the
/// one the application set rather than one this emulator renders.
fn refused_target() -> (&'static str, u16, String) {
    ("/absent", 404, "no such path: /absent".to_string())
}

/// [`the_programs_own_server_answers`] for a program that starts the standard
/// library's own HTTP server.
#[tokio::test]
async fn a_program_starting_the_http_server_answers_the_published_address() {
    the_programs_own_server_answers(&HTTP_SERVER_ROW, &[computed_target()]).await;
}

/// [`the_programs_own_server_answers`] for a program that starts the standard
/// library's own WSGI server.
#[tokio::test]
async fn a_program_starting_the_wsgi_server_answers_the_published_address() {
    the_programs_own_server_answers(&WSGI_SERVER_ROW, &[computed_target()]).await;
}

/// [`the_programs_own_server_answers`] for a program the module form of the
/// command runs, inside a package of the deployment — and the suite that asks for
/// the path its handler refuses as well, because the server it starts is the one
/// whose run costs least.
#[tokio::test]
async fn a_program_run_as_a_module_answers_the_published_address() {
    the_programs_own_server_answers(
        &PACKAGE_MODULE_ROW,
        &[computed_target(), refused_target()],
    )
    .await;
}

/// RULE: the START COMMAND decides what the platform starts — a program it runs,
/// an application it names for a server to serve, or a program of the other
/// runtime this emulator embeds — and a command this emulator starts nothing from
/// leaves the environment serving the bundle's files, exactly as a `Procfile` that
/// states no web process at all leaves the platform's own default command starting
/// the application.
#[tokio::test]
async fn the_start_command_decides_what_the_platform_starts() {
    for row in command_rows() {
        let case = row.case;
        let emulator = Emulator::with_object_storage();
        launched(
            &emulator,
            case,
            "procfile-env-1",
            row.procfile,
            row.files,
            row.platform_category,
            &[],
        )
        .await;

        let served = answered(&emulator, case, "procfile-env-1", "/").await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: expected 200 from the address, got {} and body {}",
            served.status, served.body
        );
        assert_eq!(
            served.body, row.body,
            "case {case:?}: expected {:?}, got {:?}",
            row.body, served.body
        );
    }
}

/// Every log group under `prefix` CloudWatch Logs holds, as
/// `(name, retentionInDays)`.
async fn log_groups(emulator: &Emulator, case: &str, prefix: &str) -> Vec<(String, Option<i64>)> {
    let answered = logs(
        emulator,
        case,
        "DescribeLogGroups",
        serde_json::json!({ "logGroupNamePrefix": prefix }),
    )
    .await;
    answered
        .get("logGroups")
        .and_then(serde_json::Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| {
                    let name = group.get("logGroupName")?.as_str()?.to_string();
                    Some((name, group.get("retentionInDays").and_then(|days| days.as_i64())))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every message the log group `log_group` carries, read through CloudWatch Logs'
/// own reader of a group — looking again until it carries one.
async fn delivered(emulator: &Emulator, case: &str, log_group: &str) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let answered = logs(
            emulator,
            case,
            "FilterLogEvents",
            serde_json::json!({ "logGroupName": log_group }),
        )
        .await;
        let carried: Vec<String> = answered
            .get("events")
            .and_then(serde_json::Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| Some(event.get("message")?.as_str()?.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if !carried.is_empty() || Instant::now() >= deadline {
            return carried;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the output a Procfile-started program writes to its own standard output
/// is the output the instance's standard-output log file carries, so an
/// environment whose configuration turns instance log streaming on delivers it
/// into the group that file is streamed to — kept for the retention the
/// configuration states, and with no group for a file the environment's platform
/// does not stream.
#[tokio::test]
async fn a_programs_output_reaches_the_group_of_the_file_it_goes_to() {
    let case = "a Procfile-started program on a Python platform, streaming its logs";
    let environment = "procfile-logs-1";
    let retention = "14";
    let emulator = Emulator::with_object_storage_and_logs();
    launched(
        &emulator,
        case,
        environment,
        b"web: python3 -u server.py\n",
        &[("server.py", SOCKET_SERVER_PROGRAM)],
        eb::state::PLATFORM_CATEGORY_PYTHON,
        &[
            (
                eb::state::NAMESPACE_CLOUDWATCH_LOGS,
                eb::state::OPTION_STREAM_LOGS,
                "true",
            ),
            (
                eb::state::NAMESPACE_CLOUDWATCH_LOGS,
                eb::state::OPTION_RETENTION_IN_DAYS,
                retention,
            ),
        ],
    )
    .await;

    // The whole scenario a client drives: the address answers the program's own
    // body, and the line that program printed while starting is in the group the
    // file its output goes to is streamed to.
    let served = answered(&emulator, case, environment, "/").await;
    assert_eq!(
        served.body,
        format!("{PROPERTY_VALUE} at / query="),
        "case {case:?}: expected the program's own answer, got {:?}",
        served.body
    );

    let prefix = format!("/aws/elasticbeanstalk/{environment}");
    let group = eb::state::log_group_name(environment, eb::state::APPLICATION_STDOUT_LOG);
    let held = log_groups(&emulator, case, &prefix).await;
    let kept = held
        .iter()
        .find(|(name, _)| *name == group)
        .unwrap_or_else(|| panic!("case {case:?}: expected a group {group:?}, got {held:?}"));
    assert_eq!(
        kept.1,
        Some(retention.parse::<i64>().expect("a count of days")),
        "case {case:?}: expected the configured retention on {group:?}, got {:?}",
        kept.1
    );
    // A file only another platform's log task writes has no group at all.
    let names: Vec<&str> = held.iter().map(|(name, _)| name.as_str()).collect();
    let not_streamed = eb::state::log_group_name(environment, "/var/log/puma/puma.log");
    assert!(
        !names.contains(&not_streamed.as_str()),
        "case {case:?}: expected no group {not_streamed:?}, got {names:?}"
    );

    let messages = delivered(&emulator, case, &group).await;
    assert!(
        messages.iter().any(|message| message == LOG_MARKER),
        "case {case:?}: expected {LOG_MARKER:?} among the delivered events, got {messages:?}"
    );
}

/// One application whose output a request produces, and the command that starts
/// it: a row of [`the_output_one_request_produces_reaches_the_same_group`].
struct RequestOutputRow {
    case: &'static str,
    procfile: &'static [u8],
    files: &'static [(&'static str, &'static [u8])],
    platform_category: &'static str,
}

/// One row per engine an application the start command names is run in, because
/// the output a request produces is the application's own however it was started.
fn request_output_rows() -> Vec<RequestOutputRow> {
    vec![
        RequestOutputRow {
            case: "a program that starts a server of its own",
            procfile: b"web: python3 -u server.py\n",
            files: &[("server.py", SOCKET_SERVER_PROGRAM)],
            platform_category: eb::state::PLATFORM_CATEGORY_PYTHON,
        },
        RequestOutputRow {
            case: "a WSGI application the command names for a server to serve",
            procfile: b"web: gunicorn --bind :8000 project.wsgi:served\n",
            files: &[("project/__init__.py", b""), ("project/wsgi.py", WSGI_APPLICATION)],
            platform_category: eb::state::PLATFORM_CATEGORY_PYTHON,
        },
        RequestOutputRow {
            case: "a program of the other runtime this emulator embeds",
            procfile: b"web: node app.js\n",
            files: &[("app.js", NODE_PROGRAM)],
            platform_category: eb::state::PLATFORM_CATEGORY_NODEJS,
        },
    ]
}

/// RULE: what the application writes while ANSWERING a request is written to the
/// same instance log file as what it wrote while starting, so a request's own
/// output is delivered into that file's group too — and starting the application
/// is delivered once, for the start it was, rather than again per request.
#[tokio::test]
async fn the_output_one_request_produces_reaches_the_same_group() {
    for row in request_output_rows() {
        let case = row.case;
        let environment = "procfile-logs-1";
        let emulator = Emulator::with_object_storage_and_logs();
        launched(
            &emulator,
            case,
            environment,
            row.procfile,
            row.files,
            row.platform_category,
            &[(
                eb::state::NAMESPACE_CLOUDWATCH_LOGS,
                eb::state::OPTION_STREAM_LOGS,
                "true",
            )],
        )
        .await;
        let group = eb::state::log_group_name(environment, eb::state::APPLICATION_STDOUT_LOG);
        // What STARTING the application wrote is delivered when the environment
        // started it, before any request was served.
        let started = delivered(&emulator, case, &group).await;
        assert_eq!(
            started.iter().filter(|message| *message == LOG_MARKER).count(),
            1,
            "case {case:?}: expected the start-up line once, got {started:?}"
        );

        answered(&emulator, case, environment, "/").await;
        let after = once_carrying(&emulator, case, &group, SERVED_MARKER).await;
        assert!(
            after.iter().any(|message| message == SERVED_MARKER),
            "case {case:?}: expected {SERVED_MARKER:?} among the delivered events, got {after:?}"
        );
        // Serving a request starts the application again in this emulator, but
        // that is no second START of it: the line it wrote while starting stays
        // delivered once.
        assert_eq!(
            after.iter().filter(|message| *message == LOG_MARKER).count(),
            1,
            "case {case:?}: expected the start-up line once, got {after:?}"
        );
    }
}

/// Every message the log group `log_group` carries once it carries `expected` —
/// looking again until it does, the way a client reading streamed logs waits for
/// them.
async fn once_carrying(
    emulator: &Emulator,
    case: &str,
    log_group: &str,
    expected: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let carried = delivered(emulator, case, log_group).await;
        if carried.iter().any(|message| message == expected) || Instant::now() >= deadline {
            return carried;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
