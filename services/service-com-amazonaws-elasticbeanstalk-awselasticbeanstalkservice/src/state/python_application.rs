// SPDX-License-Identifier: CC0-1.0

//! The application a deployed PYTHON bundle runs: the WSGI entry point the platform starts,
//! and what it answers a request with.
//!
//! An Elastic Beanstalk Python platform starts the application from the file its own
//! configuration names — "WSGIPath: the file that contains the WSGI application", whose
//! default is the value the option catalog publishes for it ([`default_wsgi_path`]) — and
//! that file carries a WSGI callable the platform's server calls for every request the environment's address receives. So an
//! environment deploying such a bundle has the same two observable outputs an environment
//! running any other application has: what the application writes to its own standard output
//! (the instance's `/var/log/web.stdout.log`) and what its callable answers.
//!
//! Neither is synthesised here. The bundle's own `.py` files are handed to the CONTAINED
//! Python interpreter this workspace embeds — the one the Lambda service crate owns
//! (`state::execution::python`), reused rather than stood up a second time — the module the
//! entry point names is IMPORTED (which is what writes the application's start-up output),
//! and the callable in it is CALLED with the request's own method, path, query and headers in
//! a WSGI environment ([`SERVE_SOURCE`]). What comes back is what that code returned: a
//! bundle whose import raises, or whose entry point carries no WSGI callable, answers nothing
//! and leaves the plane reporting an address with nothing behind it.
//!
//! # Bounds and containment
//!
//! Both are the interpreter's, in host code no deployed application can reach: the
//! native-module allow-list that leaves every socket, subprocess, memory-map and
//! host-library module unregistered, the in-memory operating-system modules supplied in
//! place of the host's, and the stop no run can clear. What this module states is what ONE
//! run of a deployed application may spend ([`application_budget`]) and the thread it runs
//! on ([`PYTHON_APPLICATION_STACK_BYTES`], [`PYTHON_APPLICATION_HOST_WAIT`]) — the engine is
//! synchronous, so a run never happens on the async executor.
//!
//! Split out of [`super::deployed_application`], which owns WHICH engine a bundle's entry
//! point is run by and the files that bundle carries.

use super::*;

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::python;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::INIT_PHASE_LIMIT;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE;

/// The configuration-option namespace holding the Python platform's own container options.
pub const NAMESPACE_PYTHON_CONTAINER: &str = "aws:elasticbeanstalk:container:python";

/// The option in it that names the file the application is started from: "WSGIPath: the file
/// that contains the WSGI application".
pub const OPTION_WSGI_PATH: &str = "WSGIPath";

/// The suffix of a file the interpreter imports as a module, or runs as a program.
pub(super) const PYTHON_SUFFIX: &str = ".py";

/// How long ONE run of a deployed Python application may last.
///
/// An EMULATOR bound, not an AWS one: the code runs in this process, so a loop with no exit
/// would otherwise hold a thread for ever. This is the WALL CLOCK, so it has to be wide
/// enough for the run a deployed web application really costs here on a host busy with other
/// work: the modules of the standard library its own server is built on are imported into a
/// fresh interpreter for every run, which is seconds of work before a line of the
/// application's own code is reached. The bound that does NOT depend on how fast the host is
/// — and so the one that stops code that diverges — is [`RUN_STEPS`] beside it.
const RUN_WALL_CLOCK: Duration = Duration::from_secs(45);

/// How many traced events one run may execute — the STEP bound beside the wall-clock one, and
/// the one that does not depend on how fast the host is. CHOSEN: large enough to import the
/// standard library an application's server is built on and answer a request through it.
const RUN_STEPS: u64 = 20_000_000;

/// How much memory one run may add to this process. CHOSEN, for the same reason.
const RUN_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// How much one run may write to the `/tmp` of the environment it runs in. CHOSEN, for the
/// same reason: that directory is a disk on a real instance and is held in this process's
/// memory here, so what one run leaves there is bounded like everything else it holds.
const RUN_SCRATCH_BYTES: u64 = 256 * 1024 * 1024;

