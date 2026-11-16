// SPDX-License-Identifier: CC0-1.0

//! The SPARK runtime a submitted application finds on its import path.
//!
//! A `spark-submit` of a Python application runs that application in the contained Python
//! interpreter this workspace embeds ([`super::spark_step_run`]), and an application built on
//! Spark resolves its engine through the `pyspark` package. That package is the workspace's
//! ONE in-process Spark runtime ([`::service_common::spark_runtime`]), shared with every
//! other service whose data plane runs a caller-supplied Spark program, so what an
//! application computes here is what one computes there. What this module adds is the part
//! that is this service's own: the entry module that RUNS a submitted application
//! ([`APPLICATION_ENTRY_SOURCE`]).
//!
//! What those modules are, where their filesystem reads and writes land, and why neither the
//! bounds nor the containment live in them is stated by the shared module: both are the
//! interpreter's, in host code the application cannot reach (the deadline, the step budget,
//! the memory ceiling, the recursion limit and the output cap of [`super::spark_step_run`],
//! and the native-module allow-list of the engine itself).

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;

/// The modules this emulator puts on a submitted application's import path, as the
/// interpreter takes them: the shared `pyspark` package, and the entry module that RUNS the
/// application ([`APPLICATION_ENTRY_SOURCE`]).
///
/// `spark_version` is the version of Spark the cluster's own release ships, which is what the
/// application is told it is running on — `None` for a cluster whose release bundles no
/// Spark, where nothing is known to tell it.
#[must_use]
pub fn spark_runtime_modules(spark_version: Option<&str>) -> Vec<PackageModule> {
    ::std::iter::once((
        format!("{APPLICATION_ENTRY_MODULE}.py"),
        APPLICATION_ENTRY_SOURCE.to_string(),
    ))
    .chain(::service_common::spark_runtime::pyspark_modules(spark_version))
    .map(|(path, source)| PackageModule { path, source })
    .collect()
}

/// The module this emulator runs the submitted application FROM, and the function in it the
/// engine calls.
pub const APPLICATION_ENTRY_MODULE: &str = "__emr_spark_application";

/// How many characters of EACH console stream one run of an application keeps.
///
/// CHOSEN: a step's log file keeps at most [`crate::state::MAX_STEP_LOG_BYTES`] of a stream,
/// so keeping that many characters is everything a delivered file can carry, and what the run
/// answers with stays bounded whatever the application writes.
pub const APPLICATION_STREAM_CHARACTERS: usize = crate::state::MAX_STEP_LOG_BYTES;
/// See [`APPLICATION_ENTRY_MODULE`].
pub const APPLICATION_ENTRY_FUNCTION: &str = "run";
/// The handler the interpreter resolves: `<module>.<function>`.
#[must_use]
pub fn application_entry_handler() -> String {
    format!("{APPLICATION_ENTRY_MODULE}.{APPLICATION_ENTRY_FUNCTION}")
}

/// The entry module: it runs the application the way `spark-submit` runs one — as the
/// `__main__` module, with the application's path and its own arguments on `sys.argv` — and
/// reports what the run came to: the status it exited with, the two console streams it wrote,
/// and the exception it ended on when it ended on one.
///
/// The streams are reported SEPARATELY because that is what a step's log directory holds
/// ([`super::step_logs`]): a client-mode `spark-submit` writes the driver's own output to
/// standard output and the engine's logging and any traceback to standard error, so the two
/// are kept apart here rather than merged into one console. What each keeps is bounded
/// ([`APPLICATION_STREAM_CHARACTERS`]), so a chatty application cannot grow the answer without
/// limit.
///
/// Reporting rather than raising is how the runtime this interpreter serves reports an
/// unhandled exception too: the engine's own bounds are unaffected, because a run whose budget
/// was spent is reported as stopped whatever the guest answered.
const APPLICATION_ENTRY_SOURCE: &str = r#"
"""Runs a submitted Spark application, as the Python program it is."""

import sys
import traceback


class _Console:
    """One console stream of the application, bounded by what one run keeps of it."""

    def __init__(self, limit):
        self._parts = []
        self._kept = 0
        self._limit = limit

    def write(self, text):
        if not isinstance(text, str):
            text = str(text)
        room = self._limit - self._kept
        if room > 0:
            kept = text[:room]
            self._parts.append(kept)
            self._kept += len(kept)
        return len(text)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        return None

    def isatty(self):
        return False

    def text(self):
        return "".join(self._parts)


def run(event, context):
    out = _Console(event["streamCharacters"])
    err = _Console(event["streamCharacters"])
    held = (sys.stdout, sys.stderr)
    sys.stdout = out
    sys.stderr = err
    status = 0
    error_type = ""
    error_message = ""
    try:
        # The command line a Spark application resolves its arguments off: its own path
        # followed by the arguments the submit passed it, which is what spark-submit hands it.
        sys.argv = [event["path"]] + list(event["arguments"])
        globals_of_main = {"__name__": "__main__", "__file__": event["path"]}
        exec(compile(event["source"], event["path"], "exec"), globals_of_main)
    except SystemExit as ended:
        # sys.exit() is how a program names the status it exits with: no argument and None
        # are a clean exit, an integer is that status, and anything else is printed on
        # standard error and exits non-zero, exactly as the interpreter does with it.
        code = ended.code
        if code is None or code is False:
            status = 0
        elif code is True:
            status = 1
        elif isinstance(code, int):
            status = int(code)
        else:
            status = 1
            err.write("%s\n" % (code,))
    except BaseException as raised:
        # spark-submit writes the driver's traceback on standard error and exits non-zero,
        # which is what the step's own log files then carry.
        status = 1
        error_type = type(raised).__name__
        error_message = str(raised)
        trace = sys.exc_info()[2]
        # This module's own frame is not part of the application's traceback.
        if trace is not None and trace.tb_next is not None:
            trace = trace.tb_next
        err.write("".join(traceback.format_exception(type(raised), raised, trace)))
    finally:
        sys.stdout, sys.stderr = held
    return {
        "exitStatus": status,
        "stdout": out.text(),
        "stderr": err.text(),
        "errorType": error_type,
        "errorMessage": error_message,
    }
"#;
