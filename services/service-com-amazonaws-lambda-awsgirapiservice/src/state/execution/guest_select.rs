// SPDX-License-Identifier: CC0-1.0

//! The WAITING half of this runtime's standard library: the `select` module a Python
//! handler — or a package it vendors — imports beside the socket interface.
//!
//! A `python3.x` runtime is a promise about an interpreter AND about the standard library
//! that comes with it: a handler whose module body says `import select`, or that imports
//! one of the standard library's own modules built on it (`selectors`, and so every client
//! library that multiplexes its connections), imports cleanly on the runtime it was written
//! for, and so it has to here. A module of the standard library this emulator registers no
//! extension for is SUPPLIED on the import path instead of being absent from it, exactly as
//! the socket interface and the TLS layer are ([`super::guest_http::runtime_stdlib_modules`]
//! and [`super::guest_tls`], which this module's source is resolved beside). What this
//! runtime refuses is the CALL that needs a capability it does not have, never the import.
//!
//! What is this module's own, and what it refuses:
//!
//! * the NAMES a program reads off `select` — the `poll` and `epoll` event bits, the pipe
//!   write limit and the module's own error — are all here, so a module that imports
//!   `select` to name one of them, or to ask which multiplexers the platform has, imports
//!   and runs;
//! * a WAIT with nothing to wait for is time, so it is served: a `select` over three empty
//!   sets, or a `poll` with no registration, waits out the timeout it was given against the
//!   invocation's own clock (`time.sleep`, which this runtime clamps to what is left of the
//!   run) and answers that nothing became ready;
//! * a wait on a DESCRIPTOR is not, and no call here pretends otherwise: a Lambda function
//!   in this emulator holds no host descriptor at all — its sockets reach only the
//!   in-process X-Ray daemon ([`super::guest_http`]) and its files are refused
//!   ([`super::python`]) — so each call answers for a descriptor that is not open exactly
//!   as the system call it stands for does: `select` raises `OSError` with `EBADF`, `poll`
//!   reports the registration as `POLLNVAL`, and `epoll` refuses the registration with
//!   `EBADF`. A negative descriptor is the language's own `ValueError`, as it is in
//!   CPython.
//!
//! Nothing here invents readiness: a descriptor is never reported ready, because there is
//! none for anything to be ready on.

use super::package::PackageModule;

/// The modules of the standard library this half of the runtime supplies: the waiting
/// interface.
///
/// Resolved with the socket interface and the TLS layer it is used beside, before the frozen
/// standard library and after the deployment package's own entries — so this IS the `select`
/// a handler imports, the one the frozen `selectors` builds its default selector out of, and
/// a package shipping its own still wins.
#[must_use]
pub fn runtime_stdlib_modules() -> Vec<PackageModule> {
    vec![PackageModule {
        path: "select.py".to_string(),
        source: SELECT_SOURCE.to_string(),
    }]
}

/// `select`: the waiting interface of this runtime.
///
/// The surface is the standard library's, so a handler and the standard library's own
/// modules built on it (`selectors`, which reads `select.select` into its own selector and
/// asks which of `poll`, `epoll`, `devpoll` and `kqueue` the platform has) read the names
/// they expect. The event bits are the ones this platform's `<poll.h>` and `<sys/epoll.h>`
/// define, and the two multiplexers this platform has are the two defined here, so the
/// default selector a program gets is the one it would get on the advertised runtime.
const SELECT_SOURCE: &str = r#"
"""The waiting interface of this runtime: a function holds no descriptor to wait on."""

import errno
import time

# What `select.error` is in a current Python: the builtin error, which is what a caller
# catches.
error = OSError

# The bytes a write to a pipe is atomic up to, as this platform's own `PIPE_BUF` states it.
# DATA, like the event bits below: there is no pipe here to write to.
PIPE_BUF = 4096

