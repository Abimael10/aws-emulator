// SPDX-License-Identifier: CC0-1.0

//! The standard-library surface a DEPLOYED SERVER is built on, inside the
//! interpreter this crate runs a deployment in.
//!
//! A program an Elastic Beanstalk platform starts from a bundle's `Procfile`
//! serves HTTP with the standard library's own server
//! (`crate::procfile_started_application` drives those servers end to end over
//! the wire). Two things have to hold inside the embedded interpreter for that to
//! be the program's to use: the connection interface those servers are built on
//! has to be importable, and the HEADER PARSER every one of them reads a request's
//! headers through has to be on the HTTP client the runtime supplies in place of
//! the frozen one — a module whose transport is this emulator's own seam rather
//! than a socket, and which carried no such parser.
//!
//! So this suite asserts those two directly, where a failure names the missing one
//! instead of showing up as an address that answers nothing: it runs a deployment
//! whose code reports what it found, and reads that report out of the run's own
//! output.

use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;

/// A deployment that reports, on its own standard output, what it found of that
/// surface: the connection interface, and the header parser reading a message off
/// a stream the way a server reads one off a connection.
const PROBE: &str = r#"
import io

try:
    import socketserver
    print("socketserver: %s found" % socketserver.StreamRequestHandler.__name__)
except BaseException as failure:
    print("socketserver: not importable: %s" % failure)

try:
    import http.client

    message = http.client.parse_headers(
        io.BytesIO(b"Host: example.test\r\nX-Probe: counted\r\n\r\n")
    )
    print(
        "parse_headers: host=%s probe=%s"
        % (message.get("Host"), message.get("X-Probe"))
    )
except BaseException as failure:
    print("parse_headers: %s: %s" % (type(failure).__name__, failure))

application = None
"#;

/// RULE: the interpreter a deployment runs in carries the connection interface the
/// standard library's servers are built on, and the header parser they read a
/// request's headers through — so a program the platform starts can serve HTTP
/// with the library it was written against.
#[test]
fn the_interpreter_carries_what_a_deployed_server_is_built_on() {
    let bundle = eb::state::ApplicationBundle {
        files: [("application.py".to_string(), PROBE.as_bytes().to_vec())]
            .into_iter()
            .collect(),
    };
    let modules = eb::state::python_modules(&bundle);
    let entry = eb::state::PythonEntry {
        module: "application".to_string(),
        callable: None,
    };
    // The interpreter is synchronous and its recursion limit is above what a test
    // thread's own stack holds, so the run happens on a thread of its own with the
    // stack this crate gives one ([`eb::state::PYTHON_APPLICATION_STACK_BYTES`]).
    let run = std::thread::Builder::new()
        .stack_size(eb::state::PYTHON_APPLICATION_STACK_BYTES)
        .spawn(move || eb::state::run_python_application(&entry, &modules, &[], None))
        .expect("a thread for the run")
        .join()
        .expect("the run's own thread finished");
    let reported: Vec<&str> = eb::state::application_log_lines(&run);
    for name in [
        "socketserver: StreamRequestHandler found",
        // The headers of the message, read back off the parsed message itself:
        // a parser that returned something other than those headers is no parser
        // a server could read a request through.
        "parse_headers: host=example.test probe=counted",
    ] {
        assert!(
            reported.contains(&name),
            "case {name:?}: expected it among what the run reported, got {reported:?} \
             and failure {:?}",
            run.failure
        );
    }
}
