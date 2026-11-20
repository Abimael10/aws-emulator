// SPDX-License-Identifier: CC0-1.0

//! The RUNTIME the handler runs under, in the language the handler is written in.
//!
//! A Python runtime's job is small and entirely defined by what the handler sees: import
//! the handler's module out of the deployment package, build the context object, call the
//! handler with the event and the context, and report back either what it returned or the
//! error document Lambda answers for an exception. That is this source: it runs INSIDE the
//! interpreter [`super::python`] builds, because every one of those steps is a Python one —
//! `importlib` resolves the module, `json` reads the event and writes the response, and
//! `traceback` renders the stack of an exception the handler raised.
//!
//! What it is NOT is the containment. Nothing here is a security boundary: a module the
//! guest must not reach is not registered by the host at all ([`super::python`]), so this
//! source cannot be made to hand one over by deleting or rebinding a name in it. The two
//! host functions it does use are the ones the host deliberately exposes — the log the
//! handler's output goes into, and the bounded sleep — and both are enforced in host code.
//! No bound of the run is reachable from here either: this source neither starts nor moves
//! the deadline the handler's run is stopped at, which is why the protocol below is TWO
//! calls the host makes rather than one.
//!
//! The protocol between host and runtime is a document in and two functions out, so the
//! host's own interpreter surface stays as small as a string in and a string out:
//!
//! * `__lambda_input` — a JSON document the host hands to the first call below: the
//!   handler name, the package's modules and its own data files, the content the function's
//!   LAYERS mount, the modules the RUNTIME supplies beside them (the AWS SDK), the event and
//!   the environment the invocation runs with.
//! * `__lambda_prepare` — the INIT phase: it installs this runtime, imports the handler's
//!   module and resolves its entry point, answering the empty string when the environment
//!   is ready and an error document for an initialization that failed. None of that is the
//!   handler's run, so none of it is bounded by the function's `Timeout` — it is bounded by
//!   the span an initialization is allowed instead. That bound is the HOST's and is in force
//!   before this source is evaluated at all: the trace hook that spends the run's deadline,
//!   step count and memory ceiling is installed before `__lambda_prepare` is called, and the
//!   deadline it holds during this phase is the initialization span plus the function's
//!   `Timeout` ([`super::python`], [`super::INIT_PHASE_LIMIT`]), with the host's own watch
//!   sampling that deadline from outside the guest. So module-level code of the deployment
//!   package — a busy loop, an unbounded allocation, a blocking construct — is stopped on
//!   those bounds like any other guest frame, reaches no bound of its own and cannot wedge
//!   the interpreter: the phase simply has a longer span than one invocation, not an
//!   unbounded one.
//! * `__lambda_bind` — the NEXT invocation's own request, put on the prepared environment:
//!   the event, the request id, the ARN the call addressed, the client context, the tenant
//!   and the environment the invocation runs with. It answers the empty string when the
//!   request is bound and an error document when it could not be. An execution environment
//!   serves invocation after invocation, so what one invocation brings arrives here rather
//!   than in the initialization above — and the logging handler is re-installed with this
//!   invocation's request id, so every record a run leaves names the invocation that wrote
//!   it. The payload is unmarshalled and the context object built here too: both are the
//!   runtime's own work on receiving an invocation, so neither is charged to the handler's
//!   own run.
//! * `__lambda_invoke` — the handler's own run, which the host calls next, having begun the
//!   invoke phase itself and passing the instant the `Timeout` is up at. It answers a JSON
//!   document carrying what the handler returned, or the error it raised.
//!
//! Nothing here imports an operating-system module: the environment the handler sees is
//! installed by the HOST into the module `os` reads it from, and the context's members are
//! read out of the document above rather than out of a module this source would otherwise
//! have to reach for. What the handler reads as the FILES of its execution environment is a
//! source of its own ([`super::guest_mount`]), evaluated into this scope before this one and
//! handed the same mounts the finders here import from.