/// How deep one run may recurse. The interpreter's own default is below what the frozen
/// standard library's import chain needs, so this is the limit a run is given; the call that
/// would raise it is taken away by the engine before any of the application's code runs.
const RUN_RECURSION: usize = 2_000;

/// How much of one run's output is kept, and how large an answer it may return. Both bound
/// what one run holds in memory: the output becomes the instance's standard-output log and
/// the answer becomes the response the address serves.
const RUN_LOG_BYTES: usize = 64 * 1024;
/// See [`RUN_LOG_BYTES`].
const RUN_ANSWER_BYTES: usize = 64 * 1024;

/// The stack the thread one run happens on is given: well above the interpreter's clamped
/// recursion limit, so deep recursion in an application ends that run with the interpreter's
/// own `RecursionError` rather than touching the thread's guard page.
pub const PYTHON_APPLICATION_STACK_BYTES: usize = 16 * 1024 * 1024;

/// How long the task performing one run waits for the thread it runs on.
///
/// The spans the engine itself allows — the interpreter standing up, then the run's own wall
/// clock — plus the grace its stop needs to land: the stop is raised at the next traced event
/// the interpreter delivers, so a thread the engine has stopped answers a moment after the
/// deadline rather than at it. A thread that has still not answered by then is given up on
/// and the run reported as one that produced nothing.
pub const PYTHON_APPLICATION_HOST_WAIT: Duration = INIT_PHASE_LIMIT
    .saturating_add(RUN_WALL_CLOCK)
    .saturating_add(Duration::from_secs(5));

/// The module this emulator hands the interpreter to serve one request with, and the function
/// in it the engine calls.
const SERVE_MODULE: &str = "__eb_wsgi_server";
/// See [`SERVE_MODULE`].
const SERVE_FUNCTION: &str = "serve";

/// What the Python platform starts the deployed bundle from: the module the WSGI application
/// is in, and the callable in it the configuration named, if it named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonEntry {
    /// The module as the interpreter imports it (`application`, `project.wsgi`).
    pub module: String,
    /// The callable in it, when the configuration names one in the `module:callable` form the
    /// platform's own server takes; otherwise the run resolves the documented name
    /// ([`SERVE_SOURCE`]).
    pub callable: Option<String>,
}

/// The Python entry point of `bundle` under the configuration `option_settings`, or `None`
/// when the bundle carries no module that entry point could be in.
///
/// "WSGIPath: the file that contains the WSGI application" names it, in either of the two
/// forms the platform's own server takes: the module or file the application is in
/// (`application`, `application.py`, `project/wsgi.py`) and the `module:callable`
/// specification. A configuration that names none leaves the platform starting the
/// application from the value the option catalog publishes as that option's default
/// ([`default_wsgi_path`]) — the ONLY module an unconfigured environment starts, because that
/// default is the whole of what the platform looks for.
///
/// A path naming no module OF THE BUNDLE, and one that is not a path inside the deployment at
/// all ([`bundle_relative`]), start nothing: the platform starts a file of the deployment or
/// no application.
#[must_use]
pub fn python_entry(
    bundle: &ApplicationBundle,
    option_settings: &[ConfigurationOptionSetting],
) -> Option<PythonEntry> {
    let stated = option_value(option_settings, NAMESPACE_PYTHON_CONTAINER, OPTION_WSGI_PATH)
        .map(str::trim)
        .filter(|path| !path.is_empty());
    let named: &str = match stated {
        Some(stated) => stated,
        None => default_wsgi_path(),
    };
    let (path, callable) = match named.split_once(':') {
        Some((path, callable)) => (path, Some(callable.trim().to_string())),
        None => (named, None),
    };
    // The module the path names has to BE in the bundle: a path naming no file of the
    // deployment is one the platform starts nothing from.
    let module = module_path(bundle_relative(path)?);
    bundle_module(bundle, &module).map(|module| PythonEntry { module, callable })
}

