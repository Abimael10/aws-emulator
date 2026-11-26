// SPDX-License-Identifier: CC0-1.0

//! The PYTHON a MicroVM's guest runs inside: the modules this emulator supplies on the
//! import path, and the driver the host calls into the interpreter.
//!
//! The interpreter registers no native module that reaches the host
//! ([`super::ALLOWED_NATIVE_MODULES`]), so the modules a Python program expects of its
//! operating system have nothing to be built on: each is supplied HERE, in Python, and each
//! reaches only what this emulator holds —
//!
//! * [`POSIX_SOURCE`] is the module `os` builds itself out of: the variables, the working
//!   directory and the identity the MicroVM published to its guest, and the metadata of the
//!   MicroVM's own in-memory machine ([`super::super::guest_machine`]), reached through the
//!   host seam;
//! * [`FILES_SOURCE`] is the `open` a guest calls: the files of that same machine, and no
//!   path of the host;
//! * [`SOCKET_SOURCE`] is the module an outbound connection is opened through: WHERE a
//!   connection goes is the host's decision, taken from the account's own connectors,
//!   subnets and security groups ([`super::super::microvm_network`]), and no host socket
//!   exists at all;
//! * [`SOCKETSERVER_SOURCE`] is the module a program binds a port through: a bound server is
//!   REGISTERED rather than given a socket of this machine, and the request the MicroVM's
//!   endpoint carries is handed to the handler class exactly as the standard library's own
//!   server hands it one;
//! * [`SELECT_SOURCE`] is the waiting interface the standard library names: there is no
//!   descriptor of the host to wait on, so every call of it raises;
//! * [`STREAMS_SOURCE`] is what `print` and a server's own log line are written to: the
//!   MicroVM's log, and nothing of this process's standard streams.
//!
//! The REQUEST and the RESPONSE are not this emulator's work: the standard library's own
//! `http.server` parses the one and renders the other, on top of the two modules above, so
//! what a client reads back is what the guest's own program produced.
//!
//! [`RUNTIME_SOURCE`] is the driver: it makes the image's own modules importable from
//! memory, starts the entry module as `__main__` — which is how the image's command starts
//! it — and hands one proxied request at a time to the server the program bound.

/// The name the host functions are reachable under, which every module here imports them by.
/// It is not in the native module table ([`super::ALLOWED_NATIVE_MODULES`]), so there is no
/// second module of this name to be had.
pub(super) const HOST_MODULE: &str = "_microvm_host";

/// `posix`: the operating system a MicroVM's guest runs on.
pub(super) const POSIX_SOURCE: &str = r#"
"""The operating system a MicroVM's guest runs on.

`os` builds itself out of the names it finds on the module it is built on, so what a guest's
`os` can do is exactly what is here: the variables its MicroVM published to it, the directory
its command runs in, the identity of its single process, and the entries of the MicroVM's own
in-memory machine, reached through the host seam.

No name here reaches a file, a descriptor, a process, a user or the clock accounting of the
host this emulator runs on, and no such name EXISTS on this module — a program naming one
reads the `AttributeError` a platform without it answers, and `os` builds itself without the
calls that would have needed it.
"""

import _microvm_host as _host
import _random as _generator
import errno as _errno

# `os.error`, and the list `os` builds its `supports_dir_fd` and `supports_follow_symlinks`
# sets out of: EMPTY, which is what says that no call of this platform takes a directory
# descriptor or follows a symbolic link — there is no host descriptor and no host link.
error = OSError
_have_functions = []

# The access, open and seek flags a program names, as `<fcntl.h>` and `<unistd.h>` number
# them on this platform (the values Python's own module publishes). DATA: naming one opens
# nothing.
F_OK = 0
X_OK = 1
W_OK = 2
R_OK = 4
O_RDONLY = 0
O_WRONLY = 1
O_RDWR = 2
O_CREAT = 64
O_EXCL = 128
O_NOCTTY = 256
O_TRUNC = 512
O_APPEND = 1024
O_NONBLOCK = 2048
O_DIRECTORY = 65536
O_CLOEXEC = 524288
SEEK_SET = 0
SEEK_CUR = 1
SEEK_END = 2
EX_OK = 0

# The identity the MicroVM published to its guest: the environment, the working directory and
# the process id. The HOST installs each of them from the MicroVM's own machine before a line
# of guest code runs; what stands here is what a machine that published none would have.
environ = {}
_cwd = "/"
_pid = 1


def getcwd():
    """The directory the guest's command runs in, as its MicroVM started it."""
    return _cwd


def getcwdb():
    return _cwd.encode("utf-8", "surrogateescape")


def getpid():
    """The identity of the MicroVM's single guest process."""
    return _pid


def getppid():
    """The process that started it: the MicroVM's own init, which is process 1."""
    return 1


def cpu_count():
    """How many processors a MicroVM's guest runs on: ONE. Its program is executed on a
    single thread of this process and nothing of it runs in parallel, so one is what a
    program sizing a pool to the machine must read."""
    return 1


def process_cpu_count():
    return cpu_count()


def sched_getaffinity(pid):
    """The processors that one processor is, numbered from zero."""
    return {0}


def isatty(fd):
    """A MicroVM's guest has no terminal: its standard streams are its MicroVM's log."""
    return False


def device_encoding(fd):
    """No descriptor of this platform is a terminal, so none carries an encoding."""
    return None


