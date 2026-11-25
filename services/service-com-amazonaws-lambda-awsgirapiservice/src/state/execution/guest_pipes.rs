// SPDX-License-Identifier: CC0-1.0

//! The CONNECTED PAIRS a run holds inside this process, and the descriptors they are watched
//! by: what `socket.socketpair()` answers and what the waiting interface reports readiness on.
//!
//! A `python3.x` runtime is a promise about an interpreter AND about the standard library that
//! comes with it, and part of that library is built on a pair of connected descriptors that
//! never leaves the process: the event loop of `asyncio` opens one as its own wakeup pipe
//! before it can run anything at all, and a program that wants a channel between two parts of
//! itself opens one too. Neither reaches the host — a pair is two byte queues and the two ends
//! that read and write them — so this runtime serves them, in Python, with nothing of the host
//! behind them: no host descriptor is opened, no interface is bound and nothing leaves this
//! process.
//!
//! What is this module's own:
//!
//! * the PAIRS themselves, each end a socket-shaped object with the calls a program makes on
//!   one (`send`, `sendall`, `recv`, `recv_into`, `close`, `shutdown`, the blocking mode and
//!   the timeout), and the bytes one end writes read back off the other in order;
//! * the DESCRIPTOR of each end, allocated from this environment's own table — the numbers past
//!   the three streams an execution environment already has — so the waiting interface
//!   ([`super::guest_select`]) reports readiness on a descriptor this runtime really holds and
//!   answers every other descriptor the way it answers one that is not open;
//! * the BOUNDS of a pair: how many an invocation may hold open and how many bytes may stand
//!   unread in one direction, so a program cannot turn a pair into an unbounded write, and a
//!   read that cannot be satisfied answers rather than holding the invocation's one thread.
//!
//! Nothing here invents readiness: an end is readable when the bytes its peer wrote are
//! standing in it or its peer has closed (which is the end-of-file a read sees), and writable
//! while it is open and its peer's queue is inside the bound. Nothing here waits, either: an
//! invocation runs on ONE thread, so a read with nothing standing is a read nothing can
//! satisfy, and it says so at once instead of holding that thread.

use super::package::PackageModule;

/// The modules this half of the runtime supplies: the in-process pairs, under a name of the
/// runtime's own (the leading underscore is how the standard library names a module that is an
/// implementation's own rather than part of its published surface).
///
/// Resolved before the frozen standard library and after the deployment package's own entries,
/// with the socket interface that answers a pair out of it and the waiting interface that
/// watches one.
#[must_use]
pub fn runtime_stdlib_modules() -> Vec<PackageModule> {
    vec![PackageModule {
        path: "_lambda_pipes.py".to_string(),
        source: PIPES_SOURCE.to_string(),
    }]
}

/// `_lambda_pipes`: the connected pairs of this runtime and the descriptors they are watched
/// by.
const PIPES_SOURCE: &str = r#"
"""The connected pairs of this runtime: two byte queues, both ends in this process."""

import errno

# The first descriptor this table hands out. An execution environment already has the three
# standard streams, so a pair of this runtime is numbered past them, exactly as the first
# descriptor a process opens is.
_FIRST_DESCRIPTOR = 3

# How many ends an invocation may hold open at a time. CHOSEN, like every other bound this
# runtime holds a guest to: each end is a queue in this process's memory, and a program decides
# how many pairs it opens. It is far above the one pair an event loop opens for itself.
MAX_OPEN_ENDS = 256

# How many bytes may stand unread in ONE direction of a pair, which is what a host's own socket
# buffer bounds. A write that would pass it is refused the way a write to a full buffer is.
MAX_QUEUED_BYTES = 1024 * 1024

# How long a blocking read with nothing to read waits: not at all. An invocation runs on ONE
# thread — this runtime starts none — so the bytes such a read waits for could only be written
# by the very code that is waiting: the wait can never be ended, and the runtime says so at
# once rather than holding the invocation's one thread to say the same thing.
_NOTHING_TO_WAIT_FOR = "the read had nothing to wait for and nothing can write to this end"

# The ends this environment holds open, by descriptor.
_HELD = {}

# The next descriptor to hand out. A descriptor is never handed out twice, so a call on an end
# a program closed is answered for a descriptor that is not open rather than reaching another.
_NEXT = [_FIRST_DESCRIPTOR]


def held(fd):
    """The end `fd` names, or ``None`` for a descriptor this runtime does not hold."""
    return _HELD.get(fd)


def readable(fd):
    """Whether the end `fd` names has something for a read: bytes standing in it, or the
    end-of-file its peer closing leaves."""
    end = _HELD.get(fd)
    if end is None:
        return False
    return end.readable()


def writable(fd):
    """Whether the end `fd` names can take a write."""
    end = _HELD.get(fd)
    if end is None:
        return False
    return end.writable()


def socketpair(family=None, type=None, proto=0):
    """A pair of connected ends, both in this process.

    The bytes written to one end are the bytes read off the other, in order. Nothing of the
    host is involved: there is no descriptor of the operating system behind either end.
    """
    if len(_HELD) + 2 > MAX_OPEN_ENDS:
        raise OSError(
            errno.EMFILE,
            "one invocation may hold %d connected ends open at a time" % (MAX_OPEN_ENDS,),
        )
    first = _End(family, type, proto)
    second = _End(family, type, proto)
    first._peer = second
    second._peer = first
    return (first, second)


