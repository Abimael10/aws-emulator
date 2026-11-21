// SPDX-License-Identifier: CC0-1.0

//! The application a deployed bundle runs as a PROGRAM: the command the platform starts, the
//! server that program starts for itself, and what that server answers a request with.
//!
//! An Elastic Beanstalk platform of the current generation starts the application with the
//! command the source bundle's `Procfile` gives its `web` process, where the bundle carries
//! one ([`super::procfile`]) — "the command that starts your application", in place of the
//! platform's own default command. For a Python platform that command is a run of the
//! interpreter over a file of the deployment (`python3 -u server.py`), and such a program
//! starts a SERVER of its own rather than handing a WSGI callable to the platform's server
//! ([`super::python_application`]).
//!
//! Both of its observable outputs are the program's own. It is RUN by the contained Python
//! interpreter this workspace embeds, as `__main__`, exactly as the command runs it — which is
//! what writes the program's start-up output (the instance's `/var/log/web.stdout.log`) — and
//! the request the environment's address received is handed to the server that program started,
//! through that server's OWN accept path (`finish_request`), so the bytes the client reads are
//! the bytes the program's request handler wrote. Nothing is synthesised: a program that starts
//! no server, or whose own code raises, answers nothing and leaves the plane reporting an
//! address with nothing behind it.
//!
//! # What the platform supplies under the program
//!
//! A program that serves HTTP binds a listening socket and waits on it. There is no listening
//! socket here — the connection the client opened is the emulator's own
//! ([`super::environment_plane_endpoint`]) — so the serving layer under the program is the
//! platform's: the bind and the listen reach no operating system, and the call that would run
//! the accept loop for the life of the process is where the program is stopped and its server
//! kept ([`SERVE_SOURCE`]). Everything above that layer is the standard library's own HTTP
//! server and the program's own handler, run as written.
//!
//! The bounds and the containment are [`super::python_application`]'s: one run of a deployed
//! application spends the same budget whichever command started it.

use super::*;

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;

/// The file names a command runs a Python program with: the interpreter, under any of the
/// names a platform's own start command spells it (`python`, `python3`, `python3.11`).
const PYTHON_INTERPRETER: &str = "python";

/// The option a run of the interpreter names a MODULE to run as a program with, rather than a
/// file: `python3 -m http.server`.
const MODULE_OPTION: &str = "-m";

/// The file a package is run as a program from, which is what `-m <package>` runs.
const PACKAGE_MAIN: &str = "__main__.py";

/// The module this emulator hands the interpreter to run a program and serve one request with,
/// and the function in it the engine calls.
pub(super) const SERVE_MODULE: &str = "__eb_platform_server";
/// See [`SERVE_MODULE`].
const SERVE_FUNCTION: &str = "serve";

/// What the Python platform starts when its start command runs a PROGRAM: the file of the
/// deployment that command runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonProgram {
    /// The file as the deployment carries it (`server.py`, `app/main.py`).
    pub file: String,
}

/// The Python program `command` runs out of `bundle`, or `None` when it runs none: a command
/// that is no run of the interpreter, and one naming a file or module the deployment does not
/// carry.
///
/// The command is read the way a shell hands it to the interpreter: the interpreter itself,
/// then its own options, then the file it runs — or the module `-m` names, which is that
/// module's own file of the deployment, or the `__main__` of the package it names.
#[must_use]
pub fn python_program(bundle: &ApplicationBundle, command: &str) -> Option<PythonProgram> {
    let mut tokens = command.split_whitespace();
    if !runs_the_interpreter(tokens.next()?) {
        return None;
    }
    while let Some(token) = tokens.next() {
        if token == MODULE_OPTION {
            return module_program(bundle, tokens.next()?);
        }
        // An option of the interpreter itself (`-u`), which names no program.
        if token.starts_with('-') {
            continue;
        }
        let file = token.trim_start_matches("./");
        return (file.ends_with(PYTHON_SUFFIX) && bundle.files.contains_key(file))
            .then(|| PythonProgram {
                file: file.to_string(),
            });
    }
    None
}