def fspath(path):
    """The string or bytes `path` names, which is how the standard library's own path
    handling takes a path."""
    if isinstance(path, (str, bytes)):
        return path
    held = getattr(type(path), "__fspath__", None)
    if held is None:
        raise TypeError(
            "expected str, bytes or os.PathLike object, not " + type(path).__name__
        )
    named = held(path)
    if isinstance(named, (str, bytes)):
        return named
    raise TypeError("expected __fspath__ to return str or bytes")


def urandom(size):
    """Random bytes, from the interpreter's own generator in this process: no device and no
    file of the host is opened for them."""
    held = _generator.Random()
    return bytes(held.getrandbits(8) for _ in range(int(size)))


# The file-type bits of a mode, as `<sys/stat.h>` defines them: what `stat` reports a path of
# the machine as, and what the standard library's `stat` module reads back off it.
_S_IFDIR = 0o040000
_S_IFREG = 0o100000
_DIRECTORY_MODE = _S_IFDIR | 0o755
_FILE_MODE = _S_IFREG | 0o644


class stat_result(tuple):
    """What a path of the MicroVM's machine reports, in the shape `os.stat` answers one."""

    def __new__(cls, fields):
        return tuple.__new__(cls, tuple(fields)[:10])

    @property
    def st_mode(self):
        return self[0]

    @property
    def st_ino(self):
        return self[1]

    @property
    def st_dev(self):
        return self[2]

    @property
    def st_nlink(self):
        return self[3]

    @property
    def st_uid(self):
        return self[4]

    @property
    def st_gid(self):
        return self[5]

    @property
    def st_size(self):
        return self[6]

    @property
    def st_atime(self):
        return self[7]

    @property
    def st_mtime(self):
        return self[8]

    @property
    def st_ctime(self):
        return self[9]


def stat(path, *, dir_fd=None, follow_symlinks=True):
    """What the MicroVM's machine holds at `path`.

    The answer is the machine's own (`guest_machine.rs`, through the host seam); a path it
    holds nothing at raises exactly what a filesystem raises for one.
    """
    named = _as_text(path)
    held = _host.fs_stat(named)
    if held is None:
        raise FileNotFoundError(_errno.ENOENT, "No such file or directory", named)
    directory, size = held
    mode = _DIRECTORY_MODE if directory else _FILE_MODE
    # The machine records no modification time for an entry, so the times of a reading are
    # the epoch rather than an instant this emulator made up for it.
    return stat_result((mode, 0, 0, 1, 0, 0, int(size), 0, 0, 0))


def lstat(path, *, dir_fd=None):
    """The same reading: the machine holds no symbolic link for one to differ on."""
    return stat(path, dir_fd=dir_fd)


def mkdir(path, mode=0o777, *, dir_fd=None):
    """Make a directory of the MicroVM's machine."""
    named = _as_text(path)
    if not _host.fs_mkdir(named):
        raise OSError(_errno.EACCES, "Permission denied", named)


def remove(path, *, dir_fd=None):
    """Remove a file of the MicroVM's machine."""
    named = _as_text(path)
    if not _host.fs_remove(named):
        raise FileNotFoundError(_errno.ENOENT, "No such file or directory", named)


unlink = remove


def rmdir(path, *, dir_fd=None):
    """Remove a directory of the machine, which it holds as the path it is."""
    remove(path)


def _as_text(path):
    """`path` as the text the machine keys its entries under."""
    named = fspath(path)
    if isinstance(named, bytes):
        return named.decode("utf-8", "surrogateescape")
    return named


def putenv(name, value):
    """A variable the guest sets: it lives in the mapping its MicroVM holds (`environ`,
    which `os.environ` writes through), and there is no host environment to export it to."""
    return None


def unsetenv(name):
    """The same, the other way round: the mapping is what holds the guest's variables."""
    return None


# The names `os` looks for BEYOND the ones above, every one of which lands on a descriptor, a
# process or a user of the host where a real system answers it.
#
# Each one EXISTS here — `os` builds itself out of the names it finds, so an absent one
# changes what `os` offers — and each one RAISES: a MicroVM's guest in this emulator has no
# host descriptor, no host process and no host user. Nothing here is a stub that pretends to
# have done the work, and the names that would end THIS PROCESS (`_exit`, `abort`) or start
# or signal another (`fork`, `execv`, `posix_spawn`, `kill`) raise for the same reason as the
# rest: one guest call that ended this process would take every other MicroVM of this
# emulator with it.
_REFUSED = (
    "_exit",
    "abort",
    "access",
    "chdir",
    "chmod",
    "chown",
    "chroot",
    "close",
    "closerange",
    "confstr",
    "ctermid",
    "dup",
    "dup2",
    "dup3",
    "execv",
    "execve",
    "fchdir",
    "fchmod",
    "fchown",
    "fdatasync",
    "fexecve",
    "fork",
    "forkpty",
    "fpathconf",
    "fstat",
    "fstatvfs",
    "fsync",
    "ftruncate",
    "get_blocking",
    "get_inheritable",
    "get_terminal_size",
    "getegid",
    "geteuid",
    "getgid",
    "getgrouplist",
    "getgroups",
    "getlogin",
    "getpriority",
    "getrandom",
    "getresgid",
    "getresuid",
    "getsid",
    "getuid",
    "initgroups",
    "kill",
    "killpg",
    "lchown",
    "link",
    "listdir",
    "lockf",
    "lseek",
    "major",
    "makedev",
    "minor",
    "mkfifo",
    "mknod",
    "nice",
    "open",
    "openpty",
    "pathconf",
    "pipe",
    "pipe2",
    "posix_spawn",
    "posix_spawnp",
    "pread",
    "pwrite",
    "read",
    "readlink",
    "readv",
    "rename",
    "replace",
    "scandir",
    "sched_yield",
    "sendfile",
    "set_blocking",
    "set_inheritable",
    "setegid",
    "seteuid",
    "setgid",
    "setgroups",
    "setpgid",
    "setpgrp",
    "setpriority",
    "setregid",
    "setresgid",
    "setresuid",
    "setreuid",
    "setsid",
    "setuid",
    "spawnv",
    "spawnve",
    "statvfs",
    "symlink",
    "sync",
    "sysconf",
    "system",
    "tcgetpgrp",
    "tcsetpgrp",
    "times",
    "truncate",
    "ttyname",
    "umask",
    "utime",
    "wait",
    "wait3",
    "wait4",
    "waitpid",
    "waitstatus_to_exitcode",
    "write",
    "writev",
)