class _End(object):
    """One end of a connected pair: the bytes standing in it, and its peer."""

    def __init__(self, family=None, type=None, proto=0):
        # The family and type a pair is opened with, as the standard library's own
        # `socketpair` opens one: a stream pair of the local family.
        self.family = 1 if family is None else int(family)
        self.type = 1 if type is None else int(type)
        self.proto = int(proto or 0)
        self._standing = bytearray()
        self._peer = None
        self._closed = False
        self._shut_read = False
        self._shut_write = False
        self._blocking = True
        self._timeout = None
        self._fd = _NEXT[0]
        _NEXT[0] += 1
        _HELD[self._fd] = self

    # The calls a program makes on one end of a pair.

    def fileno(self):
        """The descriptor this end is watched by, and -1 for one a program closed."""
        if self._closed:
            return -1
        return self._fd

    def detach(self):
        """Give up this end's descriptor without closing it, as the standard library's own
        socket does: the descriptor is answered and this object holds none."""
        held_fd = self.fileno()
        self._closed = True
        return held_fd

    def setblocking(self, flag):
        self._blocking = bool(flag)
        self._timeout = None if flag else 0.0

    def getblocking(self):
        return self._blocking

    def settimeout(self, timeout):
        if timeout is None:
            self._blocking = True
            self._timeout = None
            return
        seconds = float(timeout)
        if seconds < 0:
            raise ValueError("Timeout value out of range")
        self._timeout = seconds
        self._blocking = seconds > 0

    def gettimeout(self):
        return self._timeout

    def setsockopt(self, level, option, value, *rest):
        """Set an option on this end, of which a pair in this process has none to set."""

    def getsockopt(self, level, option, buflen=None):
        """Read an option off this end: a pair carries no error and no option of its own."""
        if buflen is None:
            return 0
        return bytes(buflen)

    def getsockname(self):
        """The address this end is bound to, which an unnamed pair has none of."""
        return ""

    def getpeername(self):
        """The address of the peer, which an unnamed pair has none of."""
        return ""

    def readable(self):
        if self._closed or self._shut_read:
            return False
        if self._standing:
            return True
        # A peer that has closed or shut its writing side leaves the end-of-file a read sees,
        # which is a read that answers at once.
        peer = self._peer
        return peer is None or peer._closed or peer._shut_write

    def writable(self):
        if self._closed or self._shut_write:
            return False
        peer = self._peer
        if peer is None or peer._closed:
            # A write to an end whose peer is gone is answered where the write is made.
            return True
        return len(peer._standing) < MAX_QUEUED_BYTES

    def send(self, data, flags=0):
        """Write `data` to the peer's queue and answer how many bytes it took."""
        self._usable()
        if self._shut_write:
            raise BrokenPipeError(errno.EPIPE, "Broken pipe")
        peer = self._peer
        if peer is None or peer._closed or peer._shut_read:
            raise BrokenPipeError(errno.EPIPE, "Broken pipe")
        held_bytes = bytes(data)
        room = MAX_QUEUED_BYTES - len(peer._standing)
        if room <= 0:
            raise BlockingIOError(errno.EAGAIN, "Resource temporarily unavailable")
        taken = held_bytes[:room]
        peer._standing.extend(taken)
        return len(taken)

    def sendall(self, data, flags=0):
        """Write all of `data`, which one write of a pair takes whole or refuses."""
        held_bytes = bytes(data)
        at = 0
        while at < len(held_bytes):
            at += self.send(held_bytes[at:], flags)
        return None

    def recv(self, bufsize, flags=0):
        """Read at most `bufsize` bytes of what the peer wrote."""
        self._usable()
        if int(bufsize) < 0:
            raise ValueError("negative buffersize in recv")
        if self._shut_read:
            return b""
        if not self._standing:
            peer = self._peer
            if peer is None or peer._closed or peer._shut_write:
                # The end-of-file the peer's close left.
                return b""
            self._wait()
        taken = bytes(self._standing[: int(bufsize)])
        del self._standing[: int(bufsize)]
        return taken

    def recv_into(self, buffer, nbytes=0, flags=0):
        """Read into `buffer` and answer how many bytes were read."""
        wanted = int(nbytes) or len(buffer)
        taken = self.recv(wanted, flags)
        buffer[: len(taken)] = taken
        return len(taken)

    def shutdown(self, how):
        """Shut this end's reading side, its writing side, or both."""
        self._usable()
        how = int(how)
        if how in (0, 2):
            self._shut_read = True
        if how in (1, 2):
            self._shut_write = True

    def close(self):
        """Give up this end, which is the end-of-file its peer then reads."""
        if self._closed:
            return
        self._closed = True
        _HELD.pop(self._fd, None)

    def _usable(self):
        if self._closed:
            raise OSError(errno.EBADF, "Bad file descriptor")

    def _wait(self):
        """Answer for bytes that cannot come.

        An invocation runs on ONE thread, so the only code that could write to this end is the
        code that is waiting on it: a read with nothing standing is a wait nothing can end. A
        non-blocking end says so the way a non-blocking read does, and an end with a timeout or
        with none says so at once rather than sleeping out a wait whose answer cannot change.
        """
        if self._timeout == 0.0 and not self._blocking:
            raise BlockingIOError(errno.EAGAIN, "Resource temporarily unavailable")
        raise TimeoutError(errno.ETIMEDOUT, _NOTHING_TO_WAIT_FOR)

    def __enter__(self):
        return self

    def __exit__(self, *exception):
        self.close()
        return False

    def __repr__(self):
        return "<socket.socket fd=%d, family=%d, type=%d, proto=%d>" % (
            self.fileno(),
            self.family,
            self.type,
            self.proto,
        )
"#;
