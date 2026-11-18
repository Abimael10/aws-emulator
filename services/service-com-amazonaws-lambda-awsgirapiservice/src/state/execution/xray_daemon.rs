// SPDX-License-Identifier: CC0-1.0

//! The X-RAY DAEMON a traced invocation reports its own work to, and the DATAGRAM half of the
//! `socket` module a handler reaches it through.
//!
//! An instrumented Lambda handler does not call X-Ray: it writes its segment and subsegment
//! documents as UDP datagrams to the daemon the execution environment names in
//! `AWS_XRAY_DAEMON_ADDRESS`, and the daemon submits them. That is what this module is, in
//! two halves:
//!
//! * [`DATAGRAM_SOURCE`], the sockets of the `socket` module this runtime supplies
//!   ([`super::guest_http::runtime_stdlib_modules`], which this source is appended to). The
//!   interpreter a run happens in registers no host socket extension at all
//!   ([`super::python`]), so every socket of a run is this source's. A DATAGRAM of one reaches
//!   the daemon address this invocation's environment names, and the addresses of the VPC the
//!   function is ATTACHED to — the host answers the route each takes and carries it
//!   ([`super::vpc_network`], [`super::guest_socket`]) — while a destination with no route
//!   raises the error a host with none answers; a STREAM socket's calls are the stream half
//!   appended beside this one ([`super::guest_socket`]), which connects to the data-plane
//!   listeners this emulator has opened.
//! * the DAEMON itself, in that same source: a datagram is read as the daemon's own framing —
//!   a header line declaring the format, then one segment document per line — and the documents
//!   it carries are submitted to X-Ray through `PutTraceSegments`, over the one AWS SDK seam a
//!   run makes a call on ([`super::aws_api`]), signed as the session of the function's own
//!   execution role. So a document a handler sends really is stored, under the trace the
//!   document itself names, and a function whose role is not granted `xray:PutTraceSegments`
//!   has its submission refused exactly as any other call of it would be.
//!
//! A datagram the framing does not admit is DROPPED rather than submitted, which is what the
//! daemon does with one: UDP reports nothing back, so a malformed datagram is lost and the
//! next well-formed one on the same socket still lands.
//!
//! Nothing here is the containment. Which services a run can reach, how large a request may
//! be and how many calls it may make are all host code's ([`super::aws_api`],
//! [`super::python`]); a handler that rebinds a name in this module changes none of them, and
//! the only destination it can deliver to is the in-process daemon either way.

/// The address the execution environment names the daemon at: "`AWS_XRAY_DAEMON_ADDRESS` –
/// The X-Ray daemon address in the format `IP_ADDRESS:PORT`" (the Lambda Developer Guide's
/// runtime environment variables), where the port is the `2000/udp` the X-Ray daemon
/// listens on.
///
/// The daemon is served IN PROCESS, by the source below, so the address a handler is given is
/// one its datagrams really reach rather than one it has to resolve.
pub const DAEMON_ADDRESS: &str = "127.0.0.1:2000";

/// The variable that address is reported in.
pub const DAEMON_ADDRESS_VARIABLE: &str = "AWS_XRAY_DAEMON_ADDRESS";

/// What a traced run does when a tracer finds no trace context: "`AWS_XRAY_CONTEXT_MISSING`
/// – For X-Ray tracing, Lambda sets this to `LOG_ERROR`" (the same guide), which has a
/// tracer log the miss and carry on rather than raise.
pub const CONTEXT_MISSING_VARIABLE: &str = "AWS_XRAY_CONTEXT_MISSING";

/// The value that variable carries.
pub const CONTEXT_MISSING_LOG_ERROR: &str = "LOG_ERROR";

/// The SOCKETS of the `socket` module this runtime supplies, appended to the names, errors
/// and conversions that module already states
/// ([`super::guest_http::runtime_stdlib_modules`]) — so there is ONE `socket` a handler can
/// import. What this adds to it is the socket itself, the daemon a datagram of one reaches and
/// the route a datagram takes into the VPC the function is attached to
/// ([`super::vpc_network`]); where a socket is a STREAM socket, its calls are the stream half
/// beside this one ([`super::guest_socket::STREAM_SOURCE`]).
pub const DATAGRAM_SOURCE: &str = r#"

# ---------------------------------------------------------------------------
# The datagram sockets of this runtime, the X-Ray daemon behind them, and the VPC the
# function is attached to.
#
# A datagram addressed to the daemon address the environment names is delivered to the
# daemon, which submits the segment documents it carries to X-Ray. Every other datagram is
# handed to the host, which answers the route it takes: a function attached to a VPC reaches
# the addresses of that VPC, from the private address of the interface it holds there, and the
# VPC's own DNS server replies to a query addressed to it. An address with no route is
# answered the way a host with none answers one.

import base64 as _base64
import json as _json
import os as _os

_host = __import__("_lambda_host")

# The daemon's own framing: a datagram is a header line declaring the format and the
# version, then one segment document per line.
_DAEMON_FORMAT = "json"