def _refusing(name):
    """The call `name` is answered with: the refusal of a platform that has no such thing."""

    def refused(*_arguments, **_keywords):
        raise OSError(
            _errno.EPERM,
            "a MicroVM guest in this emulator has no host descriptor, process or user to "
            + name,
        )

    refused.__name__ = name
    return refused


for _name in _REFUSED:
    globals()[_name] = _refusing(_name)
del _name
"#;

/// The files a MicroVM's guest opens: the MicroVM's own machine.
pub(super) const FILES_SOURCE: &str = r#"
"""The files a MicroVM's guest opens.

Opening a file of the HOST is refused outright — the interpreter's own file type and every
name that reaches it answer that refusal (`guest_python.rs`) — and what `open` reaches
instead is the MicroVM's own in-memory machine (`guest_machine.rs`, through the host seam):
the entries its image's build steps wrote, the files its guest writes itself, and nothing
else. No path of this process's filesystem resolves, whatever a guest names.
"""

import _microvm_host as _host
import errno as _errno
import io


class _MachineFile(io.RawIOBase):
    """One open file of the MicroVM's machine: its bytes while it is open, written back
    through the host seam when it is flushed or closed."""

    def __init__(self, path, data, writing, truncating):
        self._path = path
        self._held = bytearray(b"" if truncating else data)
        self._position = len(self._held) if (writing and not truncating) else 0
        self._writing = writing
        self._unwritten = writing and truncating

    def readable(self):
        return not self._writing

    def writable(self):
        return self._writing

    def seekable(self):
        return True

    def readinto(self, buffer):
        held = self._held[self._position : self._position + len(buffer)]
        buffer[: len(held)] = held
        self._position += len(held)
        return len(held)

    def write(self, data):
        held = bytes(data)
        end = self._position + len(held)
        if end > len(self._held):
            self._held.extend(b"\0" * (end - len(self._held)))
        self._held[self._position : end] = held
        self._position = end
        self._unwritten = True
        return len(held)

    def tell(self):
        return self._position

    def seek(self, offset, whence=0):
        if whence == 1:
            self._position = max(0, self._position + int(offset))
        elif whence == 2:
            self._position = max(0, len(self._held) + int(offset))
        else:
            self._position = max(0, int(offset))
        return self._position

    def truncate(self, size=None):
        held = len(self._held) if size is None else max(0, int(size))
        del self._held[held:]
        self._unwritten = self._writing
        return held

    def flush(self):
        if not self._unwritten:
            return None
        if not _host.fs_write(self._path, bytes(self._held)):
            raise OSError(_errno.EACCES, "Permission denied", self._path)
        self._unwritten = False
        return None

    def close(self):
        if not self.closed:
            self.flush()
        io.RawIOBase.close(self)

    @property
    def name(self):
        return self._path

    def fileno(self):
        raise OSError("a file of a MicroVM's machine has no descriptor of the host")


def open(
    file,
    mode="r",
    buffering=-1,
    encoding=None,
    errors=None,
    newline=None,
    closefd=True,
    opener=None,
):
    """Open a file of the MicroVM's machine, as a Python program opens one."""
    import posix

    path = posix._as_text(file)
    flags = set(str(mode))
    if flags - set("rwxab+tU"):
        raise ValueError("invalid mode: " + str(mode))
    binary = "b" in flags
    if binary and "t" in flags:
        raise ValueError("can't have text and binary mode at once")
    writing = bool(flags & set("wxa+"))
    held = _host.fs_read(path)
    if held is None:
        if not writing or "r" in flags:
            raise FileNotFoundError(_errno.ENOENT, "No such file or directory", path)
        held = b""
    elif "x" in flags:
        raise FileExistsError(_errno.EEXIST, "File exists", path)
    raw = _MachineFile(path, held, writing, "w" in flags or "x" in flags)
    if buffering == 0:
        if not binary:
            raise ValueError("can't have unbuffered text I/O")
        return raw
    stream = io.BufferedWriter(raw) if writing else io.BufferedReader(raw)
    if binary:
        return stream
    return io.TextIOWrapper(
        stream, encoding=encoding or "utf-8", errors=errors, newline=newline
    )
"#;

/// `socket`: where an outbound connection of a MicroVM's guest goes.
pub(super) const SOCKET_SOURCE: &str = r#"
"""The socket interface of a MicroVM's guest.

A MicroVM has no host network: nothing here opens, binds or listens on a socket of the
machine this emulator runs on. What a guest CAN reach is the network its MicroVM is attached
to — the host decides every outbound connection from the account's own network connectors,
subnets and security groups (`microvm_network.rs`) — and a connection nothing carries never
completes, which is what the caller's own timeout then ends.

The NAMES are all here (the address families, the socket types, the option levels, the
errors), so a module that imports this one to name one of them imports cleanly: the standard
library's own HTTP, email and URL modules do exactly that.
"""

