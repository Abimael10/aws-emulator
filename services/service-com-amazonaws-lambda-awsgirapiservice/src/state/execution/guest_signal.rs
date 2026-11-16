// SPDX-License-Identifier: CC0-1.0

//! The SIGNAL half of this runtime's standard library: the `signal` module a Python handler
//! — or a package it vendors — imports beside the waiting interface.
//!
//! A `python3.x` runtime is a promise about an interpreter AND about the standard library
//! that comes with it: a handler whose module body says `import signal`, or that imports one
//! of the standard library's own modules built on it, imports cleanly on the runtime it was
//! written for, and so it has to here. A module of the standard library this emulator
//! registers no extension for is SUPPLIED on the import path instead of being absent from it,
//! exactly as the socket interface, the TLS layer and the waiting interface are
//! ([`super::guest_http::runtime_stdlib_modules`], [`super::guest_tls`],
//! [`super::guest_select`], which this module's source is resolved beside). What this runtime
//! refuses is the CALL that needs a capability it does not have, never the import.
//!
//! What is this module's own, and what it refuses:
//!
//! * the NAMES a program reads off `signal` — the signal numbers, the two handler sentinels,
//!   the enumerations they are members of, the interval-timer selectors and the mask
//!   operations — are all here, so a module that imports `signal` to name one of them, or to
//!   catch its error, imports and runs;
//! * a REGISTRATION is this module's own: a handler installed for a signal is remembered and
//!   reported back, and installing one answers the handler that was in place before it,
//!   exactly as the call does on a host. Nothing ever DELIVERS one: an invocation of this
//!   emulator is not a process of its own — it has no terminal, no child process and no
//!   timer of the operating system — so no signal arrives, and the registration is a
//!   registration and nothing more;
//! * a call that would need the operating system to deliver or to time a signal is not served
//!   and none here pretends otherwise: arming a timer, raising a signal, signalling a thread,
//!   writing a wakeup descriptor and waiting for one each answer for a capability this
//!   runtime has none of, with `OSError` carrying `ENOSYS` — the number a system call that is
//!   not there answers with — and a `pause`, which is a wait nothing can end, waits out what
//!   the invocation has left and then says so, against the invocation's own clock
//!   (`time.sleep`, which this runtime clamps to what is left of the run).
//!
//! Nothing here invents a delivery: a handler this module holds is never called, because
//! nothing in this process signals a function's invocation.

use super::package::PackageModule;

/// The modules of the standard library this half of the runtime supplies: the signal
/// interface.
///
/// Resolved with the socket interface, the TLS layer and the waiting interface it is used
/// beside, before the frozen standard library and after the deployment package's own entries
/// — so this IS the `signal` a handler imports, and a package shipping its own still wins.
#[must_use]
pub fn runtime_stdlib_modules() -> Vec<PackageModule> {
    vec![PackageModule {
        path: "signal.py".to_string(),
        source: SIGNAL_SOURCE.to_string(),
    }]
}

/// `signal`: the signal interface of this runtime.
///
/// The surface is the standard library's, so a handler and the standard library's own modules
/// built on it read the names they expect. The numbers are the ones this platform's
/// `<signal.h>` defines (the values Python's own `signal` module publishes for them), and the
/// enumerations are the ones a current Python exposes, so a program that prints a signal, maps
/// a number to its name or asks which signals are valid gets the answers it would get on the
/// advertised runtime.
const SIGNAL_SOURCE: &str = r#"
"""The signal interface of this runtime: nothing delivers a signal to an invocation."""

import enum
import errno
import time

# The signal numbers this platform's `<signal.h>` defines, as Python's own `signal` module
# publishes them. DATA: there is nothing here to signal.
SIGHUP = 1
SIGINT = 2
SIGQUIT = 3
SIGILL = 4
SIGTRAP = 5
SIGABRT = 6
SIGIOT = 6
SIGBUS = 7
SIGFPE = 8
SIGKILL = 9
SIGUSR1 = 10
SIGSEGV = 11
SIGUSR2 = 12
SIGPIPE = 13
SIGALRM = 14
SIGTERM = 15
SIGSTKFLT = 16
SIGCHLD = 17
SIGCLD = 17
SIGCONT = 18
SIGSTOP = 19
SIGTSTP = 20
SIGTTIN = 21
SIGTTOU = 22
SIGURG = 23
SIGXCPU = 24
SIGXFSZ = 25
SIGVTALRM = 26
SIGPROF = 27
SIGWINCH = 28
SIGIO = 29
SIGPOLL = 29
SIGPWR = 30
SIGSYS = 31