/// The WSGI application the start command `command` serves, or `None` when it names none of
/// `bundle`.
///
/// A command that starts a WSGI SERVER over the application — the `gunicorn`, `uwsgi` or
/// `waitress-serve` an Elastic Beanstalk Python platform's own default command and its
/// documented `Procfile` examples run — states which application it serves as the
/// `module:callable` specification those servers take, among its own options. So the entry
/// point such a command starts is read out of the command itself, by the same rule the
/// platform's `WSGIPath` is read by ([`python_entry`]): the module has to BE in the deployment.
#[must_use]
pub fn python_wsgi_specified(
    bundle: &ApplicationBundle,
    command: &str,
) -> Option<PythonEntry> {
    command
        .split_whitespace()
        // An option of the server itself, whose value may well carry the separator
        // (`--bind :8000`), names no application.
        .filter(|token| !token.starts_with('-'))
        .filter_map(|token| token.split_once(':'))
        .filter(|(_, callable)| !callable.is_empty())
        .find_map(|(path, callable)| {
            let module = module_path(bundle_relative(path)?);
            bundle_module(bundle, &module).map(|module| PythonEntry {
                module,
                callable: Some(callable.to_string()),
            })
        })
}

/// `path` as a path INSIDE the deployment, or `None` when it is not one: a path of its own
/// root, and one that climbs out of the deployment, names no file the platform could start the
/// application from.
fn bundle_relative(path: &str) -> Option<&str> {
    let path = path.trim().trim_start_matches("./");
    (!path.starts_with('/') && !path.split('/').any(|segment| segment == "..")).then_some(path)
}

/// The value the published option catalog states for `WSGIPath` when a configuration sets
/// none.
///
/// Read from that catalog rather than written here a second time, so the default the platform
/// starts an application from and the `DefaultValue`
/// `DescribeConfigurationOptions` publishes for the same option cannot differ.
fn default_wsgi_path() -> &'static str {
    super::configuration_option_catalog::PYTHON_CONFIGURATION_OPTIONS
        .iter()
        .find(|option| option.key() == (NAMESPACE_PYTHON_CONTAINER, OPTION_WSGI_PATH))
        .map(|option| option.default_value)
        .unwrap_or_default()
}

/// The module name `path` imports as: the file's own path without its suffix, with the
/// separators that make it a package path.
fn module_path(path: &str) -> String {
    path.strip_suffix(PYTHON_SUFFIX)
        .unwrap_or(path)
        .replace('/', ".")
}

/// `module` when `bundle` carries the file it would be imported from, which is what makes it
/// a module the interpreter can import at all.
fn bundle_module(bundle: &ApplicationBundle, module: &str) -> Option<String> {
    let file = format!("{}{PYTHON_SUFFIX}", module.replace('.', "/"));
    bundle.files.contains_key(&file).then(|| module.to_string())
}

/// Every Python module `bundle` carries, as the interpreter is handed them: a file whose
/// bytes are not text is left out, because nothing here can present a module it cannot read.
///
/// The modules this emulator SUPPLIES — this one and the serving layer a program runs on
/// ([`SERVE_MODULE`], [`super::python_program::SERVE_MODULE`]) — are left out of them: the
/// interpreter is handed the deployment's modules and those over them, so a bundle carrying a
/// file of either name cannot replace the server side of the platform with its own.
#[must_use]
pub fn python_modules(bundle: &ApplicationBundle) -> Vec<PackageModule> {
    let supplied: Vec<String> = [SERVE_MODULE, super::python_program::SERVE_MODULE]
        .iter()
        .map(|module| format!("{module}{PYTHON_SUFFIX}"))
        .collect();
    bundle
        .files
        .iter()
        .filter(|(path, _)| path.ends_with(PYTHON_SUFFIX) && !supplied.contains(path))
        .filter_map(|(path, bytes)| {
            ::std::str::from_utf8(bytes).ok().map(|source| PackageModule {
                path: path.clone(),
                source: source.to_string(),
            })
        })
        .collect()
}

/// What one run of a deployed application may spend, which is every bound this module states.
pub(super) fn application_budget() -> python::Budget {
    python::Budget {
        wall_clock: RUN_WALL_CLOCK,
        steps: RUN_STEPS,
        memory_bytes: RUN_MEMORY_BYTES,
        ephemeral_bytes: RUN_SCRATCH_BYTES,
        log_bytes: RUN_LOG_BYTES,
        response_bytes: RUN_ANSWER_BYTES,
        recursion_limit: RUN_RECURSION,
    }
}