import _microvm_host as _host
import errno as _errno

# The address families, socket types and protocol numbers, as `<sys/socket.h>` and
# `<netinet/in.h>` define them (the values Python's own `socket` publishes). DATA: naming one
# opens nothing.
AF_UNSPEC = 0
AF_UNIX = 1
AF_INET = 2
AF_INET6 = 10
SOCK_STREAM = 1
SOCK_DGRAM = 2
SOCK_RAW = 3
SOCK_SEQPACKET = 5
SOL_SOCKET = 1
IPPROTO_IP = 0
IPPROTO_ICMP = 1
IPPROTO_TCP = 6
IPPROTO_UDP = 17
IPPROTO_IPV6 = 41

# The socket-level options a client or a server sets, as `<asm-generic/socket.h>` numbers
# them.
SO_DEBUG = 1
SO_REUSEADDR = 2
SO_TYPE = 3
SO_ERROR = 4
SO_DONTROUTE = 5
SO_BROADCAST = 6
SO_SNDBUF = 7
SO_RCVBUF = 8
SO_KEEPALIVE = 9
SO_OOBINLINE = 10
SO_LINGER = 13
SO_REUSEPORT = 15
SO_RCVTIMEO = 20
SO_SNDTIMEO = 21

# The TCP-level options beside them, as `<netinet/tcp.h>` numbers them.
TCP_NODELAY = 1
TCP_MAXSEG = 2
TCP_KEEPIDLE = 4
TCP_KEEPINTVL = 5
TCP_KEEPCNT = 6

# The shutdown directions, the send and receive flags, and the lookup flags and errors a
# program branches on, as `<sys/socket.h>` and `<netdb.h>` state them.
SHUT_RD = 0
SHUT_WR = 1
SHUT_RDWR = 2
MSG_OOB = 1
MSG_PEEK = 2
MSG_DONTROUTE = 4
MSG_TRUNC = 32
MSG_DONTWAIT = 64
MSG_WAITALL = 256
AI_PASSIVE = 1
AI_CANONNAME = 2
AI_NUMERICHOST = 4
AI_NUMERICSERV = 1024
EAI_NONAME = -2
EAI_FAMILY = -6
NI_NUMERICHOST = 1
NI_NUMERICSERV = 2
INADDR_ANY = 0
INADDR_LOOPBACK = 0x7F000001
INADDR_BROADCAST = 0xFFFFFFFF
SOMAXCONN = 4096

has_ipv6 = False

# What `socket.error` and `socket.timeout` are in a current Python: the builtin errors, which
# is what a caller catches.
error = OSError
timeout = TimeoutError

# The sentinel the standard library passes to mean "the default timeout".
_GLOBAL_DEFAULT_TIMEOUT = object()

_NO_HOST_NETWORK = "a MicroVM guest in this emulator has no host network to reach"

# The loopback of the MicroVM itself, which is where its own endpoint reaches its guest.
_LOOPBACK_NAMES = ("localhost", "localhost.localdomain", "ip6-localhost")
_LOOPBACK_ADDRESS = "127.0.0.1"

_default_timeout = None

# The MicroVM's own host name, installed by the host from the machine it booted.
_hostname = "localhost"


class herror(OSError):
    """A name service failure."""


class gaierror(OSError):
    """An address resolution failure."""


def getdefaulttimeout():
    return _default_timeout


def setdefaulttimeout(value):
    global _default_timeout
    _default_timeout = value


class socket:
    """One stream socket of a MicroVM's guest.

    The only thing it can do is CONNECT: the destination is a resource of the emulated
    network rather than a process of this machine, so what a connection establishes is
    reachability — which is what the host decides, and what `getsockname` then reports the
    local end of.
    """

    def __init__(self, family=AF_INET, type=SOCK_STREAM, proto=0, fileno=None):
        self.family = family
        self.type = type
        self.proto = proto
        self._peer = None
        self._local = None
        self._timeout = _default_timeout
        self._closed = False

    def __enter__(self):
        return self

    def __exit__(self, *_exception):
        self.close()

    def __repr__(self):
        return "<socket family=%s type=%s peer=%r>" % (self.family, self.type, self._peer)

    def connect(self, address):
        """Open a connection to `address`.

        The host decides whether the MicroVM's own network carries it. One it does not carry
        never completes — nothing answers at the other end — so the wait ends as the caller's
        own timeout ends it, which is the `TimeoutError` a blocked path raises.
        """
        host, port = _endpoint(address)
        carried, local_address, local_port = _host.connect(host, port)
        if not carried:
            raise timeout(_errno.ETIMEDOUT, "timed out")
        self._peer = (host, port)
        self._local = (local_address or "", int(local_port or 0))

    def connect_ex(self, address):
        try:
            self.connect(address)
        except OSError as raised:
            return raised.errno or _errno.ETIMEDOUT
        return 0

    def getpeername(self):
        if self._peer is None:
            raise error(_errno.ENOTCONN, "Transport endpoint is not connected")
        return self._peer

    def getsockname(self):
        if self._local is None:
            return ("0.0.0.0", 0)
        return self._local

    def settimeout(self, value):
        self._timeout = value

    def gettimeout(self):
        return self._timeout

    def setblocking(self, flag):
        self._timeout = None if flag else 0.0

    def getblocking(self):
        return self._timeout != 0.0

    def setsockopt(self, *_arguments):
        return None

    def getsockopt(self, level, option, *size):
        return 0

    def send(self, data, *_flags):
        """Nothing is delivered either way: the destination is a resource of the emulated
        network rather than a program, so what the connection established is reachability."""
        self._connected()
        return len(data)

    def sendall(self, data, *_flags):
        self._connected()
        return None

    def recv(self, size=0, *_flags):
        self._connected()
        return b""

    def recv_into(self, buffer, *_arguments):
        self._connected()
        return 0

    def makefile(self, mode="r", *_arguments, **_keywords):
        raise error(_NO_HOST_NETWORK)

    def shutdown(self, how):
        self._connected()

    def detach(self):
        self._closed = True
        return -1

    def fileno(self):
        return -1

    def close(self):
        self._closed = True

    def bind(self, _address):
        raise error(_NO_HOST_NETWORK)

    def listen(self, *_backlog):
        raise error(_NO_HOST_NETWORK)

    def accept(self):
        raise error(_NO_HOST_NETWORK)

    def _connected(self):
        if self._peer is None:
            raise error(_errno.ENOTCONN, "Transport endpoint is not connected")


