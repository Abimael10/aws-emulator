// SPDX-License-Identifier: CC0-1.0

//! The CHILD-PROCESS half of this runtime's standard library: the `subprocess` module a
//! Python handler — or a package it vendors, or the standard library's own `asyncio` —
//! imports.
//!
//! A `python3.x` runtime is a promise about an interpreter AND about the standard library
//! that comes with it: a handler whose module body says `import subprocess`, or that imports
//! one of the standard library's own modules built on it (`asyncio`, whose process transport
//! is written against this module), imports cleanly on the runtime it was written for, and so
//! it has to here. A module of the standard library this emulator registers no extension for
//! is SUPPLIED on the import path instead of being absent from it, exactly as the socket
//! interface, the TLS layer, the waiting interface and the signal interface are
//! ([`super::guest_http::runtime_stdlib_modules`], [`super::guest_tls`],
//! [`super::guest_select`], [`super::guest_signal`], which this module's source is resolved
//! beside). What this runtime refuses is the CALL that needs a capability it does not have,
//! never the import — an import that fails is a runtime without that module, and it points
//! the developer at a deployment package that is not at fault.
//!
//! What is this module's own, and what it refuses:
//!
//! * the NAMES a program reads off `subprocess` — the three stream sentinels, the error
//!   classes it catches and the result object it unpacks — are all here, so a module that
//!   imports `subprocess` to name one of them, or to catch one of its errors, imports and
//!   runs;
//! * the pieces that are COMPUTATION are performed rather than refused: a result object
//!   reports what it was built with and checks its own exit status, an error renders its own
//!   message, and an argument sequence is quoted into one command line;
//! * STARTING a child is not, and no call here pretends otherwise: an invocation of this
//!   emulator runs in this process, with no host process of its own to fork and no host
//!   program to execute, so every call that would start one answers for a capability this
//!   runtime has none of — `OSError` carrying `ENOSYS`, the number a system call that is not
//!   there answers with.
//!
//! Nothing here invents a child: no exit status, no output and no process id is reported for
//! work nothing ran.

use super::package::PackageModule;

/// The modules of the standard library this half of the runtime supplies: the child-process
/// interface.
///
/// Resolved with the interfaces it is used beside, before the frozen standard library and
/// after the deployment package's own entries — so this IS the `subprocess` a handler
/// imports, the one the frozen `asyncio` is built against, and a package shipping its own
/// still wins.
#[must_use]
pub fn runtime_stdlib_modules() -> Vec<PackageModule> {
    vec![PackageModule {
        path: "subprocess.py".to_string(),
        source: SUBPROCESS_SOURCE.to_string(),
    }]
}

/// `subprocess`: the child-process interface of this runtime.
///
/// The surface is the standard library's, so a handler and the standard library's own modules
/// built on it read the names they expect; what no call synthesises is a CHILD — a call that
/// would start one says so, and the values a caller reads off a result object are the ones it
/// was built with.
const SUBPROCESS_SOURCE: &str = r#"
"""The child-process interface of this runtime: an invocation starts no process of its own."""

import errno

# What a caller passes for a stream it wants connected to a pipe, folded into the other
# stream, or thrown away, as this module numbers the three.
PIPE = -1
STDOUT = -2
DEVNULL = -3

# Said of a call that would start a child: an invocation of this emulator runs in this
# process, so there is no process to fork and no program to execute.
_NO_CHILD_PROCESS = "a Lambda function in this emulator starts no child process"


class SubprocessError(Exception):
    """The base of this module's own errors, which is what a caller catches."""


class CalledProcessError(SubprocessError):
    """A child that exited with a status its caller had asked to be checked."""

    def __init__(self, returncode, cmd, output=None, stderr=None):
        self.returncode = returncode
        self.cmd = cmd
        self.output = output
        self.stderr = stderr

    def __str__(self):
        if self.returncode and self.returncode < 0:
            return "Command '%s' died with signal %d." % (self.cmd, -self.returncode)
        return "Command '%s' returned non-zero exit status %d." % (self.cmd, self.returncode)

    @property
    def stdout(self):
        """The other name of `output`, as this class publishes both."""
        return self.output

    @stdout.setter
    def stdout(self, value):
        self.output = value