/// RUN the deployed Python application: import the module its entry point names and, for a
/// run carrying `request`, hand that request to the WSGI callable in it.
///
/// SYNCHRONOUS, and called on a thread of its own with no state guard held: the interpreter
/// is synchronous, and its own stop is what ends code with no exit.
#[must_use]
pub fn run_python_application(
    entry: &PythonEntry,
    modules: &[PackageModule],
    environment: &[(String, String)],
    request: Option<&GuestRequest>,
) -> GuestRun {
    let mut supplied = vec![PackageModule {
        path: format!("{SERVE_MODULE}{PYTHON_SUFFIX}"),
        source: SERVE_SOURCE.to_string(),
    }];
    supplied.extend_from_slice(modules);
    let event = ::serde_json::json!({
        "module": entry.module,
        "callable": entry.callable,
        "request": request.map(request_document),
    })
    .to_string();
    run_guest(
        &supplied,
        &format!("{SERVE_MODULE}.{SERVE_FUNCTION}"),
        &event,
        environment,
    )
}

/// Run `handler` of the modules `supplied` over `event`, under the bounds and the containment
/// one run of a deployed application has.
///
/// The one place this crate starts the embedded interpreter, so a deployment started by the
/// platform's own server ([`run_python_application`]) and one started as a program
/// ([`super::python_program::run_python_program`]) cannot be run under different bounds, in a
/// different environment or with a different seam.
pub(super) fn run_guest(
    supplied: &[PackageModule],
    handler: &str,
    event: &str,
    environment: &[(String, String)],
) -> GuestRun {
    let environment: BTreeMap<String, String> = environment.iter().cloned().collect();
    let finished = python::run(&python::Run {
        modules: supplied,
        handler,
        environment: &environment,
        // The instruction set the environment the application runs in has, which the run
        // reports as its own machine: an Elastic Beanstalk environment names none, so it is
        // the one the engine reports when none is named.
        architecture: DEFAULT_ARCHITECTURE,
        event: Some(event),
        request_id: "",
        invoked_function_arn: "",
        client_context: None,
        tenant_id: None,
        budget: application_budget(),
        // A deployed application is handed no AWS SDK seam of this emulator's: nothing of
        // this environment's own configuration vends it a credential to make calls with, so
        // a call it tries is reported to it as one that was not made rather than signed as
        // an identity nobody gave it.
        aws: None,
        // No durable execution: an environment's application is no execution's run, so it
        // takes no step of one and checkpoints nothing onto one.
        durable: None,
    });
    guest_run(finished)
}

/// One request, as the JSON the WSGI environment — or the request bytes a program's own
/// server is handed — is built from.
pub(super) fn request_document(request: &GuestRequest) -> ::serde_json::Value {
    let headers: ::serde_json::Map<String, ::serde_json::Value> = request
        .headers
        .iter()
        .map(|(name, value)| {
            (
                name.to_ascii_lowercase(),
                ::serde_json::Value::String(value.clone()),
            )
        })
        .collect();
    ::serde_json::json!({
        "method": request.method,
        "path": request.path,
        "headers": ::serde_json::Value::Object(headers),
    })
}