/// The Python source of the runtime, evaluated once per execution environment.
///
/// It opens with the FILESYSTEM of the execution environment ([`super::guest_mount`]), which
/// `_prepare` below installs the mounts through: the two are one compiled unit, so the names
/// that section leaves behind are the runtime's own.
pub(super) const RUNTIME_SOURCE: &str = super::guest_mount::mount_source!(
    r##"
import sys

# Everything this runtime itself needs, imported once, before the handler's own work
# begins and before the bounds that measure that work are in place.
import datetime
import importlib
import importlib.util
import io
import json
import linecache
import logging
import time
import traceback

_host = __import__("_lambda_host")

# What the INIT phase resolved, for the INVOKE phase the host calls next: the request
# document, this runtime's own library directory and the handler's entry point.
_PREPARED = {}


class _LogStream:
    """One of the handler's output streams, writing into the invocation's log.

    The interpreter is built without the host's own standard streams, so this is the
    only `sys.stdout`/`sys.stderr` a handler has: `print()` and the logging handler this
    runtime installs (`_install_logging`) reach the invocation's log through it, and
    nothing reaches this process's stdio.
    """

    encoding = "utf-8"
    errors = "backslashreplace"
    name = "<log>"
    mode = "w"
    closed = False

    def write(self, text):
        if not isinstance(text, str):
            text = str(text)
        _host.log(text)
        return len(text)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        return None

    def isatty(self):
        return False

    def readable(self):
        return False

    def writable(self):
        return True

    def seekable(self):
        return False

    def fileno(self):
        raise OSError("the invocation log has no file descriptor")

    def close(self):
        return None


sys.stdout = _LogStream()
sys.stderr = _LogStream()
sys.stdin = None


class _RecordStream:
    """The stream this runtime's own logging handler writes through.

    A record the handler formats is a WHOLE record — its message and, where the record
    carries an exception, that exception's frames — so it is handed to the platform as one
    record and reaches the function's log group as one event. A write made straight to
    `sys.stdout` (`print()`, a handler writing bytes itself) is framed by the platform at
    its line breaks instead, which is what `_LogStream` above leaves it to.
    """

    encoding = "utf-8"
    errors = "backslashreplace"
    name = "<record>"
    mode = "w"
    closed = False

    def write(self, text):
        if not isinstance(text, str):
            text = str(text)
        _host.record(text)
        return len(text)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        return None

    def isatty(self):
        return False

    def readable(self):
        return False

    def writable(self):
        return True

    def seekable(self):
        return False

    def fileno(self):
        raise OSError("the invocation log has no file descriptor")

    def close(self):
        return None


# The value of `AWS_LAMBDA_LOG_FORMAT` whose records are STRUCTURED, which is the member
# `LoggingConfig.LogFormat` carries: "the format in which Lambda sends your function's
# application and system logs to CloudWatch. Select between plain text and structured JSON".
_JSON_LOG_FORMAT = "JSON"

# How this runtime renders the records of the invocation it is serving: the format the
# function is configured for, and the invocation every record names. Kept here because a
# record is rendered wherever one is written — the logging handler below, and the report this
# runtime writes for an unhandled exception.
_LOGGING = {"json": False, "requestId": None}


def _instant(created):
    """``created``, a POSIX timestamp, in the form a record of this runtime states an
    instant in: UTC to the millisecond.

    The ONE rendering, so the instants a run reports cannot drift between the records the
    logging handler writes and the report this runtime writes for an unhandled exception.
    """

    whole = int(created)
    millis = min(999, int((created - whole) * 1000.0))
    at = datetime.datetime.utcfromtimestamp(whole)
    return "%04d-%02d-%02dT%02d:%02d:%02d.%03dZ" % (
        at.year,
        at.month,
        at.day,
        at.hour,
        at.minute,
        at.second,
        millis,
    )


class _LambdaLogFormatter(logging.Formatter):
    """How this runtime renders ONE logging record into the invocation's log.

    The managed runtime's own text rendering of a record: the level in brackets, the
    instant the record was created in UTC to the millisecond, the invocation's request id
    and the message, tab separated. So a handler that logs through `logging` leaves lines
    a client can read the level and the invocation off, exactly as `print()` leaves the
    text it was given.
    """

    def __init__(self, request_id):
        logging.Formatter.__init__(self)
        self._request_id = request_id

    def formatTime(self, record, datefmt=None):
        return _instant(record.created)

    def exception(self, record):
        """The class, the message and the frames of the exception a record carries."""

        if not record.exc_info:
            return None
        kind, raised, trace = record.exc_info
        return (
            getattr(kind, "__name__", str(kind)),
            str(raised),
            traceback.format_exception(kind, raised, trace),
        )

    def format(self, record):
        message = record.getMessage()
        failure = self.exception(record)
        if failure is not None:
            message = "%s\n%s" % (message, "".join(failure[2]).rstrip("\n"))
        return "[%s]\t%s\t%s\t%s" % (
            record.levelname,
            self.formatTime(record),
            self._request_id,
            message,
        )


class _LambdaJsonLogFormatter(_LambdaLogFormatter):
    """The same record under the STRUCTURED log format: one JSON object.

    A function whose `LogFormat` is JSON has its application records delivered as JSON
    objects rather than in the text framing above, so a client reads the level, the message
    and the invocation off named members instead of off tab-separated fields: the level is
    the emitting logger's own level NAME, the message is the formatted line, the logger is
    the one that emitted the record, the request id is the invocation's, and the timestamp is
    the instant the record was created. A record carrying an exception names its class and
    its frames beside them, which is what makes a failure readable as a record rather than as
    free text.
    """

    def format(self, record):
        document = {
            "timestamp": self.formatTime(record),
            "level": record.levelname,
            "message": record.getMessage(),
            "logger": record.name,
            "requestId": self._request_id,
        }
        failure = self.exception(record)
        if failure is not None:
            document["errorType"] = failure[0]
            document["errorMessage"] = failure[1]
            document["stackTrace"] = failure[2]
        return json.dumps(document)


def _install_logging(request_id, log_format, log_level):
    """Give the root logger the handler the managed runtime gives it.

    A record emitted through `logging` reaches no output on its own: with no handler on
    the root logger the library falls back to one that takes nothing below a warning and
    renders the bare message, so a handler logging at the level it set its logger to
    would leave nothing in the function's log group. The managed runtime installs a
    handler instead, which is what makes `logging.getLogger().setLevel(logging.INFO)`
    followed by `logger.info(...)` appear in the log — so this runtime installs one too,
    writing into the invocation's own output.

    The FORMAT is the function's own, which the invocation's environment states: the text
    framing of the managed runtime, or the structured record its JSON format delivers.

    The LEVEL is the function's own too, where it has one: `ApplicationLogLevel` filters the
    application logs "to the selected level of detail and lower", and a managed runtime
    applies that by setting the ROOT logger to the level the invocation's environment states
    — which is what makes a function configured at a level see that level's records and
    nothing more detailed. A function that states no level leaves the root logger exactly as
    the library's own default (a warning), because that is where the managed runtime leaves
    it: what a record then has to clear is the level the handler's own code set.

    Installed before the handler's module is imported, so a record a module emits while it
    is being imported is carried too, and installed as the ONLY handler, so a handler
    module calling `logging.basicConfig()` adds none beside it and no record is written
    twice.
    """

    json_records = log_format == _JSON_LOG_FORMAT
    _LOGGING["json"] = json_records
    _LOGGING["requestId"] = request_id
    handler = logging.StreamHandler(_RecordStream())
    formatter = _LambdaJsonLogFormatter if json_records else _LambdaLogFormatter
    handler.setFormatter(formatter(request_id))
    root = logging.getLogger()
    for installed in list(root.handlers):
        root.removeHandler(installed)
    root.addHandler(handler)
    level = _LOG_LEVELS.get(str(log_level or "").upper())
    if level is not None:
        root.setLevel(level)


# What each `ApplicationLogLevel` is as a level of the logging library, in the order the
# member documents them — "TRACE is the highest level and FATAL is the lowest". The library
# names four of the six; the two it does not are the level below its lowest (a trace is more
# detailed than a debug record) and its own highest (a fatal record is a critical one).
#
# Source: the model's `ApplicationLogLevel` enum (TRACE | DEBUG | INFO | WARN | ERROR |
# FATAL) against the levels `logging` declares.
_LOG_LEVELS = {
    "TRACE": logging.DEBUG - 5,
    "DEBUG": logging.DEBUG,
    "INFO": logging.INFO,
    "WARN": logging.WARNING,
    "ERROR": logging.ERROR,
    "FATAL": logging.CRITICAL,
}


def _configured_log_format(environment):
    """The log format the invocation's own environment states, as the runtime reads it."""

    return (environment or {}).get("AWS_LAMBDA_LOG_FORMAT") or ""


def _configured_log_level(environment):
    """The application log level the invocation's own environment states, as the runtime
    reads it."""

    return (environment or {}).get("AWS_LAMBDA_LOG_LEVEL") or ""


# The MOUNTS a module one run imports can come from, and the DIRECTORY each of them is
# mounted at: the deployment package at the task root, the content of the function's layers
# under the layer root, and the modules the runtime itself supplies under its own
# directories. Which mount a module came from is what its `__file__` states — its path under
# that mount — because a `sys.path` entry is a directory and a module is the file of it that
# the entry resolved to.
_TASK_ROOT = "/var/task"
_LAYER_ROOT = "/opt"
_RUNTIME_ROOT = "/var/runtime"


class _PackageLoader:
    """Imports one module of ONE mount of the execution environment, held in memory.

    `root` is the directory that mount is mounted at, so the file the module reports — and
    the file a frame of it names — is its own path under the directory it was resolved
    from, exactly as it is on a real runtime.
    """

    def __init__(self, path, sources, root):
        self._path = path
        self._sources = sources
        self._root = root

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        source = self._sources[self._path]
        filename = self._root + "/" + self._path
        module.__dict__["__file__"] = filename
        # The source is held in memory, not on a filesystem, so the lookup a traceback
        # does for the SOURCE LINE of a frame — `linecache`, which stats the file the
        # frame names — would find nothing and the frame would be rendered as its location
        # alone. Seeding the cache with the lines this mount holds under that name is what
        # makes a frame of this module read the way the managed runtime renders it: the
        # location line and the source line at it.
        linecache.cache[filename] = (
            len(source),
            None,
            source.splitlines(True),
            filename,
        )
        code = compile(source, filename, "exec")
        exec(code, module.__dict__)

    def get_source(self, name):
        return self._sources[self._path]

    def is_package(self, name):
        return self._path.endswith("/__init__.py")

    def get_resource_reader(self, name):
        """What the standard library reads this module's own FILES through.

        A package's data file is held beside its modules (`_RESOURCE_PREFIX`), so the
        resources of a package are the held entries under its own directory, and those of
        a module are the ones beside it — which is the directory each of them is at under
        the mount the module was resolved from.
        """
        directory = self._path.rpartition("/")[0]
        return _PackageResources(self._sources, directory, self._root)


# The prefix the host keys a DATA entry of the deployment package under, which is where the
# files a package ships beside its modules are held. No module name resolves to one of
# them: `_PackageFinder` resolves a name to `<name>.py` or `<name>/__init__.py`.
_RESOURCE_PREFIX = "resource:"


def _entry_path(held):
    """The path inside the package of the held entry `held`.

    A module is held under its own path and a data file under the prefix above, and both of
    them are FILES of the package: a module's path is a file beside the others, which is
    what a package shipping its modules and its data in one archive holds.
    """
    if held.startswith(_RESOURCE_PREFIX):
        return held[len(_RESOURCE_PREFIX):]
    return held


def _mount_entries(sources):
    """The FILES one mount holds: every held entry at its own path inside the mount.

    A module and a data file are both files of the mount, so the prefix a data entry is held
    under is taken off here: what the filesystem of the execution environment serves is paths
    ([`super::guest_mount`]), and the prefix is this runtime's own way of keeping a data entry
    where no module name resolves to it.
    """
    return {_entry_path(held): sources[held] for held in sources}


class _PackageFile:
    """One PATH inside one mount of the execution environment, as the standard library's own
    resource reading traverses one.

    The entries are the ones the host read out of the archive, so a file this answers for
    is one the mount really holds and one it does not hold reads as absent. Nothing here
    opens a host file: a file's text is the text the archive carried. The path it NAMES is
    its path under the directory the mount is mounted at (`root`).
    """

    def __init__(self, sources, path, root):
        self._sources = sources
        self._path = path
        self._root = root

    def __repr__(self):
        return "<PackageFile %r>" % (self._root + "/" + self._path,)

    @property
    def name(self):
        return self._path.rpartition("/")[2]

    def _held(self):
        """The key this path's own text is held under, or `None` when none is."""
        for key in (_RESOURCE_PREFIX + self._path, self._path):
            if key in self._sources:
                return key
        return None

    def is_file(self):
        return self._held() is not None

    def is_dir(self):
        under = self._path + "/" if self._path else ""
        return not self.is_file() and any(
            _entry_path(held).startswith(under) for held in self._sources
        )

    def iterdir(self):
        under = self._path + "/" if self._path else ""
        named = []
        for held in self._sources:
            path = _entry_path(held)
            if not path.startswith(under):
                continue
            entry = path[len(under):].partition("/")[0]
            if entry and entry not in named:
                named.append(entry)
        for entry in named:
            yield self.joinpath(entry)

    def joinpath(self, *names):
        path = self._path
        for name in names:
            for part in str(name).split("/"):
                if part:
                    path = path + "/" + part if path else part
        return _PackageFile(self._sources, path, self._root)

    def __truediv__(self, name):
        return self.joinpath(name)

    def _bytes(self):
        held = self._held()
        if held is None:
            raise FileNotFoundError(self._root + "/" + self._path)
        return self._sources[held].encode("utf-8")

    def open(self, mode="r", encoding=None, errors=None, newline=None):
        held = self._bytes()
        if "b" in mode:
            return io.BytesIO(held)
        return io.TextIOWrapper(io.BytesIO(held), encoding, errors, newline)

    def read_bytes(self):
        return self._bytes()

    def read_text(self, encoding=None, errors=None, newline=None):
        with self.open("r", encoding, errors, newline) as reading:
            return reading.read()


class _PackageResources:
    """The FILES of one module of one mount, as the import system asks the loader for them.

    It answers both ways the standard library asks: `files()` for the traversable the
    current resource reading walks, and the older by-name calls beside it.
    """

    def __init__(self, sources, directory, root):
        self._sources = sources
        self._directory = directory
        self._root = root

    def files(self):
        return _PackageFile(self._sources, self._directory, self._root)

    def open_resource(self, resource):
        return self.files().joinpath(resource).open("rb")

    def resource_path(self, resource):
        # A file of this runtime is held in memory, so it is at no path of the host a
        # caller could open for itself.
        raise FileNotFoundError(self._root + "/" + self._directory)

    def is_resource(self, path):
        return self.files().joinpath(path).is_file()

    def contents(self):
        return [found.name for found in self.files().iterdir()]


class _PackageFinder:
    """Resolves a module name against the entries of ONE mount, whose own directory is
    `root`.

    The deployment package's mount is installed ahead of every other finder, so the
    handler's own modules win over a standard-library module of the same name, exactly as
    the package directory does on the real runtime's `sys.path`; a mount the RUNTIME
    supplies is installed behind the ones above it, in the order that same path lists the
    directories.

    `spec_from_loader` is handed in rather than imported here: this runs INSIDE the
    import machinery, so an import of its own would re-enter the machinery.
    """

    def __init__(self, sources, spec_from_loader, root):
        self._sources = sources
        self._spec_from_loader = spec_from_loader
        self._root = root

    def find_spec(self, name, path=None, target=None):
        key = name.replace(".", "/")
        for candidate, is_package in ((key + "/__init__.py", True), (key + ".py", False)):
            if candidate in self._sources:
                loader = _PackageLoader(candidate, self._sources, self._root)
                return self._spec_from_loader(name, loader, is_package=is_package)
        return None


def _layer_import_roots(library):
    """The DIRECTORIES of the layer root a python runtime puts on its import path, in the
    order `sys.path` lists them.

    A layer's content is extracted under the layer root, and two of its directories are
    import path entries for a python runtime: the site-packages directory of the runtime's
    OWN version, then the version-independent one beside it. The version is the one the
    runtime's own library directory is named for (`_runtime_library`), so a site-packages
    directory named for another version is on no path at all and a package shipped only
    there is not importable.
    """

    version = library.rpartition("/")[2]
    roots = []
    if version:
        roots.append("python/lib/" + version + "/" + _SITE_DIRECTORY)
    roots.append("python")
    return roots


# The name of a SITE directory: the one directory of an installation whose path-configuration
# files are processed, which is what lets a dependency layer put its packages somewhere else
# and still have them importable.
_SITE_DIRECTORY = "site-packages"

# The suffix of a path-configuration file, as site processing looks for one.
_PATH_CONFIGURATION_SUFFIX = ".pth"


def _search_path(library, layer_roots):
    """The DIRECTORIES of this execution environment's import path, in the order a managed
    runtime lists them on `sys.path`.

    The task root the deployment package is mounted at comes first, so the function's own
    modules win over everything; then the import directories of the layer root
    (`_layer_import_roots`); then the runtime's own module directory and the library directory
    of the language installation it ships. Every entry is a directory this environment really
    has, which is what the mounts are.
    """

    path = [_TASK_ROOT]
    path += [_LAYER_ROOT + "/" + root for root in layer_roots]
    path.append(_RUNTIME_ROOT)
    if library:
        path.append(library)
    return path


def _add_site_directories(roots):
    """Process the path-configuration files of the layer root's import directories, extending
    `roots` and answering the directories they added, as absolute paths.

    Each import directory of the layer root is a SITE directory of this environment, so the
    `.pth` files in it are read in the order of their names, and each line of one is either an
    `import` statement site processing executes or a directory it adds to the path — which is
    the layout a dependency layer that installs its packages elsewhere relies on. A directory
    the mounts hold nothing at is not added, exactly as site processing adds only a directory
    that exists; one that is under the layer root becomes an import directory of that root as
    well, so a module shipped there is importable and not merely listed. The directories added
    are not themselves site directories, so their own `.pth` files are not processed, which is
    where site processing stops too.
    """

    added = []
    for root in list(roots):
        directory = _LAYER_ROOT + "/" + root
        names = [
            name
            for name in _mount_names(directory)
            if name.endswith(_PATH_CONFIGURATION_SUFFIX)
        ]
        for name in sorted(names):
            for line in (_mount_text(directory + "/" + name) or "").splitlines():
                entry = _site_entry(line, directory)
                if entry is None or entry in added:
                    continue
                added.append(entry)
                if entry.startswith(_LAYER_ROOT + "/"):
                    roots.append(entry[len(_LAYER_ROOT) + 1:])
    return added


def _site_entry(line, directory):
    """The directory one line of a path-configuration file adds, or `None` when it adds none.

    A blank line and a comment add nothing; an `import` line is a statement site processing
    executes, and a line that fails is reported and does not stop the rest of the file, which
    is how site processing treats one.
    """

    text = line.strip()
    if not text or text.startswith("#"):
        return None
    if text.startswith("import ") or text.startswith("import\t"):
        try:
            exec(text, {})
        except BaseException as error:
            sys.stderr.write(
                "[WARNING] a path configuration line could not be processed: %s: %s\n"
                % (type(error).__name__, error)
            )
        return None
    # The one collapse of `.` and `..` this environment has is the filesystem's own
    # ([`super::guest_mount`]), so a line is resolved through it rather than through a second
    # copy here: a line that walks out of the mounts therefore names a path of no mount, which
    # holds no directory to add.
    resolved = _mount_resolved(text if text.startswith("/") else directory + "/" + text)
    if resolved is None:
        return None
    return resolved if _mount_is_directory(resolved) else None


class _LayerFinder:
    """Resolves a module name against the content the function's LAYERS mount.

    The entries are keyed by their own path under the layer root, because that is where a
    layer's content is extracted to, and only the directories of that root which are import
    path entries (`roots`) resolve a module at all — an entry of the layer outside them is
    content of the layer and no module of the run.

    Installed behind the deployment package's own mount and ahead of every mount the
    RUNTIME supplies, which is where those directories sit on a real runtime's `sys.path`:
    a layer that ships its own copy of a package the runtime bundles shadows the runtime's,
    and only the function's own package wins over a layer's.
    """

    def __init__(self, sources, roots, spec_from_loader):
        self._sources = sources
        self._roots = roots
        self._spec_from_loader = spec_from_loader

    def find_spec(self, name, path=None, target=None):
        key = name.replace(".", "/")
        for root in self._roots:
            for tail, is_package in ((key + "/__init__.py", True), (key + ".py", False)):
                candidate = root + "/" + tail
                if candidate in self._sources:
                    loader = _PackageLoader(candidate, self._sources, _LAYER_ROOT)
                    return self._spec_from_loader(name, loader, is_package=is_package)
        return None


class CognitoIdentity:
    """The IDENTITY the invocation was made with, as `context.identity`.

    An object with both members present whether or not an identity was supplied, which is
    the interface a handler reads them through: a handler of a function invoked without an
    Amazon Cognito identity reads ``None`` off each member rather than having nothing to
    read them off at all.
    """

    __slots__ = ("cognito_identity_id", "cognito_identity_pool_id")

    def __init__(self):
        self.cognito_identity_id = None
        self.cognito_identity_pool_id = None


class Client:
    """The mobile CLIENT a client context names, as `context.client_context.client`."""

    __slots__ = (
        "installation_id",
        "app_title",
        "app_version_name",
        "app_version_code",
        "app_package_name",
    )

    def __init__(self):
        for member in self.__slots__:
            setattr(self, member, None)


class ClientContext:
    """The CLIENT CONTEXT the caller sent, as `context.client_context`.

    The members are the three a client context document carries, read off that document by
    name and ``None`` for one it does not carry — an OBJECT rather than the document
    itself, because a handler reads `client_context.custom` rather than indexing a mapping.
    """

    __slots__ = ("custom", "env", "client")

    def __init__(self):
        for member in self.__slots__:
            setattr(self, member, None)


def _object_from(class_, document):
    """One object of ``class_`` carrying what ``document`` holds for each of its members.

    ``None`` for a document that is not there at all, which is what a handler reads when
    the invocation carried none.
    """

    if document is None:
        return None
    built = class_()
    for member in class_.__slots__:
        setattr(built, member, document.get(member))
    return built


class _ContextFault(Exception):
    """A failure of the CONTEXT itself, under the class the runtime reports it as.

    Raised where a member of the invocation cannot be read — a client context that is not
    JSON — so the invocation is refused with that class before the handler is called.
    """

    def __init__(self, error_type, error_message):
        Exception.__init__(self, error_message)
        self.error_type = error_type
        self.error_message = error_message


def _client_context(text):
    """The client context object for the document ``text`` holds, or ``None`` for none.

    The runtime PARSES the document the caller sent: an invocation carrying one that is not
    JSON is refused under the runtime's own class for it, before the handler runs.
    """

    if not text:
        return None
    try:
        document = json.loads(text)
    except BaseException as error:
        raise _ContextFault(
            "Runtime.LambdaContextUnmarshalError",
            "Unable to parse Client Context JSON: %s" % error,
        )
    built = _object_from(ClientContext, document)
    if built is not None:
        built.client = _object_from(Client, built.client)
    return built


class LambdaContext:
    """The context object the handler is called with.

    Every member is read from the ENVIRONMENT of the invocation, which is where the
    runtime interface reads them from: the function's name, version, memory size and log
    destination are environment variables of the invocation. The host hands that
    environment in, so this runtime needs no operating-system module of its own.

    The identity and the client context are OBJECTS exposing their own members
    (`CognitoIdentity`, `ClientContext`), which is the interface a handler reads them
    through.
    """

    def __init__(self, request, environment):
        self.aws_request_id = request["requestId"]
        self.invoked_function_arn = request["invokedFunctionArn"]
        self.log_group_name = environment.get("AWS_LAMBDA_LOG_GROUP_NAME")
        self.log_stream_name = environment.get("AWS_LAMBDA_LOG_STREAM_NAME")
        self.function_name = environment.get("AWS_LAMBDA_FUNCTION_NAME")
        self.function_version = environment.get("AWS_LAMBDA_FUNCTION_VERSION")
        self.memory_limit_in_mb = environment.get("AWS_LAMBDA_FUNCTION_MEMORY_SIZE")
        self.identity = CognitoIdentity()
        self.client_context = _client_context(request["clientContext"])
        self.tenant_id = request["tenantId"]
        # When this invocation's own run is up, which the host answers when the invoke
        # phase begins: the execution environment's initialization is not part of it, and
        # neither is the runtime's own work of binding this invocation, so the handler is
        # told it has its whole configured timeout left when it is entered.
        self._deadline_epoch_ms = 0

    def bind_deadline(self, deadline_epoch_ms):
        """Hand the context the instant this invocation's own run is up at.

        Called once, by this runtime, as the handler's run is begun: the deadline is the
        HOST's to decide, and until it is handed over the context reports no time left —
        the invocation's own run has not started.
        """

        self._deadline_epoch_ms = deadline_epoch_ms

    def get_remaining_time_in_millis(self):
        return max(0, int(self._deadline_epoch_ms - time.time() * 1000.0))

    def log(self, message):
        sys.stdout.write(message)


def _report(error_type, error_message, stack_trace):
    """Write the error report this runtime writes for an unhandled exception.

    The runtime reports an exception on the invocation's stderr as well as answering the
    error document, so the invocation's LOG carries what went wrong: the error's type and
    message, then the handler's own traceback. That is what a client reads out of the
    function's log group for a failed invocation.

    It is ONE record: the report is written in a single write, so the whole of it — the
    `[ERROR]` line, the traceback header and every frame — is one event of the function's
    log group rather than one event per line of it, which is what lets a client read a
    failure as the one thing that happened. The frames keep their INDENTATION as
    non-breaking spaces, which is what the managed runtime writes them with, so the shape of
    the traceback survives a reader that collapses runs of ordinary spaces.

    Under the STRUCTURED log format the same report is ONE record of that format instead:
    the class, the message and the frames become the members of a single JSON object,
    because a client that selected that format reads its function's log as records.
    """
    if _LOGGING["json"]:
        _RecordStream().write(
            json.dumps(
                {
                    "timestamp": _instant(time.time()),
                    "level": "ERROR",
                    "message": "%s: %s" % (error_type, error_message),
                    "requestId": _LOGGING["requestId"],
                    "errorType": error_type,
                    "errorMessage": error_message,
                    "stackTrace": [frame.rstrip("\n") for frame in stack_trace],
                }
            )
            + "\n"
        )
        return
    lines = [
        "[ERROR] %s: %s" % (error_type, error_message),
        "Traceback (most recent call last):",
    ]
    for frame in stack_trace:
        for line in frame.rstrip("\n").split("\n"):
            lines.append(_indented(line))
    # ONE record the RUNTIME framed, and no break of its own: the report is one record,
    # whose last line is its last line ([`_RecordStream`]).
    _RecordStream().write("\n".join(lines))


# The space the frames of a reported traceback are indented with: a non-breaking one, which
# is what the managed runtime writes the indentation of an error report as.
_REPORT_INDENT = "\u00a0"


def _indented(line):
    """``line`` with its leading spaces written as the space above, one for one — so the
    frame keeps the depth the traceback states it at and nothing else about the line
    changes."""

    stripped = line.lstrip(" ")
    return _REPORT_INDENT * (len(line) - len(stripped)) + stripped


def _raised(error_type, error_message, stack_trace):
    _report(error_type, error_message, stack_trace)
    return {
        "outcome": "raised",
        "errorType": error_type,
        "errorMessage": error_message,
        "stackTrace": stack_trace,
    }


def _runtime_library(environment):
    """The directory the modules of the RUNTIME ITSELF are reported under.

    A managed runtime keeps the standard library it ships under the library directory of
    its own language installation, named for the runtime the function is configured with
    — which the invocation's environment states, as ``AWS_EXECUTION_ENV`` is
    ``AWS_Lambda_<runtime>``.
    """

    marker = "AWS_Lambda_"
    execution_env = environment.get("AWS_EXECUTION_ENV") or ""
    if not execution_env.startswith(marker):
        return ""
    return "/var/lang/lib/" + execution_env[len(marker):]


def _frame_file(filename, library):
    """The FILE a traceback frame names.

    A frame of the deployment package's own module already names its path under the task
    root, and a frame the interpreter renders as a pseudo-file (``<string>``) has no path
    to name. Everything else is a module of the runtime's own standard library, whose
    frame names that module's source file under the runtime's library directory rather
    than the bare module name it was compiled under.
    """

    if not library or filename.startswith("/") or filename.startswith("<"):
        return filename
    path = filename[:-3] if filename.endswith(".py") else filename
    return "%s/%s.py" % (library, path.replace(".", "/"))


def _frames(trace, library):
    """One rendered frame per entry of ``trace``, as the error document carries them.

    A frame is its location line and the source line at that location, and nothing else:
    the fine-grained error-location anchors (the ``~`` and ``^`` run under the source
    line) this interpreter can render are not part of the frame the runtime reports.
    """

    rendered = []
    for frame in traceback.extract_tb(trace):
        text = '  File "%s", line %s, in %s\n' % (
            _frame_file(frame.filename, library),
            frame.lineno,
            frame.name,
        )
        if frame.line:
            text += "    %s\n" % frame.line.strip()
        rendered.append(text)
    return rendered


def _bounded_sleep(seconds):
    return _host.sleep(float(seconds))


def _built_in_modules():
    """The modules the RUNTIME ITSELF is built with, under the names a handler would give
    for them.

    The interpreter reports the modules compiled into it (``sys.builtin_module_names``),
    and the interpreter's own two are built in whether or not it lists them there: no file
    supplies ``sys`` or ``builtins``, so a handler naming either names the interpreter's
    module rather than anything of the deployment package.
    """

    return frozenset(sys.builtin_module_names) | {"builtins", "sys"}


def _prepare(request, library):
    """The INIT phase: everything that has to happen before the handler can be called.

    Answers ``None`` once the handler's module is imported and its entry point resolved,
    and an error document for an initialization that failed. What it resolved is kept for
    [_invoke], which the HOST calls next: the phase boundary is the host's to decide, so
    nothing in here — and nothing the deployment package can call — says when the
    handler's own `Timeout` starts running.
    """
    sources = request["modules"]
    layer_sources = request.get("layerModules") or {}
    runtime_stdlib = request.get("runtimeStdlib") or {}
    runtime_sources = request.get("runtimeModules") or {}
    # The FILESYSTEM of the execution environment, built before any of the function's own code
    # is read: every mount's entries at the directory that mount is mounted at, so a file the
    # deployment package or one of the function's layers ships is an ordinary readable file to
    # the handler ([`super::guest_mount`]). The same held entries are what the finders below
    # import from, so a module and a file are two readings of one entry.
    _mount_install(
        [
            (_TASK_ROOT, _mount_entries(sources)),
            (_LAYER_ROOT, _mount_entries(layer_sources)),
            (library or _RUNTIME_ROOT, _mount_entries(runtime_stdlib)),
            (_RUNTIME_ROOT, _mount_entries(runtime_sources)),
        ],
        time.time(),
    )
    # The import directories of the LAYER ROOT, which the finder below resolves a layer's
    # modules from and which site processing extends further down — one list, so a directory a
    # path-configuration file adds is both on the path and importable.
    layer_roots = _layer_import_roots(library)
    # The MOUNTS a module is resolved from, in the order a real runtime's `sys.path` lists
    # their directories: the deployment package at the task root first, so the handler's own
    # modules win over everything; then the content the function's LAYERS mount under the
    # layer root; then the modules of the STANDARD LIBRARY this runtime supplies itself — the
    # socket interface a Python program expects of its operating system, and the HTTP client
    # built on one — which are resolved ahead of the frozen standard library and behind the
    # two above, so a package or a layer shipping its own copy of one of them wins.
    mounts = [_PackageFinder(sources, importlib.util.spec_from_loader, _TASK_ROOT)]
    if layer_sources:
        mounts.append(
            _LayerFinder(
                layer_sources,
                layer_roots,
                importlib.util.spec_from_loader,
            )
        )
    if runtime_stdlib:
        mounts.append(
            _PackageFinder(
                runtime_stdlib,
                importlib.util.spec_from_loader,
                library or _RUNTIME_ROOT,
            )
        )
    for index, finder in enumerate(mounts):
        sys.meta_path.insert(index, finder)
    # The modules the RUNTIME supplies — the AWS SDK a managed runtime ships, which it keeps
    # in a directory of its own rather than in the function's — are resolved AFTER the
    # deployment package's own entries, after the layers' content and after the standard
    # library, so a package or a layer shipping its own copy of one of them wins, exactly as
    # the task root and the layer root precede the runtime's directories on a real runtime's
    # `sys.path`.
    runtime_sources = request.get("runtimeModules") or {}
    if runtime_sources:
        sys.meta_path.append(
            _PackageFinder(
                runtime_sources, importlib.util.spec_from_loader, _RUNTIME_ROOT
            )
        )
    # The import PATH the handler reads off `sys.path`: the directories of the mounts above,
    # in the order a managed runtime lists them. It is set once the finders are standing,
    # because site processing comes next and an `import` line of a path-configuration file is
    # resolved against them.
    sys.path[:] = _search_path(library, layer_roots)
    # The SITE directories of the layer root, processed as an installation's are: a
    # path-configuration file there extends the path with the directories it names, which is
    # what a dependency layer that installs its packages elsewhere relies on. They go at the
    # END of the path, where site processing appends them.
    sys.path.extend(_add_site_directories(layer_roots))
    time.sleep = _bounded_sleep
    # Before the handler's module is imported, which is where a module-level record is
    # emitted from.
    _install_logging(
        request["requestId"],
        _configured_log_format(request.get("environment")),
        _configured_log_level(request.get("environment")),
    )

    handler = request["handler"]
    module_name, separator, attribute = handler.rpartition(".")
    if not separator or not module_name or not attribute:
        return _raised(
            "Runtime.MalformedHandlerName",
            "Bad handler '%s': not enough values to unpack (expected 2, got 1)" % handler,
            [],
        )

    # A handler naming a module the interpreter itself is built with is refused before the
    # import is attempted: such a name resolves to the runtime's own module rather than to
    # anything of the deployment package, so the runtime reports the conflict instead of
    # importing it.
    if module_name.partition(".")[0] in _built_in_modules():
        return _raised(
            "Runtime.BuiltInModuleConflict",
            "Cannot use built-in module %s as a handler module" % module_name,
            [],
        )

    try:
        module = importlib.import_module(module_name)
    except ImportError as error:
        # A module that is not THERE, under the name the handler gave for it.
        return _raised(
            "Runtime.ImportModuleError",
            "Unable to import module '%s': %s" % (module_name, error),
            [],
        )
    except SyntaxError as error:
        # A module that is there and does not COMPILE, which is the package's own syntax
        # rather than a missing module.
        return _raised(
            "Runtime.UserCodeSyntaxError",
            "Syntax error in module '%s': %s" % (module_name, error),
            [],
        )
    except BaseException as error:
        # The module imported and its own top-level code raised: the failure is the
        # package's own exception, so it is reported under that exception's class rather
        # than as a module that could not be imported.
        return _raised(type(error).__name__, str(error), [])

    try:
        entry = getattr(module, attribute)
    except AttributeError:
        return _raised(
            "Runtime.HandlerNotFound",
            "Handler '%s' missing on module '%s'" % (attribute, module_name),
            [],
        )

    _PREPARED["request"] = request
    _PREPARED["library"] = library
    _PREPARED["entry"] = entry
    return None


def _invoke(deadline_epoch_ms):
    """The INVOKE phase: the handler's own run, which is what the ``Timeout`` bounds.

    ``deadline_epoch_ms`` is when that ``Timeout`` is up at, as the host decided it when it
    began this phase; it is what the context reports as the time remaining. Everything the
    runtime itself had to do for this invocation — unmarshalling the payload, building the
    context — is already done ([_bind]), so what this phase holds is the handler's own work
    and the handler is entered with its whole timeout left.
    """
    library = _PREPARED["library"]
    entry = _PREPARED["entry"]
    if "context" not in _PREPARED:
        return _raised(
            "Runtime.InternalError",
            "the execution environment has no bound invocation to run",
            [],
        )
    event = _PREPARED["event"]
    context = _PREPARED["context"]
    context.bind_deadline(deadline_epoch_ms)

    try:
        returned = entry(event, context)
    except BaseException as error:
        trace = error.__traceback__
        # The frame this runtime called the handler from is not part of the handler's
        # own stack, so the trace starts at the handler.
        if trace is not None:
            trace = trace.tb_next
        return _raised(
            type(error).__name__,
            str(error),
            _frames(trace, library) if trace is not None else [],
        )

    try:
        payload = json.dumps(returned)
    except BaseException as error:
        return _raised(
            "Runtime.MarshalError",
            "Unable to marshal response: %s" % error,
            [],
        )
    return {"outcome": "returned", "payload": payload}


def _main_prepare(payload):
    try:
        request = json.loads(payload)
    except BaseException as error:
        return json.dumps(_raised("Runtime.InternalError", str(error), []))
    # Where this runtime's OWN modules are, which a frame of one of them names.
    library = _runtime_library(request.get("environment") or {})
    _PREPARED["library"] = library
    try:
        outcome = _prepare(request, library)
    except BaseException as error:
        return json.dumps(
            _raised(
                type(error).__name__,
                str(error),
                _frames(error.__traceback__, library),
            )
        )
    if outcome is not None:
        return json.dumps(outcome)
    # Nothing to report: the initialization is done and the host may begin the invoke
    # phase.
    return ""


def _bind(request):
    """Put ONE invocation's own request on the prepared environment.

    An execution environment is initialized once and then serves invocation after
    invocation, so what the handler is called with — the event, the request id, the ARN
    the call addressed, the client context, the tenant and the environment of the
    invocation — is bound here rather than by the initialization. The logging handler is
    re-installed with this invocation's request id, so a record carries the invocation
    that wrote it and not the one that happened to initialize the environment.

    The payload is UNMARSHALLED and the context object BUILT here as well, because both are
    the runtime's own work on receiving an invocation rather than part of the handler's run:
    a payload that is not the JSON an invocation carries fails as this runtime's unmarshal
    error, and the handler itself is entered in the next phase with its whole timeout left.
    """
    prepared = _PREPARED.get("request")
    if prepared is None:
        return _raised(
            "Runtime.InternalError",
            "the execution environment has no prepared request to bind an invocation on",
            [],
        )
    for member in ("event", "requestId", "invokedFunctionArn", "clientContext", "tenantId"):
        prepared[member] = request.get(member)
    environment = request.get("environment")
    if environment is not None:
        prepared["environment"] = environment
    _install_logging(
        prepared["requestId"],
        _configured_log_format(prepared.get("environment")),
        _configured_log_level(prepared.get("environment")),
    )
    _PREPARED.pop("event", None)
    _PREPARED.pop("context", None)

    event_text = prepared["event"]
    try:
        event = json.loads(event_text) if event_text else {}
    except BaseException as error:
        return _raised(
            "Runtime.UnmarshalError", "Unable to unmarshal input: %s" % error, []
        )

    try:
        context = LambdaContext(prepared, prepared["environment"])
    except _ContextFault as fault:
        return _raised(fault.error_type, fault.error_message, [])

    _PREPARED["event"] = event
    _PREPARED["context"] = context
    return None


def _main_bind(payload):
    library = _PREPARED.get("library") or ""
    try:
        request = json.loads(payload)
    except BaseException as error:
        return json.dumps(_raised("Runtime.InternalError", str(error), []))
    try:
        outcome = _bind(request)
    except BaseException as error:
        return json.dumps(
            _raised(
                type(error).__name__,
                str(error),
                _frames(error.__traceback__, library),
            )
        )
    if outcome is not None:
        return json.dumps(outcome)
    # Nothing to report: the invocation is bound and the host may begin the invoke phase.
    return ""


def _main_invoke(deadline_epoch_ms):
    library = _PREPARED.get("library") or ""
    try:
        return json.dumps(_invoke(deadline_epoch_ms))
    except BaseException as error:
        return json.dumps(
            _raised(
                type(error).__name__,
                str(error),
                _frames(error.__traceback__, library),
            )
        )


# The three functions this source leaves behind, all called by the HOST: the first runs the
# init phase, the second binds one invocation's own request onto the prepared environment,
# and the third runs the handler itself. Nothing above has run any of the caller's own code:
# the imports and definitions above are this runtime's own setup.
__lambda_prepare = _main_prepare
__lambda_bind = _main_bind
__lambda_invoke = _main_invoke
"##
);