# The numbers a host answers these failures with, which a program reads off `errno`.
_EBADF = 9
_EOPNOTSUPP = 95
_ENETUNREACH = 101
_ENOTCONN = 107

# What a call that is not a datagram socket's is answered with.
_NOT_SUPPORTED = "Operation not supported"


def _daemon_address():
    """The address this invocation's environment names the daemon at."""
    return _os.environ.get("AWS_XRAY_DAEMON_ADDRESS") or ""


def _daemon_region():
    """The Region the submission is made in: the one the invocation runs in."""
    return _os.environ.get("AWS_REGION") or _os.environ.get("AWS_DEFAULT_REGION") or ""


def _datagram_text(data):
    if isinstance(data, (bytes, bytearray, memoryview)):
        return bytes(data).decode("utf-8", "replace")
    return str(data)


def _datagram_bytes(data):
    """The bytes one datagram carries, as a socket takes them."""
    if isinstance(data, (bytes, bytearray, memoryview)):
        return bytes(data)
    if isinstance(data, str):
        raise TypeError("a bytes-like object is required, not 'str'")
    return bytes(data)


def _datagram_endpoint(address):
    """The `(host, port)` one datagram is addressed to."""
    if not isinstance(address, (tuple, list)) or len(address) < 2:
        raise error("a datagram is addressed to a (host, port) address")
    host = address[0]
    if isinstance(host, (bytes, bytearray)):
        host = bytes(host).decode("utf-8", "replace")
    return str("" if host is None else host), _stream_port(address[1])


def _net_asked(verb, document):
    """One call on the host's own network seam, with a failure raised as the error a host
    answers it with."""
    answered = _json.loads(_host.net(verb, _json.dumps(document)))
    failed = answered.get("error")
    if failed:
        raise _stream_failure(failed)
    return answered


def _datagram_source(sock, host, port):
    """The address a datagram toward `host`:`port` leaves from.

    The daemon is served in THIS process, on this machine's own loopback interface, so a
    datagram to the address the environment names it at leaves from that interface; every other
    destination is the host's own route lookup, which answers the private address of the
    interface the function's VPC attachment holds — or raises what a host with no route to the
    address raises.
    """
    if sock._closed:
        raise error(_EBADF, "Bad file descriptor")
    if "%s:%s" % (host, port) == _daemon_address():
        return (_LOOPBACK_ADDRESS, 0)
    answered = _net_asked("route", {"host": host, "port": port})
    return (answered["source"], int(answered.get("sourcePort") or 0))


def _datagram_read(sock, size):
    """The oldest datagram that has arrived on `sock`, and the address it came from.

    A socket nothing has arrived on waits for one, and a wait of this runtime ends the way a
    host's own does when nothing comes: with the timeout a socket reports.
    """
    if sock._closed:
        raise error(_EBADF, "Bad file descriptor")
    if not sock._arrived:
        raise timeout("timed out")
    data, peer = sock._arrived.pop(0)
    try:
        size = int(size)
    except (TypeError, ValueError):
        raise error("the number of bytes to read is a number") from None
    if size < 0:
        raise ValueError("negative buffersize in recv")
    return (data[:size], peer)


def _segment_documents(data):
    """The segment documents one datagram carries, or none when the framing is not the
    daemon's: a header line that is a JSON object declaring the format, and then one document
    per line. A datagram the framing does not admit is dropped whole, and a line that is not a
    JSON document is dropped on its own."""
    lines = [line for line in _datagram_text(data).split("\n") if line.strip()]
    if len(lines) < 2:
        return []
    try:
        header = _json.loads(lines[0])
    except ValueError:
        return []
    if not isinstance(header, dict) or header.get("format") != _DAEMON_FORMAT:
        return []
    documents = []
    for line in lines[1:]:
        try:
            parsed = _json.loads(line)
        except ValueError:
            continue
        if isinstance(parsed, dict):
            documents.append(line)
    return documents


def _delivered(data):
    """Submit what this datagram carries to X-Ray. Nothing is reported back: a datagram
    carries no answer, so a submission the service refused is lost exactly as a datagram the
    daemon could not read is."""
    documents = _segment_documents(data)
    if not documents:
        return
    try:
        _host.aws(
            "xray",
            "PutTraceSegments",
            _daemon_region(),
            _json.dumps({"TraceSegmentDocuments": documents}),
        )
    except Exception:
        return