/// What the finished run produced, in the form every engine of this crate reports a run in:
/// the output the application wrote, the response it answered, and the account of a run that
/// answered none.
fn guest_run(finished: python::Finished) -> GuestRun {
    let (answered, failure) = match &finished.completion {
        python::Completion::Returned(payload) => (
            ::serde_json::from_str::<::serde_json::Value>(payload).ok(),
            None,
        ),
        python::Completion::Raised {
            error_type,
            error_message,
            ..
        } => (None, Some(format!("{error_type}: {error_message}"))),
        python::Completion::Stopped(stop) => (None, Some(stop.reason().to_string())),
        python::Completion::Internal(detail) => (None, Some(detail.clone())),
        // The engine reports this for a handler that STREAMED its answer, which the module
        // this emulator supplies never does: it returns the response it built.
        python::Completion::Streamed { .. } => (
            None,
            Some("the deployed application streamed no response this plane reads".to_string()),
        ),
    };
    GuestRun {
        // The interpreter captures the run's standard output and standard error into one
        // log, which is what the instance's standard-output log file carries.
        stdout: finished.log,
        stderr: String::new(),
        // What the application wrote while serving the request, which the serving layer
        // captured on its own ([`SERVE_SOURCE`]) rather than leaving in the run's log beside
        // the output of starting the application.
        served: answered
            .as_ref()
            .and_then(|answered| answered.get("log"))
            .and_then(::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        response: answered.as_ref().and_then(served_response),
        failure,
    }
}

/// The response the application answered, read back off the document the supplied module
/// returned — and `None` for a run that answered no request (one started with none to serve,
/// and one whose application never started a response), or whose answer could not be read.
fn served_response(answered: &::serde_json::Value) -> Option<GuestResponse> {
    let status = answered.get("status")?.as_u64()?;
    let headers = answered
        .get("headers")
        .and_then(::serde_json::Value::as_array)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|header| {
                    let pair = header.as_array()?;
                    let name = pair.first()?.as_str()?;
                    let value = pair.get(1)?.as_str()?;
                    Some((name.to_ascii_lowercase(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    // The body travels BASE64-encoded, because a WSGI application answers BYTES: a response
    // body is an image, an archive or any other content that is not text, and encoding it is
    // what carries those bytes back unchanged.
    let body = answered
        .get("body")
        .and_then(::serde_json::Value::as_str)
        .map(|encoded| ::base64::Engine::decode(&::base64::engine::general_purpose::STANDARD, encoded))
        .transpose()
        .ok()?
        .unwrap_or_default();
    Some(GuestResponse {
        status: u16::try_from(status).ok()?,
        headers,
        body,
    })
}

/// The module this emulator hands the interpreter: the WSGI server side of a Python
/// platform's application server, and nothing else.
///
/// It imports the module the entry point names — which is what runs the application's own
/// module-level code and writes its start-up output — and, for a run carrying a request,
/// builds the WSGI environment of that request and calls the application with it, returning
/// the status, the headers and the body the application produced. WSGI's own contract is what
/// it implements: the callable is handed `(environ, start_response)`, its answer is the
/// iterable it returns (closed afterwards if it carries a `close`) together with whatever it
/// wrote through the callable `start_response` hands back, and an application that never
/// started a response has answered nothing.
const SERVE_SOURCE: &str = r#"
"""Serving one request with the WSGI application an Elastic Beanstalk platform starts."""

import base64
import importlib
import io
import sys

# The name a WSGI application is found under when the configuration states no
# `module:callable`: the `application` Elastic Beanstalk's Python platform documents for the
# file its `WSGIPath` names.
_CALLABLE_NAMES = ("application",)

# What the platform's server reports to the application about itself when the request carries
# no host of its own.
_DEFAULT_SERVER_NAME = "localhost"
_DEFAULT_SERVER_PORT = "80"


def _application(module, named):
    """The WSGI callable of `module`: the one named, else the documented name on it."""
    names = (named,) if named else _CALLABLE_NAMES
    for name in names:
        found = getattr(module, name, None)
        if found is not None:
            return found
    raise AttributeError(
        "no WSGI application named %s in %r"
        % (" or ".join(repr(name) for name in names), module.__name__)
    )


def _environ(request):
    """The WSGI environment of one request, as the platform's server builds it."""
    target = request.get("path") or "/"
    path, _, query = target.partition("?")
    environ = {
        "REQUEST_METHOD": (request.get("method") or "GET").upper(),
        "SCRIPT_NAME": "",
        "PATH_INFO": path or "/",
        "QUERY_STRING": query,
        "SERVER_PROTOCOL": "HTTP/1.1",
        "wsgi.version": (1, 0),
        "wsgi.url_scheme": "http",
        # The request is already fully read and carries no body, so the application reads an
        # empty stream rather than one that could block.
        "wsgi.input": io.BytesIO(b""),
        "wsgi.errors": sys.stderr,
        "wsgi.multithread": False,
        "wsgi.multiprocess": False,
        "wsgi.run_once": False,
    }
    for name, value in (request.get("headers") or {}).items():
        key = str(name).upper().replace("-", "_")
        if key == "CONTENT_TYPE":
            environ[key] = value
        elif key != "CONTENT_LENGTH":
            environ["HTTP_" + key] = value
    # The length of the body the application is handed, which is the empty stream above — not
    # whatever length the client's own request declared.
    environ["CONTENT_LENGTH"] = "0"
    host = environ.get("HTTP_HOST") or ""
    server_name, _, server_port = host.partition(":")
    environ["SERVER_NAME"] = server_name or _DEFAULT_SERVER_NAME
    environ["SERVER_PORT"] = server_port or _DEFAULT_SERVER_PORT
    return environ


def _status_code(status):
    """The code of a WSGI status line ("200 OK"), which is what a client reads as the status."""
    code = str(status or "").split(" ", 1)[0]
    return int(code) if code.isdigit() else 200


def serve(event, context):
    """Import the application and, for a run carrying a request, answer that request."""
    module = importlib.import_module(event["module"])
    request = event.get("request")
    if request is None:
        # A run with no request to serve: the import above is what it was for, and what the
        # application wrote while being imported is its start-up output.
        return {}

    # What the application writes from here on it writes while SERVING this request, so the
    # platform's server captures it apart from the output of starting the application: it is
    # the output that one request produced, and it is attributable to it.
    served = io.StringIO()
    writing, failing = sys.stdout, sys.stderr
    sys.stdout = sys.stderr = served
    try:
        answered = _answer(module, event, request)
    except BaseException:
        # The application wrote this before it raised, and a run that answered nothing
        # reports no request's output: it belongs in the run's own log instead of being lost.
        writing.write(served.getvalue())
        raise
    finally:
        sys.stdout, sys.stderr = writing, failing
    if answered:
        answered["log"] = served.getvalue()
    return answered


def _answer(module, event, request):
    """Hand one request to the WSGI application and answer with what it returned."""
    answered = {}
    chunks = []

    def write(chunk):
        """The writer `start_response` hands back, which a write-style application answers through."""
        if chunk:
            chunks.append(chunk if isinstance(chunk, bytes) else str(chunk).encode("utf-8"))

    def start_response(status, headers, exc_info=None):
        answered["status"] = status
        answered["headers"] = headers
        return write

    body = _application(module, event.get("callable"))(_environ(request), start_response)
    try:
        for chunk in body:
            write(chunk)
    finally:
        close = getattr(body, "close", None)
        if close is not None:
            close()
    if "status" not in answered:
        # The application never started a response, so it answered nothing: there is no status
        # of its own to report, and none is invented for it.
        return {}
    return {
        "status": _status_code(answered.get("status")),
        "headers": [
            [str(name), str(value)] for name, value in (answered.get("headers") or [])
        ],
        # A WSGI application answers BYTES, so the body travels encoded: the bytes it wrote
        # reach the client unchanged whether or not they are text.
        "body": base64.b64encode(b"".join(chunks)).decode("ascii"),
    }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// A bundle carrying `files`, read the way a source bundle is.
    fn bundle(files: &[(&str, &[u8])]) -> ApplicationBundle {
        ApplicationBundle {
            files: files
                .iter()
                .map(|(path, bytes)| ((*path).to_string(), bytes.to_vec()))
                .collect(),
        }
    }

    /// One setting of the Python platform's container namespace.
    fn wsgi_path(value: &str) -> Vec<ConfigurationOptionSetting> {
        vec![ConfigurationOptionSetting {
            resource_name: None,
            namespace: Some(NAMESPACE_PYTHON_CONTAINER.to_string()),
            option_name: Some(OPTION_WSGI_PATH.to_string()),
            value: Some(value.to_string()),
        }]
    }

    /// One bundle and configuration the entry point is resolved from, and what it resolves
    /// to: a row of [`the_entry_point_is_the_file_the_configuration_names`].
    struct EntryRow {
        case: &'static str,
        /// The files the bundle carries, as `(path, content)`.
        files: &'static [(&'static str, &'static [u8])],
        /// The `WSGIPath` the configuration states, or `None` for one that states none.
        wsgi_path: Option<&'static str>,
        /// The module the entry point resolves to, and the callable the configuration named.
        entry: Option<(&'static str, Option<&'static str>)>,
    }

    /// RULE: the entry point is the module the platform's own option names — in either form
    /// it takes — and, where the configuration names none, the module its published default
    /// names; a name no module of the bundle answers to, and a path that is not one inside
    /// the deployment, start nothing.
    #[test]
    fn the_entry_point_is_the_file_the_configuration_names() {
        let cases: &[EntryRow] = &[
            EntryRow {
                case: "the published default WSGIPath, carried by the bundle",
                files: &[("application.py", b"application = None\n")],
                wsgi_path: None,
                entry: Some(("application", None)),
            },
            EntryRow {
                case: "a module the configuration names at the bundle's root",
                files: &[("app.py", b"application = None\n")],
                wsgi_path: Some("app.py"),
                entry: Some(("app", None)),
            },
            EntryRow {
                case: "a module the configuration names inside a package",
                files: &[("site/wsgi.py", b"application = None\n")],
                wsgi_path: Some("site/wsgi.py"),
                entry: Some(("site.wsgi", None)),
            },
            EntryRow {
                case: "the module:callable form the platform's server takes",
                files: &[("server.py", b"handler = None\n")],
                wsgi_path: Some("server:handler"),
                entry: Some(("server", Some("handler"))),
            },
            EntryRow {
                case: "a WSGIPath naming no module of the bundle",
                files: &[("application.py", b"application = None\n")],
                wsgi_path: Some("absent.py"),
                entry: None,
            },
            EntryRow {
                case: "a bundle whose only module the published default does not name",
                files: &[("app.py", b"application = None\n")],
                wsgi_path: None,
                entry: None,
            },
            EntryRow {
                case: "a WSGIPath that climbs out of the deployment",
                files: &[("application.py", b"application = None\n")],
                wsgi_path: Some("../application.py"),
                entry: None,
            },
            EntryRow {
                case: "a WSGIPath of its own root",
                files: &[("application.py", b"application = None\n")],
                wsgi_path: Some("/etc/application.py"),
                entry: None,
            },
            EntryRow {
                case: "a bundle carrying no Python module at all",
                files: &[("index.html", b"<p>static\n")],
                wsgi_path: None,
                entry: None,
            },
        ];
        for row in cases {
            let case = row.case;
            let settings = row.wsgi_path.map(wsgi_path).unwrap_or_default();
            let actual = python_entry(&bundle(row.files), &settings);
            let expected = row.entry.map(|(module, callable)| PythonEntry {
                module: module.to_string(),
                callable: callable.map(str::to_string),
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the option this module reads the entry point off is one the service PUBLISHES,
    /// so a client can read the same (namespace, option) out of
    /// `DescribeConfigurationOptions` and set it.
    #[test]
    fn the_entry_point_option_is_a_published_one() {
        let published = super::super::configuration_option_catalog::PYTHON_CONFIGURATION_OPTIONS
            .iter()
            .any(|option| option.key() == (NAMESPACE_PYTHON_CONTAINER, OPTION_WSGI_PATH));
        assert!(
            published,
            "expected the catalog to publish {NAMESPACE_PYTHON_CONTAINER} / {OPTION_WSGI_PATH}"
        );
        let default = default_wsgi_path();
        assert!(
            !default.is_empty(),
            "expected the published option to carry a default, got {default:?}"
        );
    }

    /// RULE: the modules handed to the interpreter are the bundle's own Python files, and
    /// only those — a file of another kind, one whose bytes are not text, and one named after
    /// the module this emulator supplies, is no module of the deployment.
    #[test]
    fn only_the_bundles_python_files_are_handed_over() {
        let files: &[(&str, &[u8])] = &[
            ("application.py", b"application = None\n"),
            ("site/wsgi.py", b"application = None\n"),
            ("index.html", b"<p>static\n"),
            ("broken.py", &[0xff, 0xfe, 0x00]),
            ("__eb_wsgi_server.py", b"serve = None\n"),
        ];
        let modules = python_modules(&bundle(files));
        let paths: Vec<&str> = modules.iter().map(|module| module.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["application.py", "site/wsgi.py"],
            "expected only the bundle's readable Python files, got {paths:?}"
        );
    }
}
