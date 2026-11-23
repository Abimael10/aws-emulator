// SPDX-License-Identifier: CC0-1.0

//! The CHILD-PROCESS half of this runtime's standard library: the `subprocess` module a
//! Python handler — or a package it vendors, or the standard library's own `asyncio` —
//! imports, and the programs it starts.
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
//! beside).
//!
//! # What a call of it DOES
//!
//! A managed runtime's execution environment is an ordinary POSIX sandbox with a shell and the
//! usual utilities, and `/opt/bin` of a layer is on its `PATH`, so a handler that runs a
//! program — a utility of the sandbox, or an executable its own layer ships — gets that
//! program's output and its exit status. Every entry point here therefore STARTS the program
//! it was given: the call hands the program, its operands, the environment it runs in and the
//! FILES of this execution environment to the host, which resolves the program against that
//! environment's own `PATH` and mounts and performs it over that filesystem in this process
//! ([`super::guest_child_process`]), and what comes back is the exit status and the two
//! streams the program really produced. A program that wrote to the writable `/tmp` leaves
//! that file behind — the module's source applies those writes through the same ephemeral
//! storage the handler itself writes ([`super::guest_ephemeral`]), so a child's file is
//! charged against the size the function configured exactly as the handler's own is, and the
//! handler reads afterwards what its child wrote.
//!
//! What is this module's own:
//!
//! * the NAMES a program reads off `subprocess` — the three stream sentinels, the error
//!   classes it catches and the result object it unpacks — are all here, so a module that
//!   imports `subprocess` to name one of them, or to catch one of its errors, imports and
//!   runs;
//! * the pieces that are COMPUTATION are performed here rather than asked of the host: the
//!   arguments of each entry point are read as the standard library reads them, a result
//!   object reports what it was built with and checks its own exit status, an error renders
//!   its own message, and an argument sequence is quoted into one command line;
//! * a child's own streams: what it wrote is answered as the bytes or the text the caller
//!   asked for, and the input the caller writes to it is what the program reads.
//!
//! # What is still refused
//!
//! A program this emulator has no engine for — a native executable a layer ships, a program
//! written for another interpreter — is reported as that, through the call's own channel: no
//! exit status, no output and no process id is invented for work nothing performed. A program
//! the environment does not have at all is the `FileNotFoundError` the command search ending
//! with nothing raises, exactly as it does on the advertised runtime.
//!
//! # What a child of this environment reads
//!
//! The FILES the environment holds as text, which is what the shell performs work over: the
//! module hands the host the entries of the mounts and of the ephemeral storage whose bytes
//! decode, up to the bounds below. An entry whose bytes are NOT text is a file of the
//! environment to the handler itself ([`super::guest_mount`]) and is not in the tree a child
//! runs over, because nothing here runs a program that works over bytes.
//!
//! A child's own standard ERROR is kept where its caller asked for it (`stderr=PIPE` or
//! `STDOUT`) and is otherwise not kept at all; the `timeout` a caller names is never reached,
//! because a program performed in this process is finished when it answers.

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
/// built on it read the names they expect; what a call PERFORMS is one program of this
/// execution environment, and the values a caller reads off a result object are the ones that
/// program produced.
const SUBPROCESS_SOURCE: &str = r##"
"""The child-process interface of this runtime: one program of this execution environment."""

import errno
import io
import json
import os

_host = __import__("_lambda_host")

# What a caller passes for a stream it wants connected to a pipe, folded into the other
# stream, or thrown away, as this module numbers the three.
PIPE = -1
STDOUT = -2
DEVNULL = -3

# The roots of this execution environment's filesystem a child runs over: the read-only mounts
# a handler reads — the layer root and the task root — and the writable ephemeral storage
# beside them, which is the one place a child's writes are kept.
_TASK_ROOT = "/var/task"
_LAYER_ROOT = "/opt"
_TMP_ROOT = "/tmp"
_READ_ONLY_ROOTS = (_LAYER_ROOT, _TASK_ROOT)
_ROOTS = (_LAYER_ROOT, _TASK_ROOT, _TMP_ROOT)