# One past the highest signal number, as `<signal.h>` states it: what a program reads to size
# its own table of signals.
NSIG = 65

# The interval timers a process can arm, and the three mask operations, as the module
# publishes them.
ITIMER_REAL = 0
ITIMER_VIRTUAL = 1
ITIMER_PROF = 2
SIG_BLOCK = 0
SIG_UNBLOCK = 1
SIG_SETMASK = 2


class Signals(enum.IntEnum):
    """The signals of this platform, as the module's own enumeration of them."""

    SIGHUP = 1
    SIGINT = 2
    SIGQUIT = 3
    SIGILL = 4
    SIGTRAP = 5
    SIGABRT = 6
    SIGBUS = 7
    SIGFPE = 8
    SIGKILL = 9
    SIGUSR1 = 10
    SIGSEGV = 11
    SIGUSR2 = 12
    SIGPIPE = 13
    SIGALRM = 14
    SIGTERM = 15
    SIGSTKFLT = 16
    SIGCHLD = 17
    SIGCONT = 18
    SIGSTOP = 19
    SIGTSTP = 20
    SIGTTIN = 21
    SIGTTOU = 22
    SIGURG = 23
    SIGXCPU = 24
    SIGXFSZ = 25
    SIGVTALRM = 26
    SIGPROF = 27
    SIGWINCH = 28
    SIGIO = 29
    SIGPWR = 30
    SIGSYS = 31


class Handlers(enum.IntEnum):
    """The two handler sentinels a registration can name instead of a callable."""

    SIG_DFL = 0
    SIG_IGN = 1


class Sigmasks(enum.IntEnum):
    """The operations `pthread_sigmask` takes."""

    SIG_BLOCK = 0
    SIG_UNBLOCK = 1
    SIG_SETMASK = 2


SIG_DFL = Handlers.SIG_DFL
SIG_IGN = Handlers.SIG_IGN


class ItimerError(OSError):
    """What an interval-timer call raises, as the module declares it."""


# Said of a call that needs the operating system to deliver or to time a signal: an
# invocation of this emulator is no process of its own, so there is nothing to deliver one.
_NO_SIGNALS = "a Lambda function in this emulator is not signalled"

# How long a `pause` is given, in seconds. The runtime clamps a sleep to what is LEFT of the
# invocation, and no function's `Timeout` can reach this length, so one sleep of it spends the
# rest of the invocation — after which the invocation's own deadline is up, which is what ends
# such a wait on a real host.
_UNBOUNDED_WAIT = 3600.0

# Said of a `pause` once the invocation's own time has been spent waiting: nothing was going
# to arrive.
_NOTHING_TO_WAIT_FOR = "the wait had nothing to wait for and the invocation's time is up"

# The handlers this runtime holds, by signal number. A signal with no entry is handled the
# way the platform handles it by default, which is what `getsignal` reports for it.
_HANDLERS = {}

# What every signal of this platform is called, for `strsignal`: the text the operating
# system gives for it.
_DESCRIPTIONS = {
    SIGHUP: "Hangup",
    SIGINT: "Interrupt",
    SIGQUIT: "Quit",
    SIGILL: "Illegal instruction",
    SIGTRAP: "Trace/breakpoint trap",
    SIGABRT: "Aborted",
    SIGBUS: "Bus error",
    SIGFPE: "Floating point exception",
    SIGKILL: "Killed",
    SIGUSR1: "User defined signal 1",
    SIGSEGV: "Segmentation fault",
    SIGUSR2: "User defined signal 2",
    SIGPIPE: "Broken pipe",
    SIGALRM: "Alarm clock",
    SIGTERM: "Terminated",
    SIGSTKFLT: "Stack fault",
    SIGCHLD: "Child exited",
    SIGCONT: "Continued",
    SIGSTOP: "Stopped (signal)",
    SIGTSTP: "Stopped",
    SIGTTIN: "Stopped (tty input)",
    SIGTTOU: "Stopped (tty output)",
    SIGURG: "Urgent I/O condition",
    SIGXCPU: "CPU time limit exceeded",
    SIGXFSZ: "File size limit exceeded",
    SIGVTALRM: "Virtual timer expired",
    SIGPROF: "Profiling timer expired",
    SIGWINCH: "Window changed",
    SIGIO: "I/O possible",
    SIGPWR: "Power failure",
    SIGSYS: "Bad system call",
}