SocketType = socket


def create_connection(
    address,
    timeout=_GLOBAL_DEFAULT_TIMEOUT,
    source_address=None,
    all_errors=False,
):
    """A stream socket connected to `address`, as the standard library's own builds one.

    `source_address` binds nothing: a guest has no host interface to bind, and the local end
    of a connection is the one the MicroVM's own network gave it.
    """
    opened = socket(AF_INET, SOCK_STREAM)
    try:
        if timeout is not _GLOBAL_DEFAULT_TIMEOUT:
            opened.settimeout(timeout)
        opened.connect(address)
    except BaseException:
        opened.close()
        raise
    return opened


def create_server(*_arguments, **_keywords):
    raise error(_NO_HOST_NETWORK)


def socketpair(*_arguments, **_keywords):
    raise error(_NO_HOST_NETWORK)


def _endpoint(address):
    """The host and port `address` names, as a connection is asked for them."""
    if not isinstance(address, (tuple, list)) or len(address) < 2:
        raise error("a connection is addressed by (host, port)")
    host = address[0]
    if isinstance(host, (bytes, bytearray)):
        host = bytes(host).decode("utf-8", "replace")
    host = "" if host is None else str(host).strip("[]")
    if host.lower() in _LOOPBACK_NAMES:
        host = _LOOPBACK_ADDRESS
    return host, _port(address[1])


def _port(port):
    if isinstance(port, str):
        if not port.isdigit():
            raise error("the port of a connection is a number")
        return int(port)
    return int(port or 0)


def _is_address(value):
    parts = value.split(".")
    if len(parts) != 4:
        return False
    return all(part.isdigit() and 0 <= int(part) <= 255 for part in parts)


def getaddrinfo(host, port, family=AF_UNSPEC, type=0, proto=0, flags=0):
    """The addresses `host`:`port` names.

    An ADDRESS is a pure conversion: the address itself is the answer. A NAME resolves to
    nothing — a MicroVM in this emulator reaches the resources of its own network by address,
    and no resolver of the host is asked — which is what a lookup of an unknown name reports.
    """
    if isinstance(host, (bytes, bytearray)):
        host = bytes(host).decode("utf-8", "replace")
    named = ("" if host is None else str(host)).strip("[]")
    kind = type or SOCK_STREAM
    protocol = proto or (IPPROTO_UDP if kind == SOCK_DGRAM else IPPROTO_TCP)
    if named.lower() in _LOOPBACK_NAMES:
        named = _LOOPBACK_ADDRESS
    if _is_address(named):
        if family not in (AF_UNSPEC, AF_INET):
            raise gaierror(EAI_FAMILY, "Address family for hostname not supported")
        return [(AF_INET, kind, protocol, "", (named, _port(port)))]
    raise gaierror(EAI_NONAME, "Name or service not known")


def getnameinfo(address, flags):
    host, port = _endpoint(address)
    return (host, str(port))


def gethostbyname(name):
    named = str(name)
    if named.lower() in _LOOPBACK_NAMES:
        return _LOOPBACK_ADDRESS
    if _is_address(named):
        return named
    raise gaierror(EAI_NONAME, "Name or service not known")


def gethostbyname_ex(name):
    return (str(name), [], [gethostbyname(name)])


def gethostbyaddr(address):
    return (str(address), [], [str(address)])


def gethostname():
    """The MicroVM's own host name, which the host installed on this module."""
    return _hostname


def getfqdn(name=""):
    """The qualified name of `name`: a MicroVM names no domain, so an address is itself and
    an unnamed host is the MicroVM's own."""
    named = "" if name is None else str(name)
    if not named or named in ("0.0.0.0", "::"):
        return _hostname
    return named


def inet_aton(address):
    """The four bytes a dotted quad spells. A pure conversion: it resolves nothing."""
    if not _is_address(str(address)):
        raise error("illegal IP address string passed to inet_aton")
    return bytes(int(part) for part in str(address).split("."))


def inet_ntoa(packed):
    if len(packed) != 4:
        raise error("packed IP wrong length for inet_ntoa")
    return ".".join(str(byte) for byte in packed)


def htons(value):
    return int(value) & 0xFFFF


def ntohs(value):
    return int(value) & 0xFFFF


def htonl(value):
    return int(value) & 0xFFFFFFFF


def ntohl(value):
    return int(value) & 0xFFFFFFFF
"#;