# How much of that filesystem ONE child is handed: the entries are read out of this
# environment's own memory and handed over per child, so how many of them, how large one of
# them may be and how deep the walk goes are bounded here. A file past these bounds is not in
# the tree the program runs over, which is what the program reads as a file that is not there.
_MAX_TREE_FILES = 512
_MAX_TREE_BYTES = 2 * 1024 * 1024
_MAX_FILE_BYTES = 256 * 1024
_MAX_TREE_DEPTH = 32


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
    """A child that was still running when the time its caller allowed it ran out.

    A program of this environment is performed in process and is finished when it answers, so
    there is never a child still running for a caller to be waiting on: the class is here
    because a caller catches it, and nothing raises it.
    """

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
    """What a finished child came to: the arguments, the exit status and the output."""

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

    Text processing, so it is performed here: a caller that builds a command line to log it,
    or to hand it to something else, gets the line it would get on the advertised runtime.
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


# ----------------------------------------------------------------- reading what was passed


def _as_text(value):
    """`value` as the text a program is given: a path-like object as the path it names, bytes
    as the characters they spell, and anything else as what it says it is."""
    if isinstance(value, bytes):
        return value.decode("utf-8", "surrogateescape")
    if isinstance(value, str):
        return value
    try:
        named = os.fspath(value)
    except TypeError:
        return str(value)
    if isinstance(named, bytes):
        return named.decode("utf-8", "surrogateescape")
    return named


def _argv_of(args):
    """The program and its operands, as the standard library reads them: a single string (or
    bytes, or a path) names the program itself, and a sequence names it with its operands."""
    if isinstance(args, (str, bytes)) or hasattr(args, "__fspath__"):
        return [_as_text(args)]
    return [_as_text(word) for word in args]


def _command_line_of(args):
    """The command LINE a `shell=True` call carries: the string itself, or the first item of a
    sequence, which is "the command string"."""
    if isinstance(args, (str, bytes)) or hasattr(args, "__fspath__"):
        return _as_text(args)
    words = list(args)
    return _as_text(words[0]) if words else ""


def _wants_text(encoding, errors, text, universal_newlines):
    """Whether the caller asked for the streams as TEXT rather than as bytes, which is what
    any of those four says."""
    return bool(encoding or errors or text or universal_newlines)


# ------------------------------------------------------------- the environment's own files


def _resolved(path):
    """`path` as ONE absolute path with no empty, `.` or `..` segment left in it — `None` for
    anything that is not an absolute path at all.

    Every decision about WHERE a path is is made on this form and nothing else: a path is a
    key of this environment's own filesystem, and two spellings of one path must not be able
    to answer two different questions about which root it is under. `..` above the root
    resolves to the root, as it does on a machine.
    """
    if not isinstance(path, str) or not path.startswith("/"):
        return None
    walked = []
    for part in path.split("/"):
        if not part or part == ".":
            continue
        if part == "..":
            if walked:
                walked.pop()
            continue
        walked.append(part)
    return "/" + "/".join(walked)


def _under(path, root):
    """Whether the RESOLVED `path` is at or under the directory `root`.

    The caller resolves first ([_resolved]): this is a comparison of resolved paths, not a
    reading of whatever spelling something handed over.
    """
    held = root.rstrip("/")
    return path == held or path.startswith(held + "/")


def _in_storage(path):
    """The path of this environment's EPHEMERAL STORAGE that `path` names, and `None` for a
    path that names anything else.

    The storage is the one writable place a child's work is kept, so this is the gate every
    write and every removal of that work goes through — made on the resolved path, so no
    spelling of one reaches another root, and answered as the resolved path, which is what is
    then written.
    """
    resolved = _resolved(path)
    if resolved is None or not _under(resolved, _TMP_ROOT):
        return None
    return resolved