/// Whether `token` runs the Python interpreter: the command's own file name, whatever
/// directory it was named under — the interpreter's name, and otherwise its name followed by
/// the release it is the interpreter of (`python3`, `python3.11`), which is how a platform's
/// own start command spells it.
fn runs_the_interpreter(token: &str) -> bool {
    let name = token.rsplit('/').next().unwrap_or(token);
    let Some(release) = name.strip_prefix(PYTHON_INTERPRETER) else {
        return false;
    };
    release
        .chars()
        .all(|character| character.is_ascii_digit() || character == '.')
}

/// The program `-m <module>` runs: the module's own file of the deployment, and otherwise the
/// `__main__` of the package that name stands for.
fn module_program(bundle: &ApplicationBundle, module: &str) -> Option<PythonProgram> {
    let directory = module.replace('.', "/");
    [
        format!("{directory}{PYTHON_SUFFIX}"),
        format!("{directory}/{PACKAGE_MAIN}"),
    ]
    .into_iter()
    .find(|file| bundle.files.contains_key(file))
    .map(|file| PythonProgram { file })
}

/// RUN the deployed program: execute `source` as `__main__`, the way the start command runs
/// it, and hand `request` — where the run carries one — to the server it started.
///
/// SYNCHRONOUS, and called on a thread of its own with no state guard held, for the reasons
/// [`super::python_application::run_python_application`] states.
#[must_use]
pub fn run_python_program(
    program: &PythonProgram,
    source: &str,
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
        "program": { "path": program.file, "source": source },
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

/// The module this emulator hands the interpreter: the serving layer a program that starts a
/// server of its own runs on, and nothing else.
///
/// It runs the program as `__main__` and keeps the server that program starts, then hands one
/// request to that server through the server's own accept path — so the request is parsed, the
/// handler is called and the response is written by the standard library's own HTTP server and
/// the program's own code. What comes back is the bytes that server wrote, read as the status,
/// the headers and the body the client is answered with.
const SERVE_SOURCE: &str = r#"
"""Running the program an Elastic Beanstalk platform starts, and serving one request with it."""

import io
import sys

# Everything else this module needs it imports where it uses it: the modules of the standard
# library the serving layer is built on are imported while the run SERVES, rather than while
# the interpreter is still standing up, so a slow host spends them against the run's own span.

# The directory a deployment's files are under, which is what a frame of the program names.
_TASK_ROOT = "/var/task"

# What the server reports about the host it runs on, where this runtime resolves no name, and
# the host a request that carried none is served under.
_HOST_NAME = "localhost"

# The client of one request as the program's server sees it: the proxy on the instance itself,
# which is what forwards a request to the application.
_CLIENT_ADDRESS = ("127.0.0.1", 0)

# The headers of the client's own request that are NOT handed to the program's server: no body
# is forwarded, so a length or a transfer encoding declaring one would describe a body that is
# not there.
_HEADERS_NOT_FORWARDED = ("content-length", "transfer-encoding")


class _Serving(BaseException):
    """Where the program hands its socket to its own accept loop.

    A program that starts serving does not return: the call it is stopped at here is the one
    that runs for the life of the process. Not an `Exception`, so a program's own error
    handling does not catch the point at which it started serving.
    """


# The server the program started, filled by the serving call below.
_STARTED = {}


def _bound(self):
    """`server_bind`: NO socket is bound.

    The address the program asked for stays the address its server reports, because no
    operating system lent it one — the requests this server answers are the ones the
    environment's own address received.
    """


def _activated(self):
    """`server_activate`: nothing is listened on, for the same reason."""


def _serving(self, *arguments, **keywords):
    """The server's accept loop: the point the program starts serving at.

    The server is kept and the program is stopped exactly there, which is where a real run of
    it stays for the life of the process.
    """
    _STARTED["server"] = self
    raise _Serving()


def _hostname(name=""):
    """The name of the host the server runs on, where this runtime resolves none itself."""
    return name or _HOST_NAME


def _install():
    """Put the platform's serving layer under the program, before a line of it runs."""
    import socket
    import socketserver

    socketserver.TCPServer.server_bind = _bound
    socketserver.TCPServer.server_activate = _activated
    socketserver.BaseServer.serve_forever = _serving
    socketserver.BaseServer.handle_request = _serving
    try:
        socket.getfqdn("")
    except Exception:
        socket.getfqdn = _hostname


class _Connection(object):
    """The connection one request arrived on, as the program's own server reads and writes it.

    The bytes it reads are the request the environment's address received and the bytes it
    writes are the answer the client is given: there is no socket underneath, because the
    connection the client opened is the emulator's own.
    """

    def __init__(self, received):
        self._received = io.BytesIO(received)
        self._sent = bytearray()

    def sent(self):
        """Everything the server has written on this connection."""
        return bytes(self._sent)

    def makefile(self, mode="rb", bufsize=-1, *arguments, **keywords):
        """The stream the server reads the request off, or writes its answer to."""
        if "r" in mode:
            return self._received
        return self

    def recv(self, size=-1):
        return self._received.read(size)

    def sendall(self, data):
        self._sent.extend(data)

    def send(self, data):
        self._sent.extend(data)
        return len(data)

    def write(self, data):
        self._sent.extend(data)
        return len(data)

    def flush(self):
        pass

    def close(self):
        pass

    @property
    def closed(self):
        return False

    def settimeout(self, timeout):
        pass

    def gettimeout(self):
        return None

    def setsockopt(self, *arguments, **keywords):
        pass

    def getsockopt(self, *arguments, **keywords):
        return 0

    def shutdown(self, how=0):
        pass

    def fileno(self):
        return -1

    def getpeername(self):
        return _CLIENT_ADDRESS

    def getsockname(self):
        return (_CLIENT_ADDRESS[0], 0)


def _framed(name, value):
    """Whether a header can be handed over as it stands: a name or a value carrying a line
    ending, and a name carrying the separator or a space, is one no request could carry."""
    for character in ("\r", "\n", "\x00"):
        if character in name or character in value:
            return False
    return name != "" and ":" not in name and " " not in name


def _requested(request):
    """The request bytes the program's server is handed: the request line and the headers the
    client sent, as HTTP frames them — and `None` for a request that cannot be framed at all."""
    method = str(request.get("method") or "GET").upper()
    target = str(request.get("path") or "") or "/"
    if not _framed(method, target):
        return None
    lines = ["%s %s HTTP/1.1" % (method, target)]
    host = None
    for name, value in (request.get("headers") or {}).items():
        name = str(name)
        value = str(value)
        if name.lower() in _HEADERS_NOT_FORWARDED or not _framed(name, value):
            continue
        if name.lower() == "host":
            host = value
        lines.append("%s: %s" % (name, value))
    if host is None:
        lines.append("Host: %s" % _HOST_NAME)
    lines.append("")
    lines.append("")
    return "\r\n".join(lines).encode("iso-8859-1", "replace")


def _answered(sent):
    """The response the server wrote, read as a client reads it: the status it set, the headers
    it wrote and the body after them. A server that wrote no status line answered nothing."""
    import base64
    import http.client

    head, separator, body = sent.partition(b"\r\n\r\n")
    status_line, _, header_lines = head.partition(b"\r\n")
    stated = status_line.decode("iso-8859-1", "replace").split(" ")
    if len(stated) < 2 or not stated[1].isdigit():
        return {}
    headers = http.client.parse_headers(io.BytesIO(header_lines + b"\r\n\r\n"))
    return {
        "status": int(stated[1]),
        "headers": [[str(name), str(value)] for name, value in headers.items()],
        # The body travels BASE64-encoded, because a server answers BYTES: a response body is
        # an image, an archive or any other content that is not text, and encoding it is what
        # carries those bytes back unchanged.
        "body": base64.b64encode(bytes(body)).decode("ascii"),
    }


def _answer(server, request):
    """Hand ONE request to the server the program started, the way its own accept loop does."""
    received = _requested(request)
    if received is None:
        return {}
    connection = _Connection(received)
    try:
        server.finish_request(connection, _CLIENT_ADDRESS)
    except Exception:
        # The server's own error handling, which is what a real run does with a request its
        # handler raised on: the account of it goes to the program's own log.
        server.handle_error(connection, _CLIENT_ADDRESS)
    finally:
        server.shutdown_request(connection)
    return _answered(connection.sent())


def serve(event, context):
    """Run the program the platform starts and, for a run carrying a request, answer it."""
    _install()
    program = event["program"]
    filename = _TASK_ROOT + "/" + program["path"]
    scope = {
        "__name__": "__main__",
        "__file__": filename,
        "__doc__": None,
        "__package__": None,
        "__spec__": None,
        "__loader__": None,
    }
    try:
        exec(compile(program["source"], filename, "exec"), scope)
    except _Serving:
        pass
    server = _STARTED.get("server")
    request = event.get("request")
    if server is None or request is None:
        # A run with no request to serve, and a program that started no server at all: what
        # the program wrote while running is its output, and there is nothing else to report.
        return {}
    # What the program writes from here on it writes while SERVING this request, so the
    # platform's server captures it apart from the output of starting the program: it is the
    # output that one request produced, and it is attributable to it.
    served = io.StringIO()
    writing, failing = sys.stdout, sys.stderr
    sys.stdout = sys.stderr = served
    try:
        answered = _answer(server, request)
    except BaseException:
        # The program wrote this before it raised, and a run that answered nothing reports no
        # request's output: it belongs in the run's own log instead of being lost.
        writing.write(served.getvalue())
        raise
    finally:
        sys.stdout, sys.stderr = writing, failing
    if answered:
        answered["log"] = served.getvalue()
    return answered
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

    /// One start command and the program it runs: a row of
    /// [`the_program_is_the_file_the_command_runs`].
    struct ProgramRow {
        case: &'static str,
        /// The files the bundle carries.
        files: &'static [(&'static str, &'static [u8])],
        /// The command the platform starts the application with.
        command: &'static str,
        /// The file of the deployment that command runs.
        file: Option<&'static str>,
    }

    /// RULE: the program a start command runs is the file of the deployment the interpreter is
    /// handed — past the interpreter's own options, or the module `-m` names — and a command
    /// that runs no interpreter, or names a file the deployment does not carry, runs none.
    #[test]
    fn the_program_is_the_file_the_command_runs() {
        const SERVER: (&str, &[u8]) = ("server.py", b"print('up')\n");
        let cases: &[ProgramRow] = &[
            ProgramRow {
                case: "the interpreter over a file of the deployment",
                files: &[SERVER],
                command: "python3 server.py",
                file: Some("server.py"),
            },
            ProgramRow {
                case: "an option of the interpreter before the file",
                files: &[SERVER],
                command: "python3 -u server.py",
                file: Some("server.py"),
            },
            ProgramRow {
                case: "the interpreter under a versioned name, with a path of its own",
                files: &[SERVER],
                command: "/usr/bin/python3.11 -u -B ./server.py",
                file: Some("server.py"),
            },
            ProgramRow {
                case: "a file inside a directory of the deployment",
                files: &[("app/main.py", b"print('up')\n")],
                command: "python app/main.py",
                file: Some("app/main.py"),
            },
            ProgramRow {
                case: "a module run as a program",
                files: &[("app/serve.py", b"print('up')\n")],
                command: "python3 -m app.serve",
                file: Some("app/serve.py"),
            },
            ProgramRow {
                case: "a package run as a program",
                files: &[("app/__main__.py", b"print('up')\n")],
                command: "python3 -m app",
                file: Some("app/__main__.py"),
            },
            ProgramRow {
                case: "arguments of the program itself, after the file it runs",
                files: &[SERVER],
                command: "python3 server.py --port 8000",
                file: Some("server.py"),
            },
            ProgramRow {
                case: "a file the deployment does not carry",
                files: &[SERVER],
                command: "python3 absent.py",
                file: None,
            },
            ProgramRow {
                case: "a module the deployment does not carry",
                files: &[SERVER],
                command: "python3 -m absent",
                file: None,
            },
            ProgramRow {
                case: "a file that climbs out of the deployment",
                files: &[SERVER],
                command: "python3 ../server.py",
                file: None,
            },
            ProgramRow {
                case: "a command that runs no interpreter at all",
                files: &[SERVER],
                command: "gunicorn --bind :8000 server:application",
                file: None,
            },
            ProgramRow {
                case: "a command whose name only starts like the interpreter's",
                files: &[SERVER],
                command: "pythonic-tool server.py",
                file: None,
            },
            ProgramRow {
                case: "a command naming the interpreter and nothing else",
                files: &[SERVER],
                command: "python3 -u",
                file: None,
            },
        ];
        for row in cases {
            let case = row.case;
            let actual = python_program(&bundle(row.files), row.command);
            let expected = row.file.map(|file| PythonProgram {
                file: file.to_string(),
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