/// `socketserver`: what a program that binds a port inside a MicroVM registers.
pub(super) const SOCKETSERVER_SOURCE: &str = r#"
"""The server interface of a MicroVM's guest.

A guest's requests arrive through the MicroVM's PUBLISHED ENDPOINT rather than on a socket of
this machine, so a server a program binds here is REGISTERED: the port it bound is a port the
MicroVM is listening on (`bound_ports`), and the request the endpoint carries is handed to
the server's handler class exactly as the standard library's own server hands it one — the
handler is constructed with the connection, the client's address and the server, which is
what runs its `setup`, `handle` and `finish`.

Nothing here parses a request or renders a response: the standard library's own `http.server`
does both, on top of the connection below, so what a client reads back is what the guest's
own program produced.
"""

import io
import socket

# Every server a guest has bound, in the order it bound them.
_bound = []


def bound_ports():
    """The ports the guest's servers bound, in the order they bound them, which is what the
    MicroVM is listening on."""
    return [server.server_address[1] for server in _bound]


def server_on(port):
    """The server a request addressing `port` is handed to: the one that bound that port,
    and where none did, a server that bound no port of its own."""
    for server in _bound:
        if server.server_address[1] == port:
            return server
    for server in _bound:
        if not server.server_address[1]:
            return server
    return None


def _register(server):
    if server not in _bound:
        _bound.append(server)


def _forget(server):
    if server in _bound:
        _bound.remove(server)


class _ConnectionWriter:
    """Where a handler writes its response: the bytes go to the connection the endpoint
    carried the request in on, and the MicroVM's endpoint reads them back from there."""

    def __init__(self, connection):
        self._connection = connection
        self.closed = False

    def write(self, data):
        held = bytes(data)
        self._connection.sent(held)
        return len(held)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        return None

    def close(self):
        self.closed = True

    def writable(self):
        return True

    def readable(self):
        return False

    def seekable(self):
        return False

    def fileno(self):
        raise OSError("a MicroVM guest's connection has no descriptor of the host")


class Connection:
    """ONE connection to a server inside the MicroVM: the request the endpoint carried in,
    and the response the handler wrote back."""

    def __init__(self, received, client_address, server_address):
        self._received = io.BytesIO(received)
        self._written = bytearray()
        self._client = tuple(client_address)
        self._server = tuple(server_address)
        self._timeout = None

    def makefile(self, mode="rb", buffering=-1, *_arguments, **_keywords):
        if "w" in mode or "a" in mode:
            return _ConnectionWriter(self)
        if "b" in mode:
            return self._received
        return io.TextIOWrapper(self._received, encoding="iso-8859-1", newline="")

    def sent(self, data):
        self._written.extend(data)

    def written(self):
        """The whole response the guest wrote on this connection."""
        return bytes(self._written)

    def sendall(self, data, *_flags):
        self.sent(bytes(data))
        return None

    def send(self, data, *_flags):
        held = bytes(data)
        self.sent(held)
        return len(held)

    def recv(self, size=0, *_flags):
        return self._received.read(size if size and size > 0 else -1)

    def settimeout(self, value):
        self._timeout = value

    def gettimeout(self):
        return self._timeout

    def setsockopt(self, *_arguments):
        return None

    def getsockopt(self, *_arguments):
        return 0

    def shutdown(self, _how):
        return None

    def close(self):
        return None

    def fileno(self):
        raise OSError("a MicroVM guest's connection has no descriptor of the host")

    def getpeername(self):
        return self._client

    def getsockname(self):
        return self._server


class BaseServer:
    """A server of a MicroVM's guest: the address it bound and the class that handles one
    request."""

    timeout = None
    allow_reuse_address = False
    allow_reuse_port = False
    request_queue_size = 5

    def __init__(self, server_address, RequestHandlerClass):
        self.server_address = tuple(server_address)
        self.RequestHandlerClass = RequestHandlerClass

    def serve_forever(self, poll_interval=0.5):
        """Hand this server over to the MicroVM's endpoint.

        A guest's requests arrive through that endpoint, which calls this server's handler
        class once per request, so this returns rather than waiting on a socket there is none
        of: the server stays registered and keeps answering for as long as the MicroVM runs.
        """
        _register(self)
        return None

    def handle_request(self):
        _register(self)
        return None

    def service_actions(self):
        return None

    def shutdown(self):
        _forget(self)

    def server_close(self):
        _forget(self)

    def fileno(self):
        raise OSError("a MicroVM guest's server has no descriptor of the host")

    def get_request(self):
        raise OSError("a MicroVM guest's server accepts no connection of the host")

    def verify_request(self, request, client_address):
        return True

    def process_request(self, request, client_address):
        self.finish_request(request, client_address)
        self.shutdown_request(request)

    def finish_request(self, request, client_address):
        self.RequestHandlerClass(request, client_address, self)

    def shutdown_request(self, request):
        self.close_request(request)

    def close_request(self, request):
        return None

    def handle_error(self, request, client_address):
        import traceback

        traceback.print_exc()

    def handle_timeout(self):
        return None

    def __enter__(self):
        return self

    def __exit__(self, *_exception):
        self.server_close()


class TCPServer(BaseServer):
    """A server bound to a port of the MicroVM."""

    address_family = socket.AF_INET
    socket_type = socket.SOCK_STREAM

    def __init__(self, server_address, RequestHandlerClass, bind_and_activate=True):
        BaseServer.__init__(self, server_address, RequestHandlerClass)
        self.socket = socket.socket(self.address_family, self.socket_type)
        if bind_and_activate:
            self.server_bind()
            self.server_activate()

    def server_bind(self):
        """Bind the address the program named: a port of the MicroVM, which this emulator
        publishes rather than opening on the host."""
        address = self.server_address
        host = address[0] if address else ""
        port = address[1] if len(address) > 1 else 0
        self.server_address = ("" if host is None else str(host), int(port or 0))

    def server_activate(self):
        _register(self)