def _file_text(path):
    """What the file at `path` holds, as the text a program works over — `None` for a file
    whose bytes are not text, or one longer than one file of this tree may be, neither of
    which is something this environment's own shell performs work over."""
    try:
        with open(path, "rb") as stream:
            held = stream.read(_MAX_FILE_BYTES + 1)
    except OSError:
        return None
    except ValueError:
        return None
    if len(held) > _MAX_FILE_BYTES:
        return None
    try:
        return held.decode("utf-8")
    except UnicodeDecodeError:
        return None


def _walk_tree(path, files, directories, held, depth):
    """Collect the files and directories under `path`, in path order, within the bounds."""
    if depth <= 0:
        return
    try:
        names = sorted(os.listdir(path))
    except OSError:
        return
    directories.append(path)
    for name in names:
        entry = path.rstrip("/") + "/" + name
        if os.path.isdir(entry):
            _walk_tree(entry, files, directories, held, depth - 1)
            continue
        if len(files) >= _MAX_TREE_FILES or held[0] >= _MAX_TREE_BYTES:
            continue
        text = _file_text(entry)
        if text is None:
            continue
        held[0] = held[0] + len(text)
        files[entry] = text


def _environment_tree():
    """The filesystem of this execution environment a child runs over: the files and the
    directories of its mounts and of its ephemeral storage."""
    files = {}
    directories = []
    held = [0]
    for root in _ROOTS:
        _walk_tree(root, files, directories, held, _MAX_TREE_DEPTH)
    return files, directories


def _apply_writes(written, sent):
    """Put what the child WROTE on this environment's own ephemeral storage.

    Only `/tmp` is writable, so only what the program left there is applied, and which paths
    those are is decided on the RESOLVED path ([_in_storage]): a path that names anything else
    is not written and not removed, whatever it was spelled as. It is applied through the
    storage's own writes — so a child's file is charged against the size the function
    configured exactly as the handler's own file is. A write the storage will not take (it is
    full, or it holds a directory at that path) is one the child's work did not leave behind:
    there is nothing for this to report it to, and the next write of that path reports the
    storage's own refusal to whoever makes it.
    """
    kept = set()
    for path in sorted(written):
        resolved = _in_storage(path)
        if resolved is None:
            continue
        kept.add(resolved)
        text = written[path]
        if sent.get(resolved) == text:
            continue
        _put_file(resolved, text)
    for path in sorted(sent):
        resolved = _in_storage(path)
        if resolved is None or resolved in kept:
            continue
        try:
            os.remove(resolved)
        except OSError:
            pass


def _put_file(path, text):
    """Write `text` at `path` on the ephemeral storage, with the directories above it.

    `path` is a resolved path of that storage ([_in_storage]), which is the only form this is
    called with.
    """
    directory = path.rsplit("/", 1)[0]
    try:
        if directory and not os.path.isdir(directory):
            os.makedirs(directory)
        with open(path, "w") as stream:
            stream.write(text)
    except OSError:
        pass
    except ValueError:
        pass


# -------------------------------------------------------------------------- starting a child


def _not_started(failure):
    """The exception a call that started NO child raises, as the host reported it."""
    number = failure.get("errno")
    message = failure.get("message") or ""
    path = failure.get("path") or ""
    if number is None:
        return OSError(message)
    if number == errno.ENOENT:
        return FileNotFoundError(number, message, path) if path else FileNotFoundError(
            number, message
        )
    return OSError(number, message, path) if path else OSError(number, message)


def _performed(argv, line, executable, cwd, env, fed):
    """Perform one program of this execution environment and answer what it came to.

    `line` is the command line of a shell invocation and `argv` the program and its operands of
    every other one; `fed` is what the caller handed the child on its standard input.
    """
    files, directories = _environment_tree()
    asked = {
        "cwd": _as_text(cwd) if cwd else os.getcwd(),
        "environment": dict(os.environ if env is None else env),
        "files": files,
        "directories": directories,
        "readOnlyRoots": list(_READ_ONLY_ROOTS),
    }
    if line is not None:
        asked["program"] = line
    else:
        asked["argv"] = argv
    if executable:
        asked["executable"] = _as_text(executable)
    if fed:
        asked["input"] = fed
    answered = json.loads(_host.child(json.dumps(asked)))
    if not answered.get("started"):
        raise _not_started(answered.get("failure") or {})
    _apply_writes(answered.get("files") or {}, files)
    return answered