def _signal_number(signalnum):
    """`signalnum` as the signal it names, checked the way the module checks one.

    A value that is no signal of this platform is the language's own `ValueError`, as it is
    in CPython, and the number comes back as its enumeration member so a caller reads the
    name back.
    """
    try:
        return Signals(signalnum)
    except ValueError:
        raise ValueError("signal number out of range") from None


def signal(signalnum, handler):
    """Install `handler` for `signalnum` and answer the handler that was in place.

    The registration is remembered here. Nothing delivers the signal, because an invocation
    of this emulator is not a process of its own — so an installed handler is never called.
    """
    number = _signal_number(signalnum)
    if not callable(handler) and handler not in (SIG_DFL, SIG_IGN):
        raise TypeError("signal handler must be signal.SIG_IGN, signal.SIG_DFL, or a callable object")
    previous = _HANDLERS.get(int(number), SIG_DFL)
    _HANDLERS[int(number)] = handler
    return previous


def getsignal(signalnum):
    """The handler installed for `signalnum`, or the platform's default where none is."""
    number = _signal_number(signalnum)
    return _HANDLERS.get(int(number), SIG_DFL)


def strsignal(signalnum):
    """What the operating system calls `signalnum`, or `None` for a number that is none."""
    try:
        number = _signal_number(signalnum)
    except ValueError:
        return None
    return _DESCRIPTIONS.get(int(number))


def valid_signals():
    """Every signal of this platform, as the module answers the set."""
    return frozenset(Signals)


def default_int_handler(signalnum=SIGINT, frame=None):
    """The interrupt handler the platform installs by default, which raises."""
    raise KeyboardInterrupt()


# What the interpreter installs for the interrupt signal on its way up, which is what
# `getsignal` reports for that signal until a program installs a handler of its own.
_HANDLERS[int(SIGINT)] = default_int_handler


def alarm(seconds):
    """Arm the real-time timer, which needs the operating system to time it."""
    raise OSError(errno.ENOSYS, _NO_SIGNALS)


def setitimer(which, seconds, interval=0.0):
    """Arm an interval timer, which needs the operating system to time it."""
    raise ItimerError(errno.ENOSYS, _NO_SIGNALS)


def getitimer(which):
    """Read an interval timer, which this runtime arms none of."""
    raise ItimerError(errno.ENOSYS, _NO_SIGNALS)


def raise_signal(signalnum):
    """Deliver `signalnum` to this process, which is the capability there is none of."""
    _signal_number(signalnum)
    raise OSError(errno.ENOSYS, _NO_SIGNALS)


def pthread_kill(thread_id, signalnum):
    """Deliver `signalnum` to one thread, which is that same capability."""
    _signal_number(signalnum)
    raise OSError(errno.ENOSYS, _NO_SIGNALS)


def pthread_sigmask(how, mask):
    """Change the blocked-signal mask, which is the operating system's own."""
    raise OSError(errno.ENOSYS, _NO_SIGNALS)


def sigpending():
    """The signals waiting to be delivered, which there is no delivery for."""
    raise OSError(errno.ENOSYS, _NO_SIGNALS)


def sigwait(sigset):
    """Wait for one of `sigset`, which nothing is going to deliver."""
    raise OSError(errno.ENOSYS, _NO_SIGNALS)


def sigtimedwait(sigset, timeout):
    """The same wait, bounded: nothing delivers one, so there is nothing to wait for."""
    raise OSError(errno.ENOSYS, _NO_SIGNALS)


def sigwaitinfo(sigset):
    """The same wait again, with the delivery's own details."""
    raise OSError(errno.ENOSYS, _NO_SIGNALS)


def siginterrupt(signalnum, flag):
    """Choose whether a delivery restarts an interrupted system call."""
    _signal_number(signalnum)
    raise OSError(errno.ENOSYS, _NO_SIGNALS)


def set_wakeup_fd(fd, warn_on_full_buffer=True):
    """Write every delivery to a descriptor, which an invocation holds none of."""
    if not isinstance(fd, int):
        raise TypeError("an integer is required")
    if fd != -1:
        raise ValueError("invalid fd")
    return -1


def pause():
    """Wait for a signal.

    Nothing delivers one, so this is a wait nothing can end: ONE sleep, which the runtime
    clamps to what is left of the invocation, and then what it is — there was nothing to wait
    for and the time is up. It never loops and never reports a delivery it did not see.
    """
    time.sleep(_UNBOUNDED_WAIT)
    raise TimeoutError(errno.ETIMEDOUT, _NOTHING_TO_WAIT_FOR)
"#;