# The event bits a `poll` registration and answer are built out of, as `<poll.h>` defines
# them on this platform (the values Python's own `select` module documents for them).
POLLIN = 0x001
POLLPRI = 0x002
POLLOUT = 0x004
POLLERR = 0x008
POLLHUP = 0x010
POLLNVAL = 0x020
POLLRDNORM = 0x040
POLLRDBAND = 0x080
POLLWRNORM = 0x100
POLLWRBAND = 0x200
POLLMSG = 0x400
POLLRDHUP = 0x2000

# The same for an `epoll` registration, as `<sys/epoll.h>` defines them: the first bits are
# the `poll` ones, and the control bits are the high ones.
EPOLLIN = 0x001
EPOLLPRI = 0x002
EPOLLOUT = 0x004
EPOLLERR = 0x008
EPOLLHUP = 0x010
EPOLLRDNORM = 0x040
EPOLLRDBAND = 0x080
EPOLLWRNORM = 0x100
EPOLLWRBAND = 0x200
EPOLLMSG = 0x400
EPOLLRDHUP = 0x2000
EPOLLEXCLUSIVE = 1 << 28
EPOLLWAKEUP = 1 << 29
EPOLLONESHOT = 1 << 30
EPOLLET = 1 << 31
EPOLL_CLOEXEC = 0o2000000

# The registration a `poll` object makes when it is given no event mask, which is the mask
# CPython's own `poll.register` defaults to.
_POLL_DEFAULT = POLLIN | POLLPRI | POLLOUT

# What the event mask of an `epoll` registration defaults to, as `epoll.register` documents
# it.
_EPOLL_DEFAULT = EPOLLIN | EPOLLPRI | EPOLLOUT

# How long one wait of a call that nothing can ever end is given, in seconds. The runtime
# clamps a sleep to what is LEFT of the invocation, and no function's `Timeout` can reach
# this length, so one sleep of it spends the rest of the invocation — after which the
# invocation's own deadline is up, which is what ends such a wait on a real host.
_UNBOUNDED_WAIT = 3600.0

# Said of a wait nothing can end, once the invocation's own time has been spent waiting: no
# descriptor is being watched, so no event was ever going to arrive.
_NOTHING_TO_WAIT_FOR = "the wait had nothing to wait for and the invocation's time is up"

# Said of a descriptor this runtime has none of, which is every descriptor: the text the
# operating system gives `EBADF`.
_BAD_DESCRIPTOR = "Bad file descriptor"


def _descriptor(fileobj):
    """The descriptor `fileobj` names, as every call of this module reads one.

    An integer is itself and anything else is asked for its own `fileno`, which is how
    CPython reads a file object here, and a negative one is the language's own error.
    """
    if isinstance(fileobj, int):
        found = fileobj
    else:
        named = getattr(fileobj, "fileno", None)
        if named is None:
            raise TypeError("argument must be an int, or have a fileno() method.")
        found = named()
        if not isinstance(found, int):
            raise TypeError("fileno() returned a non-integer")
    if found < 0:
        raise ValueError("file descriptor cannot be a negative integer (%d)" % (found,))
    return found


def _seconds(timeout):
    """`timeout` as the seconds a wait lasts, checked the way CPython checks one."""
    if not isinstance(timeout, (int, float)):
        raise TypeError("timeout must be a float or None")
    return float(timeout)


def _milliseconds(timeout):
    """`timeout` as the milliseconds a `poll` waits, which is the unit that call takes."""
    if not isinstance(timeout, (int, float)):
        raise TypeError("timeout must be an integer or None")
    return int(timeout)


def _waited(seconds):
    """Wait `seconds`, or spend what is left of the invocation when nothing bounds the wait.

    `seconds` of `None` is a wait nothing can end: no descriptor is being watched, so no
    event is ever going to arrive. It is ONE sleep, which the runtime clamps to what is left
    of the invocation — so the call waits exactly as long as the invocation had, and then
    says what it is: there was nothing to wait for and the time is up. It never loops and
    never reports a readiness it did not see.
    """
    if seconds is None:
        time.sleep(_UNBOUNDED_WAIT)
        raise TimeoutError(errno.ETIMEDOUT, _NOTHING_TO_WAIT_FOR)
    if seconds > 0:
        time.sleep(seconds)