class Popen(object):
    """A child process: one program of this execution environment.

    The program is performed in this process, so a child is finished as soon as it has been
    started: `poll` answers its status, `wait` answers the same status without waiting for
    anything, and the streams carry exactly what it wrote. A child whose standard input the
    caller writes itself (`stdin=PIPE`) is performed when the caller is finished writing — on
    `communicate`, on a `wait` or when that stream is closed — because what the program reads
    is what was written to it.
    """

    def __init__(self, args, bufsize=-1, executable=None, stdin=None, stdout=None,
                 stderr=None, preexec_fn=None, close_fds=True, shell=False, cwd=None,
                 env=None, universal_newlines=None, startupinfo=None, creationflags=0,
                 restore_signals=True, start_new_session=False, pass_fds=(), *,
                 user=None, group=None, extra_groups=None, encoding=None, errors=None,
                 text=None, umask=-1, pipesize=-1, process_group=None):
        self.args = args
        self.returncode = None
        self.pid = None
        self.encoding = encoding
        self.errors = errors
        self.universal_newlines = universal_newlines
        self.stdout = None
        self.stderr = None
        self._text = _wants_text(encoding, errors, text, universal_newlines)
        self._line = _command_line_of(args) if shell else None
        self._argv = None if shell else _argv_of(args)
        self._executable = executable
        self._cwd = cwd
        self._env = env
        self._out_wanted = stdout == PIPE
        self._err_wanted = stderr == PIPE
        self._err_joined = stderr == STDOUT
        self._out = ""
        self._err = ""
        self._status = None
        self._started = False
        self.stdin = _ChildInput(self) if stdin == PIPE else None
        if self.stdin is None:
            # Nothing is to be written to it, so the program runs now.
            self._start(None)

    def _start(self, fed):
        """Perform the program, once."""
        if self._started:
            return
        self._started = True
        answered = _performed(
            self._argv, self._line, self._executable, self._cwd, self._env, fed
        )
        self.pid = answered.get("pid")
        self._status = answered.get("status")
        written = answered.get("stdout") or ""
        reported = answered.get("stderr") or ""
        if self._err_joined:
            # "stderr=STDOUT" folds the one stream into the other, so what the child reported
            # is part of what it wrote.
            written = written + reported
            reported = ""
        self._out = written
        self._err = reported
        if self._out_wanted:
            self.stdout = self._stream(written)
        if self._err_wanted:
            self.stderr = self._stream(reported)

    def _stream(self, text):
        """One of the child's streams as the caller reads it: text, or the bytes it wrote."""
        if self._text:
            return io.StringIO(text)
        return io.BytesIO(text.encode("utf-8", "surrogateescape"))

    def _value(self, text, wanted):
        """What `communicate` answers for one stream: `None` for a stream the caller did not
        ask to be given."""
        if not wanted:
            return None
        if self._text:
            return text
        return text.encode("utf-8", "surrogateescape")

    def _collected(self):
        """What the caller wrote to this child's standard input, if it has one."""
        return self.stdin.collected() if self.stdin is not None else None

    def communicate(self, input=None, timeout=None):
        """Hand the child its input and read back both of its streams."""
        fed = _as_text(input) if input is not None else self._collected()
        if self.stdin is not None:
            self.stdin.closed = True
        self._start(fed)
        self.returncode = self._status
        return (self._value(self._out, self._out_wanted),
                self._value(self._err, self._err_wanted))

    def poll(self):
        """The status of a child that has run, and `None` while none has been started."""
        if not self._started:
            return None
        self.returncode = self._status
        return self.returncode

    def wait(self, timeout=None):
        """The status the child exited with. Nothing is waited for: the program is performed
        in this process, so it is finished when it has been started."""
        self._start(self._collected())
        self.returncode = self._status
        return self.returncode

    def send_signal(self, signal):
        """Deliver a signal to the child, which has already run to completion: there is no
        process left to signal, and nothing of its outcome changes."""
        return None

    def terminate(self):
        """The same, for the signal that asks a child to end."""
        return None

    def kill(self):
        """The same, for the one it cannot catch."""
        return None

    def __enter__(self):
        return self

    def __exit__(self, *exception):
        if self.stdin is not None:
            self.stdin.close()
        self.wait()
        return False