class UDPServer(TCPServer):
    """A datagram server, which a MicroVM's guest has no datagram path for."""

    socket_type = socket.SOCK_DGRAM

    def server_activate(self):
        raise OSError("a MicroVM guest in this emulator carries no datagram traffic")


class ThreadingMixIn:
    """The mix-in a threading server declares. One request is served at a time inside a
    MicroVM — a guest starts no thread of this process — so a request is handled on the
    thread the MicroVM's own guest runs on."""

    daemon_threads = False
    block_on_close = True

    def process_request_thread(self, request, client_address):
        try:
            self.finish_request(request, client_address)
        except Exception:
            self.handle_error(request, client_address)
        finally:
            self.shutdown_request(request)

    def process_request(self, request, client_address):
        self.process_request_thread(request, client_address)


class ForkingMixIn(ThreadingMixIn):
    """The mix-in a forking server declares; a MicroVM's guest starts no process."""


class ThreadingTCPServer(ThreadingMixIn, TCPServer):
    pass


class ThreadingUDPServer(ThreadingMixIn, UDPServer):
    pass


class ForkingTCPServer(ForkingMixIn, TCPServer):
    pass


class BaseRequestHandler:
    """One request, handled: constructed with the connection, the client's address and the
    server, exactly as the standard library's own handler is."""

    def __init__(self, request, client_address, server):
        self.request = request
        self.client_address = client_address
        self.server = server
        self.setup()
        try:
            self.handle()
        finally:
            self.finish()

    def setup(self):
        return None

    def handle(self):
        return None

    def finish(self):
        return None


class StreamRequestHandler(BaseRequestHandler):
    """A handler that reads the request and writes the response as STREAMS, which is what
    `http.server` is built on."""

    timeout = None
    disable_nagle_algorithm = False
    rbufsize = -1
    wbufsize = 0

    def setup(self):
        self.connection = self.request
        if self.timeout is not None:
            self.connection.settimeout(self.timeout)
        self.rfile = self.connection.makefile("rb", self.rbufsize)
        self.wfile = self.connection.makefile("wb", self.wbufsize)

    def finish(self):
        try:
            self.wfile.flush()
        except OSError:
            pass
        self.wfile.close()
        self.rfile.close()


class DatagramRequestHandler(BaseRequestHandler):
    """A datagram handler, which a MicroVM's guest has no datagram path for."""
"#;

/// `select`: the waiting interface the standard library names.
pub(super) const SELECT_SOURCE: &str = r#"
"""The waiting interface of a MicroVM's guest.

There is no descriptor of the host inside a MicroVM — this emulator opens no socket, pipe or
file of the machine it runs on for a guest — so there is nothing here to wait on and every
call raises. The module EXISTS because the standard library's own server and HTTP modules
import it to name it.
"""

error = OSError
PIPE_BUF = 4096

_NO_DESCRIPTOR = "a MicroVM guest in this emulator has no descriptor of the host to wait on"


def select(rlist, wlist, xlist, timeout=None):
    raise error(_NO_DESCRIPTOR)


def poll():
    raise error(_NO_DESCRIPTOR)


def epoll(*_arguments, **_keywords):
    raise error(_NO_DESCRIPTOR)
"#;

/// The standard streams a guest writes through: its MicroVM's log.
pub(super) const STREAMS_SOURCE: &str = r#"
"""Where a guest's own output goes: the MicroVM's log.

`print`, a traceback the interpreter renders and the line `http.server` logs for each request
are all written to the interpreter's standard streams, and this process's own streams are not
in the build at all. Each is this object instead, which hands whole LINES to the host — so
what a guest wrote is readable beside the request it was answering, and nothing of it reaches
a stream, a descriptor or a file of the host.
"""

import _microvm_host as _host


class _LogStream:
    """One of the guest's standard streams."""

    encoding = "utf-8"
    errors = "replace"

    def __init__(self, name):
        self.name = name
        self.mode = "w"
        self._held = ""

    def write(self, text):
        held = text if isinstance(text, str) else str(text)
        self._held += held
        while "\n" in self._held:
            line, _, rest = self._held.partition("\n")
            self._held = rest
            _host.log(self.name + ": " + line)
        return len(held)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def flush(self):
        if self._held:
            _host.log(self.name + ": " + self._held)
            self._held = ""

    def close(self):
        self.flush()

    def isatty(self):
        return False

    def readable(self):
        return False

    def writable(self):
        return True

    def seekable(self):
        return False

    def fileno(self):
        raise OSError("a MicroVM guest's log has no descriptor of the host")
"#;

/// The driver the host calls into the interpreter: the guest's program started, and one
/// proxied request handed to it.
pub(super) const RUNTIME_SOURCE: &str = r#"
"""The runtime a MicroVM's Python guest runs inside: its own modules, started the way its
image's command starts them, and the one request the published endpoint hands it."""

import builtins
import email.parser
import importlib.util
import json
import linecache
import sys
import traceback

import _microvm_host as _host
import socketserver as _servers

# Where the image's modules are mounted, which is the root its machine holds them under: the
# file a module reports, and the file a frame of it names, is its own path there.
_ROOT = "/"

# Who the MicroVM's endpoint reaches its guest as: the proxy carries a request over the
# MicroVM's own loopback, so that is the client address a handler reads.
_CLIENT = ("127.0.0.1", 0)

_MODULES = {}