def select(rlist, wlist, xlist, timeout=None):
    """Wait until one of the named descriptors is ready, or until `timeout` passes.

    A function of this runtime holds no host descriptor, so a named descriptor is one that
    is not open — which is the `EBADF` a `select` call answers for it — and the three empty
    sets are a wait for the time alone.
    """
    watched = []
    for named in (rlist, wlist, xlist):
        try:
            entries = list(named)
        except TypeError:
            raise TypeError("arguments 1-3 must be sequences") from None
        watched.extend(_descriptor(entry) for entry in entries)
    waited = None
    if timeout is not None:
        waited = _seconds(timeout)
        if waited < 0:
            raise ValueError("timeout must be non-negative")
    if watched:
        raise error(errno.EBADF, _BAD_DESCRIPTOR)
    _waited(waited)
    return [], [], []


class poll(object):
    """The `poll` multiplexer of this runtime.

    The registration is this object's own, and a descriptor is reported the way `poll`
    reports one that is not open: `POLLNVAL`, immediately. Nothing is dropped from the
    registration for it, exactly as nothing is on a host.
    """

    def __init__(self):
        self._watched = {}

    def register(self, fd, eventmask=_POLL_DEFAULT):
        self._watched[_descriptor(fd)] = int(eventmask)

    def modify(self, fd, eventmask):
        found = _descriptor(fd)
        if found not in self._watched:
            raise error(errno.ENOENT, "No such file or directory")
        self._watched[found] = int(eventmask)

    def unregister(self, fd):
        found = _descriptor(fd)
        if found not in self._watched:
            raise KeyError(found)
        del self._watched[found]

    def poll(self, timeout=None):
        """What the registered descriptors answer: each of them is invalid, so it is
        answered at once; a registration of none at all is a wait for the time alone,
        where `timeout` is MILLISECONDS and a negative one means nothing bounds it."""
        if self._watched:
            return [(fd, POLLNVAL) for fd in sorted(self._watched)]
        if timeout is not None:
            milliseconds = _milliseconds(timeout)
            if milliseconds >= 0:
                _waited(milliseconds / 1000.0)
                return []
        # No registration, and nothing bounds the wait.
        _waited(None)


class epoll(object):
    """The `epoll` multiplexer of this runtime.

    `epoll` is told which descriptor to watch while it is registered rather than while it
    waits, so a descriptor that is not open is refused there — `EBADF`, which is what
    `epoll_ctl` answers — and a wait with nothing registered is a wait for the time alone.
    """

    def __init__(self, sizehint=-1, flags=0):
        self._closed = False

    @classmethod
    def fromfd(cls, fd):
        _descriptor(fd)
        raise error(errno.EBADF, _BAD_DESCRIPTOR)

    def __enter__(self):
        return self

    def __exit__(self, *exception):
        self.close()
        return False

    @property
    def closed(self):
        return self._closed

    def close(self):
        self._closed = True

    def fileno(self):
        """This object watches nothing, so it holds no descriptor of its own."""
        return -1

    def _open(self):
        if self._closed:
            raise ValueError("I/O operation on closed epoll object")

    def register(self, fd, eventmask=_EPOLL_DEFAULT):
        self._open()
        _descriptor(fd)
        raise error(errno.EBADF, _BAD_DESCRIPTOR)

    def modify(self, fd, eventmask):
        self._open()
        _descriptor(fd)
        raise error(errno.EBADF, _BAD_DESCRIPTOR)

    def unregister(self, fd):
        self._open()
        _descriptor(fd)
        raise error(errno.EBADF, _BAD_DESCRIPTOR)

    def poll(self, timeout=None, maxevents=-1):
        """Nothing is registered, because no registration is accepted, so this is the wait
        for the time alone — in SECONDS, where `None` or a negative value means nothing
        bounds it."""
        self._open()
        if timeout is not None:
            seconds = _seconds(timeout)
            if seconds >= 0:
                _waited(seconds)
                return []
        # No registration, and nothing bounds the wait.
        _waited(None)
"#;