class _ChildInput(object):
    """The standard input of a child the caller writes itself (`stdin=PIPE`).

    What is written here is what the program reads, so the program is performed when the
    caller is finished writing: closing this stream starts it.
    """

    def __init__(self, child):
        self._child = child
        self._written = []
        self.closed = False

    def write(self, data):
        if self.closed:
            raise ValueError("I/O operation on closed file")
        text = _as_text(data)
        self._written.append(text)
        return len(data)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def collected(self):
        return "".join(self._written)

    def flush(self):
        return None

    def writable(self):
        return True

    def readable(self):
        return False

    def seekable(self):
        return False

    def close(self):
        if self.closed:
            return
        self.closed = True
        self._child._start(self.collected())


# ------------------------------------------------------------------------- the entry points


def run(*popenargs, input=None, capture_output=False, timeout=None, check=False, **keywords):
    """Run the program and wait for it, answering what it came to."""
    if input is not None:
        if keywords.get("stdin") is not None:
            raise ValueError("stdin and input arguments may not both be used.")
        keywords["stdin"] = PIPE
    if capture_output:
        if keywords.get("stdout") is not None or keywords.get("stderr") is not None:
            raise ValueError(
                "stdout and stderr arguments may not be used with capture_output."
            )
        keywords["stdout"] = PIPE
        keywords["stderr"] = PIPE
    with Popen(*popenargs, **keywords) as process:
        written, reported = process.communicate(input, timeout=timeout)
        status = process.poll()
    completed = CompletedProcess(process.args, status, written, reported)
    if check:
        completed.check_returncode()
    return completed


def call(*popenargs, timeout=None, **keywords):
    """Run the program and answer the status it exited with."""
    with Popen(*popenargs, **keywords) as process:
        return process.wait(timeout=timeout)


def check_call(*popenargs, **keywords):
    """The same, raising this module's own error on a status that is not success."""
    status = call(*popenargs, **keywords)
    if status:
        arguments = keywords.get("args")
        if arguments is None:
            arguments = popenargs[0] if popenargs else None
        raise CalledProcessError(status, arguments)
    return 0


def check_output(*popenargs, timeout=None, **keywords):
    """The same, answering what the child wrote."""
    if "stdout" in keywords:
        raise ValueError("stdout argument not allowed, it will be overridden.")
    if "input" in keywords and keywords["input"] is None:
        keywords["input"] = "" if _wants_text(
            keywords.get("encoding"),
            keywords.get("errors"),
            keywords.get("text"),
            keywords.get("universal_newlines"),
        ) else b""
    return run(
        *popenargs, stdout=PIPE, timeout=timeout, check=True, **keywords
    ).stdout


def getstatusoutput(cmd, *, encoding=None, errors=None):
    """Run a command line through the environment's shell, answering its status and what it
    wrote on either stream."""
    try:
        written = check_output(
            cmd, shell=True, text=True, stderr=STDOUT, encoding=encoding, errors=errors
        )
        status = 0
    except CalledProcessError as failed:
        written = failed.output
        status = failed.returncode
    if written[-1:] == "\n":
        written = written[:-1]
    return status, written


def getoutput(cmd, *, encoding=None, errors=None):
    """The same, answering what it wrote alone."""
    return getstatusoutput(cmd, encoding=encoding, errors=errors)[1]
"##;