class _Loader:
    """Imports one module of the image, held in memory."""

    def __init__(self, path, sources):
        self._path = path
        self._sources = sources

    def create_module(self, spec):
        return None

    def exec_module(self, module):
        source = self._sources[self._path]
        filename = _ROOT + self._path
        module.__dict__["__file__"] = filename
        linecache.cache[filename] = (
            len(source),
            None,
            source.splitlines(True),
            filename,
        )
        exec(compile(source, filename, "exec"), module.__dict__)

    def get_source(self, name):
        return self._sources[self._path]

    def is_package(self, name):
        return self._path.endswith("/__init__.py")


class _Finder:
    """Resolves a module name against the image's own modules, which are the entries its code
    artifact and its build steps left (`guest_program.rs`).

    The directory the entry module lives in is searched first, which is where the command
    that starts the guest runs it from, and then the root the image holds.
    """

    def __init__(self, sources, directories, spec_from_loader):
        self._sources = sources
        self._directories = directories
        self._spec_from_loader = spec_from_loader

    def find_spec(self, name, path=None, target=None):
        key = name.replace(".", "/")
        for directory in self._directories:
            stem = directory + key if directory else key
            for candidate, package in (
                (stem + "/__init__.py", True),
                (stem + ".py", False),
            ):
                if candidate in self._sources:
                    loader = _Loader(candidate, self._sources)
                    return self._spec_from_loader(name, loader, is_package=package)
        return None


def _directories_of(entry):
    """The directories a module name is resolved from: the entry module's own, then the root
    of the image."""
    directory = entry.rpartition("/")[0]
    held = []
    if directory:
        held.append(directory + "/")
    held.append("")
    return held


def _raised():
    """What a raise of the guest's own code is reported as: the error's class and message, as
    the guest worded them."""
    rendered = traceback.format_exception_only(*sys.exc_info()[:2])
    return "".join(rendered).strip() or "an error that could not be rendered"


def __microvm_start(modules, entry):
    """Start the guest's program.

    The image's modules become importable from memory and the entry module runs as
    `__main__`, which is how the image's command starts it — so a program that guards its
    server behind `if __name__ == "__main__"` binds it here exactly as it does on a real
    machine.

    Answers the empty string once the program has run, and what it raised when it raised.
    """
    global _MODULES
    _MODULES = json.loads(modules)
    sys.meta_path.insert(
        0, _Finder(_MODULES, _directories_of(entry), importlib.util.spec_from_loader)
    )
    source = _MODULES.get(entry)
    if source is None:
        return "the image holds no module at " + entry
    filename = _ROOT + entry
    linecache.cache[filename] = (
        len(source),
        None,
        source.splitlines(True),
        filename,
    )
    main = type(sys)("__main__")
    main.__dict__["__file__"] = filename
    main.__dict__["__builtins__"] = builtins
    sys.modules["__main__"] = main
    sys.argv = [filename]
    try:
        exec(compile(source, filename, "exec"), main.__dict__)
    except SystemExit:
        return ""
    except BaseException:
        return _raised()
    return ""


def __microvm_ports():
    """The ports the guest's own servers bound."""
    return [int(port or 0) for port in _servers.bound_ports()]


def __microvm_dispatch(port, method, url, headers, body):
    """Hand ONE request, as the endpoint carried it, to the server the guest bound on `port`.

    The handler class is constructed with the connection, the client's address and the
    server, which is the standard library's own contract for serving one request: what parses
    the request line and the headers, and what renders the status, the headers and the body,
    is the guest's own program and the `http.server` it is built on. The response is read
    back off the connection it wrote it to and handed to the host.

    Answers the empty string when the request was served, and what the guest raised when it
    raised before answering.
    """
    server = _servers.server_on(port)
    if server is None:
        return ""
    held = json.loads(headers)
    connection = _servers.Connection(
        _request_bytes(method, url, held, body), _CLIENT, server.server_address
    )
    try:
        server.finish_request(connection, _CLIENT)
    except SystemExit:
        pass
    except BaseException:
        raised = _raised()
        _respond(connection)
        return raised
    _respond(connection)
    return ""


def _respond(connection):
    """Hand the host what the guest wrote on `connection`, if it wrote anything at all."""
    written = connection.written()
    if not written:
        return
    status, headers, body = _parsed(written)
    _host.respond(status, json.dumps([[name, value] for name, value in headers]), body)


def _request_bytes(method, url, headers, body):
    """The request as it arrives inside the MicroVM: the line and headers the client sent,
    framed as the HTTP the guest's own server reads."""
    lines = ["%s %s HTTP/1.1" % (method, url)]
    named = set()
    for name, value in headers:
        named.add(str(name).lower())
        lines.append("%s: %s" % (name, value))
    if "host" not in named:
        lines.append("Host: %s" % _CLIENT[0])
    if body and not named & {"content-length", "transfer-encoding"}:
        lines.append("Content-Length: %d" % len(body))
    head = ("\r\n".join(lines) + "\r\n\r\n").encode("iso-8859-1", "replace")
    return head + bytes(body)


def _parsed(written):
    """The status, the headers and the body of the response the guest wrote.

    The headers are read by the standard library's own parser rather than by anything of this
    emulator's, so a header the guest wrote is the header a client reads.
    """
    head, separator, body = written.partition(b"\r\n\r\n")
    if not separator:
        head, body = written, b""
    line, _, rest = head.partition(b"\r\n")
    fields = line.split(None, 2)
    status = 200
    if len(fields) > 1 and fields[1].isdigit():
        status = int(fields[1])
    message = email.parser.BytesHeaderParser().parsebytes(rest + b"\r\n\r\n")
    return status, [(name, value) for name, value in message.items()], body
"#;