class TimeoutExpired(SubprocessError):
    """A child that was still running when the time its caller allowed it ran out."""

    def __init__(self, cmd, timeout, output=None, stderr=None):
        self.cmd = cmd
        self.timeout = timeout
        self.output = output
        self.stderr = stderr

    def __str__(self):
        return "Command '%s' timed out after %s seconds" % (self.cmd, self.timeout)

    @property
    def stdout(self):
        return self.output

    @stdout.setter
    def stdout(self, value):
        self.output = value


class CompletedProcess(object):
    """What a finished child came to: the arguments, the exit status and the output.

    Built by a caller out of values it already holds, and it reports exactly those — so this
    is the module's own object and no part of it is a reading of something that ran here.
    """

    def __init__(self, args, returncode, stdout=None, stderr=None):
        self.args = args
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

    def __repr__(self):
        parts = ["args=%r" % (self.args,), "returncode=%r" % (self.returncode,)]
        if self.stdout is not None:
            parts.append("stdout=%r" % (self.stdout,))
        if self.stderr is not None:
            parts.append("stderr=%r" % (self.stderr,))
        return "%s(%s)" % (type(self).__name__, ", ".join(parts))

    def check_returncode(self):
        """Raise this module's own error when the status is not success."""
        if self.returncode:
            raise CalledProcessError(self.returncode, self.args, self.stdout, self.stderr)


def list2cmdline(sequence):
    """One command line out of an argument sequence, quoted as this module quotes one.

    Text processing, so it is performed here rather than refused: a caller that builds a
    command line to log it, or to hand it to something else, gets the line it would get on
    the advertised runtime.
    """
    parts = []
    for argument in sequence:
        text = str(argument)
        if not text:
            parts.append('""')
            continue
        if not any(character in text for character in ' \t"'):
            parts.append(text)
            continue
        quoted = ['"']
        backslashes = 0
        for character in text:
            if character == "\\":
                backslashes += 1
                continue
            if character == '"':
                quoted.append("\\" * (backslashes * 2 + 1))
                quoted.append('"')
            else:
                quoted.append("\\" * backslashes)
                quoted.append(character)
            backslashes = 0
        quoted.append("\\" * (backslashes * 2))
        quoted.append('"')
        parts.append("".join(quoted))
    return " ".join(parts)


class Popen(object):
    """A child process, which this runtime starts none of.

    Constructing one is the call that would fork and execute, so it is where the capability
    is answered for: nothing is started, no process id is handed back and no stream is
    opened.
    """

    def __init__(self, args, *arguments, **keywords):
        raise OSError(errno.ENOSYS, _NO_CHILD_PROCESS)

    def __enter__(self):
        raise OSError(errno.ENOSYS, _NO_CHILD_PROCESS)

    def __exit__(self, *exception):
        return False


def run(*arguments, **keywords):
    """Start a child and wait for it, which is the capability there is none of."""
    raise OSError(errno.ENOSYS, _NO_CHILD_PROCESS)


def call(*arguments, **keywords):
    """The same, reporting the exit status."""
    raise OSError(errno.ENOSYS, _NO_CHILD_PROCESS)


def check_call(*arguments, **keywords):
    """The same, raising on a status that is not success."""
    raise OSError(errno.ENOSYS, _NO_CHILD_PROCESS)


def check_output(*arguments, **keywords):
    """The same, answering what the child wrote."""
    raise OSError(errno.ENOSYS, _NO_CHILD_PROCESS)


def getstatusoutput(cmd, *arguments, **keywords):
    """Run a command line through a shell, which is that same capability."""
    raise OSError(errno.ENOSYS, _NO_CHILD_PROCESS)


def getoutput(cmd, *arguments, **keywords):
    """The same, answering the output alone."""
    raise OSError(errno.ENOSYS, _NO_CHILD_PROCESS)
"#;