class socket(object):
    """One socket of the invocation.

    A DATAGRAM socket delivers to the daemon the environment names and to the VPC the function
    is attached to, and every other destination has no route. A STREAM socket carries bytes on
    a connection the host opens for it, to an address this emulator serves a resource's own
    data plane at: those calls are the stream half of this module, appended below.
    """

    def __init__(self, family=AF_INET, type=SOCK_STREAM, proto=0, fileno=None):
        self.family = family
        self.type = type
        self.proto = proto
        self._closed = False
        self._peer = None
        self._timeout = None
        # The connection a STREAM socket holds, once it has one. A datagram socket holds none.
        self._handle = None
        # The address this socket's traffic leaves from, once a route toward a destination has
        # been resolved; a socket with no route resolved is bound to nothing.
        self._source = None
        # The datagrams that have arrived on this socket and have not been read yet, oldest
        # first, each with the address it came from.
        self._arrived = []

    def __enter__(self):
        return self

    def __exit__(self, *exception):
        self.close()
        return False

    def __repr__(self):
        return "<socket family=%s type=%s proto=%s>" % (
            self.family,
            self.type,
            self.proto,
        )

    def _is_stream(self):
        return self.type != SOCK_DGRAM

    def close(self):
        if self._is_stream():
            _stream_close(self)
        self._arrived = []
        self._closed = True

    def detach(self):
        self._closed = True
        return -1

    def fileno(self):
        return -1

    def settimeout(self, value):
        self._timeout = value

    def gettimeout(self):
        return self._timeout

    def setblocking(self, blocking):
        self._timeout = None if blocking else 0.0

    def getblocking(self):
        return self._timeout != 0.0

    def setsockopt(self, *arguments):
        return None

    def getsockopt(self, level, option, *arguments):
        return 0

    def bind(self, address):
        return None

    def getsockname(self):
        """The address this socket's traffic leaves from.

        Once a route toward a destination has been resolved, that is the address the route
        leaves from — the private address of the interface the function's VPC attachment holds
        for an address of its VPC, and this machine's own interface for a destination served in
        this process. A socket with no route resolved is bound to no address of its own, which
        is the answer an unbound socket gives.
        """
        if self._source is not None:
            return self._source
        return ("0.0.0.0", 0)

    def getpeername(self):
        if self._peer is None:
            raise error(_ENOTCONN, "Transport endpoint is not connected")
        return self._peer

    def sendto(self, data, *arguments):
        if not arguments:
            raise TypeError("sendto() takes an address")
        if self._is_stream():
            raise error(_EOPNOTSUPP, _NOT_SUPPORTED)
        return self._deliver(data, arguments[-1])

    def connect(self, address):
        if self._is_stream():
            return _stream_connect(self, address)
        host, port = _datagram_endpoint(address)
        # A datagram socket's `connect` only states where its datagrams go, so the whole of
        # what it does is the route lookup — which is what tells a caller the address its
        # traffic toward that destination leaves from.
        self._source = _datagram_source(self, host, port)
        self._peer = (host, port)
        return None

    def connect_ex(self, address):
        try:
            self.connect(address)
        except OSError as failed:
            number = failed.errno
            return number if number else _ENETUNREACH
        return 0

    def send(self, data, flags=0):
        if self._is_stream():
            return _stream_send(self, data)
        if self._peer is None:
            raise error(_ENOTCONN, "Transport endpoint is not connected")
        return self._deliver(data, self._peer)

    def sendall(self, data, flags=0):
        if self._is_stream():
            payload = _stream_bytes(data)
            while payload:
                payload = payload[_stream_send(self, payload) :]
            return None
        self.send(data, flags)
        return None

    def recv(self, size, flags=0):
        if self._is_stream():
            return _stream_recv(self, size)
        return _datagram_read(self, size)[0]

    def recv_into(self, buffer, nbytes=None, flags=0):
        if self._is_stream():
            return _stream_recv_into(self, buffer, nbytes)
        view = memoryview(buffer)
        size = len(view) if not nbytes else min(int(nbytes), len(view))
        data = _datagram_read(self, size)[0]
        view[: len(data)] = data
        return len(data)

    def recvfrom(self, size, flags=0):
        if self._is_stream():
            return (_stream_recv(self, size), self.getpeername())
        return _datagram_read(self, size)

    def listen(self, backlog=0):
        raise error(_EOPNOTSUPP, _NOT_SUPPORTED)

    def accept(self):
        raise error(_EOPNOTSUPP, _NOT_SUPPORTED)

    def makefile(self, *arguments, **keywords):
        if self._is_stream():
            return _stream_makefile(self, *arguments, **keywords)
        raise error(_EOPNOTSUPP, _NOT_SUPPORTED)

    def shutdown(self, how):
        if self._is_stream():
            return _stream_shutdown(self, how)
        return None

    def _deliver(self, data, address):
        """Deliver ONE datagram, and keep whatever came back for a read of this socket.

        The daemon is in this process, so a datagram addressed to it is handed straight to it.
        Every other datagram goes through the host, which carries it to the address it is
        addressed to inside the function's own VPC and answers with the reply that address
        sent, if it sent one — the VPC's own DNS server does, and an address nothing is bound
        at answers nothing, exactly as UDP reports nothing back.
        """
        host, port = _datagram_endpoint(address)
        payload = _datagram_bytes(data)
        self._source = _datagram_source(self, host, port)
        if "%s:%s" % (host, port) == _daemon_address():
            _delivered(payload)
            return len(payload)
        answered = _net_asked(
            "datagram",
            {
                "host": host,
                "port": port,
                "data": _base64.b64encode(payload).decode("ascii"),
                "timeout": _stream_seconds(self._timeout),
            },
        )
        reply = answered.get("data")
        if reply:
            self._arrived.append((_base64.b64decode(reply), (host, port)))
        return len(payload)


SocketType = socket
"#;
