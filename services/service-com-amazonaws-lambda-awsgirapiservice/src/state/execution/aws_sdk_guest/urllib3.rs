// SPDX-License-Identifier: CC0-1.0

//! `urllib3`: the HTTP client the SDK this runtime ships is built on, which a managed
//! `python3.x` runtime therefore carries on a handler's import path.
//!
//! AWS Lambda's managed Python runtimes ship the AWS SDK for Python, whose own HTTP client is
//! `urllib3`, so `import urllib3` at module scope succeeds in a managed runtime with nothing
//! in the deployment package — and a large share of real handlers send a request with it.
//! It is the client AWS's own documented custom-resource response code answers with
//! (`http = urllib3.PoolManager()`, then `http.request("PUT", event["ResponseURL"], …)`), and
//! the one a handler calling an endpoint of its own reaches for.
//!
//! What these modules do and do not do:
//!
//! * The TRANSPORT is this runtime's own HTTP client ([`super::super::guest_http`]'s
//!   `http.client`), exactly as the published library is built on the standard library's: a
//!   pool makes its connection with that module's connection classes, so one request made
//!   here is one signed request the host carries into this same emulator, under every bound
//!   the host puts on one. Nothing here opens a socket, and nothing about an answer is
//!   synthesised: the status, the headers and the body a caller reads back are the ones the
//!   emulated service answered.
//! * The SURFACE is the library's: the pool manager and the connection pools a request is
//!   made through, the response it answers with (its `status`, `headers`, `data`, `json()`
//!   and the stream behind them), the retry and timeout settings a caller passes, the URL
//!   reading they share, and the exception hierarchy a caller catches — so a handler written
//!   against the published library runs unchanged.
//! * A failed request is RETRIED and a redirect FOLLOWED as the library does it: the `Retry`
//!   a caller passes decides both, an answer's own `Location` is where a redirect goes, and a
//!   retry that is used up raises `MaxRetryError`.
//! * Nothing here is the containment. Which addresses can be reached, the bounds on a request
//!   and the signature it carries are all host code's
//!   ([`super::super::guest_http`], [`super::super::python`]): a redirect is followed and a
//!   retry re-sent through that same seam, so each attempt spends one of the requests the run
//!   is allowed, cannot outlast the run's own deadline, and reaches no address this emulator
//!   does not serve — and the wait a backoff asks for is the runtime's own clamped sleep.

/// The modules of the HTTP client, under the paths the package holds them at.
#[must_use]
pub(super) fn modules() -> Vec<(&'static str, &'static str)> {
    vec![
        ("urllib3/__init__.py", INIT_SOURCE),
        ("urllib3/exceptions.py", EXCEPTIONS_SOURCE),
        ("urllib3/util/__init__.py", UTIL_INIT_SOURCE),
        ("urllib3/util/url.py", UTIL_URL_SOURCE),
        ("urllib3/util/ssl_.py", UTIL_SSL_SOURCE),
        ("urllib3/util/timeout.py", UTIL_TIMEOUT_SOURCE),
        ("urllib3/util/retry.py", UTIL_RETRY_SOURCE),
        ("urllib3/util/request.py", UTIL_REQUEST_SOURCE),
        ("urllib3/response.py", RESPONSE_SOURCE),
        ("urllib3/connection.py", CONNECTION_SOURCE),
        ("urllib3/connectionpool.py", CONNECTIONPOOL_SOURCE),
        ("urllib3/poolmanager.py", POOLMANAGER_SOURCE),
        ("urllib3/fields.py", FIELDS_SOURCE),
        ("urllib3/filepost.py", FILEPOST_SOURCE),
        ("urllib3/_request_methods.py", REQUEST_METHODS_SOURCE),
        ("urllib3/request.py", REQUEST_SOURCE),
    ]
}

/// `urllib3`: the names the package exports, and the one request a caller makes without a
/// pool manager of its own.
const INIT_SOURCE: &str = r#"
"""The HTTP client the AWS SDK this runtime ships is built on."""

from urllib3 import exceptions
from urllib3._request_methods import RequestMethods
from urllib3.connection import HTTPConnection, HTTPSConnection, VerifiedHTTPSConnection
from urllib3.connectionpool import (
    ConnectionPool,
    HTTPConnectionPool,
    HTTPSConnectionPool,
    connection_from_url,
)
from urllib3.exceptions import HTTPWarning
from urllib3.fields import RequestField
from urllib3.filepost import encode_multipart_formdata
from urllib3.poolmanager import PoolManager, ProxyManager, proxy_from_url
from urllib3.response import BaseHTTPResponse, HTTPHeaderDict, HTTPResponse
from urllib3.util.request import SKIP_HEADER, SKIPPABLE_HEADERS, make_headers
from urllib3.util.retry import Retry
from urllib3.util.ssl_ import SSLContext
from urllib3.util.timeout import Timeout
from urllib3.util.url import Url, parse_url

__all__ = [
    "BaseHTTPResponse",
    "ConnectionPool",
    "HTTPConnection",
    "HTTPConnectionPool",
    "HTTPHeaderDict",
    "HTTPResponse",
    "HTTPSConnection",
    "HTTPSConnectionPool",
    "PoolManager",
    "ProxyManager",
    "RequestField",
    "RequestMethods",
    "Retry",
    "SKIPPABLE_HEADERS",
    "SKIP_HEADER",
    "SSLContext",
    "Timeout",
    "Url",
    "VerifiedHTTPSConnection",
    "connection_from_url",
    "disable_warnings",
    "encode_multipart_formdata",
    "exceptions",
    "make_headers",
    "parse_url",
    "proxy_from_url",
    "request",
]

# The GENERATION of the library these modules implement, which is the one a managed
# `python3.x` runtime ships beside its SDK. It is not a published release of the library: no
# release of one is here to name.
__version__ = "2.0.0"

# The pool manager the module-level request is made through, as the library holds one.
_DEFAULT_POOL = PoolManager()


def request(
    method,
    url,
    *,
    body=None,
    fields=None,
    headers=None,
    preload_content=True,
    decode_content=True,
    redirect=True,
    retries=None,
    timeout=None,
    json=None,
):
    """One request, made through the pool manager this module holds."""
    return _DEFAULT_POOL.request(
        method,
        url,
        body=body,
        fields=fields,
        headers=headers,
        preload_content=preload_content,
        decode_content=decode_content,
        redirect=redirect,
        retries=retries,
        timeout=timeout,
        json=json,
    )


def disable_warnings(category=HTTPWarning):
    """Stop issuing the warnings of `category`, which is what a caller that has decided about
    them calls."""
    import warnings

    warnings.simplefilter("ignore", category)
"#;

/// `urllib3.exceptions`: the errors and warnings a caller catches, under the names the
/// library gives them.
const EXCEPTIONS_SOURCE: &str = r#"
"""The errors and warnings this client raises."""


class HTTPError(Exception):
    """The base of every error this library raises."""


class HTTPWarning(Warning):
    """The base of every warning this library issues."""


class PoolError(HTTPError):
    """An error about one connection pool."""

    def __init__(self, pool, message):
        self.pool = pool
        HTTPError.__init__(self, "%s: %s" % (pool, message))


class RequestError(PoolError):
    """An error about one request made through a pool."""

    def __init__(self, pool, url, message):
        self.url = url
        PoolError.__init__(self, pool, message)


class SSLError(HTTPError):
    """A TLS failure."""


class ProxyError(HTTPError):
    """A failure reaching a proxy."""

    def __init__(self, message, error=None):
        self.original_error = error
        HTTPError.__init__(self, message)


class DecodeError(HTTPError):
    """A body whose content encoding could not be decoded."""


class ProtocolError(HTTPError):
    """A connection that did not carry a well-formed answer."""


# The name the library's earlier line exported the same error under.
ConnectionError = ProtocolError


class MaxRetryError(RequestError):
    """Every retry of one request was used up."""

    def __init__(self, pool, url, reason=None):
        self.reason = reason
        RequestError.__init__(
            self,
            pool,
            url,
            "Max retries exceeded with url: %s (Caused by %r)" % (url, reason),
        )


class HostChangedError(RequestError):
    """A request addressed a host other than its pool's own."""

    def __init__(self, pool, url, retries=3):
        self.retries = retries
        RequestError.__init__(
            self, pool, url, "Tried to open a foreign host with url: %s" % (url,)
        )


class TimeoutStateError(HTTPError):
    """A timeout used in a state it does not apply to."""


class TimeoutError(HTTPError):
    """A request that ran out of time."""


class ReadTimeoutError(TimeoutError, RequestError):
    """The answer did not arrive in time."""


class ConnectTimeoutError(TimeoutError):
    """The connection was not made in time."""


class NewConnectionError(ConnectTimeoutError, HTTPError):
    """No connection could be made at all."""

    def __init__(self, conn, message):
        self.conn = conn
        HTTPError.__init__(self, "%s: %s" % (conn, message))

    @property
    def pool(self):
        return self.conn


class NameResolutionError(NewConnectionError):
    """A host name nothing resolves."""

    def __init__(self, host, conn, reason=None):
        self.reason = reason
        NewConnectionError.__init__(self, conn, "Failed to resolve '%s'" % (host,))
        self.host = host


class ClosedPoolError(PoolError):
    """A pool that is closed."""


class EmptyPoolError(PoolError):
    """A pool with no connection left to hand out."""


class FullPoolError(PoolError):
    """A pool with no room for another connection."""


class LocationValueError(ValueError, HTTPError):
    """A location that cannot be used."""


class LocationParseError(LocationValueError):
    """A location that cannot be read as a URL."""

    def __init__(self, location):
        self.location = location
        HTTPError.__init__(self, "Failed to parse: %s" % (location,))


class URLSchemeUnknown(LocationValueError):
    """A URL whose scheme this client does not make requests in."""

    def __init__(self, scheme):
        self.scheme = scheme
        HTTPError.__init__(self, "Not supported URL scheme %s" % (scheme,))


class ResponseError(HTTPError):
    """An answer that is itself what a retry was spent on."""

    GENERIC_ERROR = "too many error responses"
    SPECIFIC_ERROR = "too many {status_code} error responses"


class SecurityWarning(HTTPWarning):
    """A warning about the security of a request."""


class InsecureRequestWarning(SecurityWarning):
    """A request made without certificate verification."""


class SystemTimeWarning(SecurityWarning):
    """A system clock that cannot be trusted for certificate validity."""


class InsecurePlatformWarning(SecurityWarning):
    """A platform whose TLS cannot be configured."""


class NotOpenSSLWarning(SecurityWarning):
    """A TLS implementation that is not OpenSSL."""


class DependencyWarning(HTTPWarning):
    """A missing dependency of an optional feature."""


class ResponseNotChunked(ProtocolError, ValueError):
    """An answer read as chunked that was not."""


class BodyNotHttplibCompatible(HTTPError):
    """A body the client cannot read."""


class IncompleteRead(ProtocolError, ValueError):
    """An answer shorter than its own `Content-Length`."""

    def __init__(self, partial, expected):
        self.partial = partial
        self.expected = expected
        ProtocolError.__init__(
            self,
            "IncompleteRead(%i bytes read, %i more expected)" % (partial, expected),
        )


class InvalidChunkLength(ProtocolError, ValueError):
    """A chunk whose declared length is not one."""

    def __init__(self, response, length):
        self.response = response
        self.length = length
        ProtocolError.__init__(self, "Invalid chunk length: %r" % (length,))


class InvalidHeader(HTTPError):
    """A header that is not well formed."""


class HeaderParsingError(HTTPError):
    """Headers that could not be read."""


class UnrewindableBodyError(HTTPError):
    """A body that cannot be sent again, so the request cannot be retried."""
"#;

/// `urllib3.util`: the names the utilities package exports, which is where a caller reads the
/// retry, timeout and URL settings from.
const UTIL_INIT_SOURCE: &str = r#"
"""The settings a request is made under, and the reading of the URL it addresses."""

from urllib3.util.request import SKIP_HEADER, SKIPPABLE_HEADERS, make_headers
from urllib3.util.retry import Retry
from urllib3.util.ssl_ import (
    ALPN_PROTOCOLS,
    HAS_NEVER_CHECK_COMMON_NAME,
    IS_PYOPENSSL,
    IS_SECURETRANSPORT,
    SSLContext,
    assert_fingerprint,
    create_urllib3_context,
    is_ipaddress,
    resolve_cert_reqs,
    resolve_ssl_version,
    ssl_wrap_socket,
)
from urllib3.util.timeout import Timeout
from urllib3.util.url import Url, parse_url

__all__ = [
    "ALPN_PROTOCOLS",
    "HAS_NEVER_CHECK_COMMON_NAME",
    "IS_PYOPENSSL",
    "IS_SECURETRANSPORT",
    "SKIPPABLE_HEADERS",
    "SKIP_HEADER",
    "SSLContext",
    "Retry",
    "Timeout",
    "Url",
    "assert_fingerprint",
    "create_urllib3_context",
    "is_ipaddress",
    "make_headers",
    "parse_url",
    "resolve_cert_reqs",
    "resolve_ssl_version",
    "ssl_wrap_socket",
]
"#;

/// `urllib3.util.ssl_`: the TLS settings a connection of this client is made with.
const UTIL_SSL_SOURCE: &str = r#"
"""The TLS settings a connection is made with.

Every one of them is the standard library's own TLS layer: a context built here IS an
`ssl.SSLContext`, so a connection made with it is verified and negotiated by that layer
rather than by anything of this library's own.
"""

import hashlib
import ipaddress
import ssl
from ssl import (
    CERT_NONE,
    CERT_OPTIONAL,
    CERT_REQUIRED,
    HAS_NEVER_CHECK_COMMON_NAME,
    HAS_SNI,
    OP_NO_COMPRESSION,
    OP_NO_SSLv2,
    OP_NO_SSLv3,
    OP_NO_TICKET,
    PROTOCOL_TLS,
    PROTOCOL_TLS_CLIENT,
    SSLContext,
    SSLError,
    TLSVersion,
)

# Neither of the two optional TLS backends the library can be built on is here: the layer is
# the standard library's own.
IS_PYOPENSSL = False
IS_SECURETRANSPORT = False

# What this client offers in the protocol negotiation of a TLS handshake.
ALPN_PROTOCOLS = ["http/1.1"]

# The ciphers the library has always asked for where a caller names none; the layer's own
# default list is used when this is not set on a context.
DEFAULT_CIPHERS = ":".join(
    [
        "ECDHE+AESGCM",
        "ECDHE+CHACHA20",
        "DHE+AESGCM",
        "DHE+CHACHA20",
        "ECDH+AESGCM",
        "DH+AESGCM",
        "ECDH+AES",
        "DH+AES",
        "RSA+AESGCM",
        "RSA+AES",
        "!aNULL",
        "!eNULL",
        "!MD5",
        "!DSS",
        "!AESCCM",
    ]
)

_SSL_VERSIONS = {
    "PROTOCOL_TLS": PROTOCOL_TLS,
    "PROTOCOL_TLS_CLIENT": PROTOCOL_TLS_CLIENT,
}


def resolve_cert_reqs(candidate):
    """The verification mode `candidate` names: the strictest where it names none, the mode
    itself where it is one, and the mode of that name otherwise."""
    if candidate is None:
        return CERT_REQUIRED
    if isinstance(candidate, str):
        named = getattr(ssl, candidate, None)
        if named is None:
            named = getattr(ssl, "CERT_" + candidate, None)
        if named is None:
            raise ValueError("No such certificate requirement: %s" % candidate)
        return named
    return candidate


def resolve_ssl_version(candidate):
    """The protocol `candidate` names, with the client default where it names none."""
    if candidate is None:
        return PROTOCOL_TLS_CLIENT
    if isinstance(candidate, str):
        named = getattr(ssl, candidate, None)
        if named is None:
            named = getattr(ssl, "PROTOCOL_" + candidate, None)
        if named is None:
            raise ValueError("No such protocol: %s" % candidate)
        return named
    return candidate


def assert_fingerprint(cert, fingerprint):
    """Check the certificate `cert` really has the fingerprint `fingerprint` states."""
    if cert is None:
        raise SSLError("No certificate for the peer.")
    fingerprint = fingerprint.replace(":", "").lower()
    digest_length = len(fingerprint)
    hashfunc = {32: hashlib.md5, 40: hashlib.sha1, 64: hashlib.sha256}.get(digest_length)
    if hashfunc is None:
        raise SSLError("Fingerprint of invalid length: %s" % fingerprint)
    fingerprint_bytes = bytes.fromhex(fingerprint)
    cert_digest = hashfunc(cert).digest()
    if not _equal_digests(cert_digest, fingerprint_bytes):
        raise SSLError(
            'Fingerprints did not match. Expected "%s", got "%s".'
            % (fingerprint, cert_digest.hex())
        )


def _equal_digests(first, second):
    """Whether two digests are the same, compared without telling how far they matched."""
    import hmac

    return hmac.compare_digest(first, second)


def create_urllib3_context(
    ssl_version=None,
    cert_reqs=None,
    options=None,
    ciphers=None,
    ssl_minimum_version=None,
    ssl_maximum_version=None,
):
    """A TLS context with the settings this library asks of one."""
    if ssl_version is not None and ssl_minimum_version is not None:
        raise TypeError(
            "Can't specify both 'ssl_version' and either 'ssl_minimum_version' or "
            "'ssl_maximum_version'"
        )
    context = SSLContext(resolve_ssl_version(ssl_version))
    if ssl_minimum_version is not None:
        context.minimum_version = ssl_minimum_version
    if ssl_maximum_version is not None:
        context.maximum_version = ssl_maximum_version
    if ciphers is not None:
        context.set_ciphers(ciphers)
    if options is None:
        # The two retired protocol versions are never negotiated, and neither is
        # compression, which is what the library's own default options state.
        options = 0
        options |= OP_NO_SSLv2
        options |= OP_NO_SSLv3
        options |= OP_NO_COMPRESSION
        options |= OP_NO_TICKET
    context.options |= options
    if hasattr(context, "post_handshake_auth"):
        context.post_handshake_auth = True
    cert_reqs = resolve_cert_reqs(cert_reqs)
    context.verify_mode = cert_reqs
    context.check_hostname = cert_reqs != CERT_NONE
    if hasattr(context, "set_alpn_protocols"):
        try:
            context.set_alpn_protocols(ALPN_PROTOCOLS)
        except NotImplementedError:
            pass
    return context


def ssl_wrap_socket(
    sock,
    keyfile=None,
    certfile=None,
    cert_reqs=None,
    ca_certs=None,
    server_hostname=None,
    ssl_version=None,
    ciphers=None,
    ssl_context=None,
    ca_cert_dir=None,
    key_password=None,
    ca_cert_data=None,
    tls_in_tls=False,
):
    """`sock` wrapped in TLS, with the certificates and the hostname a caller named."""
    context = ssl_context
    if context is None:
        context = create_urllib3_context(ssl_version, cert_reqs, ciphers=ciphers)
    if ca_certs or ca_cert_dir or ca_cert_data:
        context.load_verify_locations(ca_certs, ca_cert_dir, ca_cert_data)
    elif ssl_context is None and hasattr(context, "load_default_certs"):
        context.load_default_certs()
    if certfile:
        if key_password is None:
            context.load_cert_chain(certfile, keyfile)
        else:
            context.load_cert_chain(certfile, keyfile, key_password)
    if server_hostname is not None and is_ipaddress(server_hostname):
        # A certificate is not verified against an address, so the name is not sent.
        context.check_hostname = False
        return context.wrap_socket(sock)
    return context.wrap_socket(sock, server_hostname=server_hostname)


def is_ipaddress(hostname):
    """Whether `hostname` is an ADDRESS rather than a name, which decides whether it is sent
    in the handshake and verified against the certificate."""
    if isinstance(hostname, bytes):
        hostname = hostname.decode("ascii")
    text = str(hostname).strip("[]")
    try:
        ipaddress.ip_address(text)
    except ValueError:
        return False
    return True


def _is_key_file_encrypted(key_file):
    """Whether the key in `key_file` is one a password has to be given for."""
    with open(key_file, "r") as reading:
        for line in reading:
            if "ENCRYPTED" in line:
                return True
    return False
"#;

/// `urllib3.util.url`: a URL in the parts a request is built from.
const UTIL_URL_SOURCE: &str = r#"
"""Reading one URL into the parts a request is built from."""

import collections
from urllib.parse import urlsplit

from urllib3.exceptions import LocationParseError

_Url = collections.namedtuple(
    "Url", ["scheme", "auth", "host", "port", "path", "query", "fragment"]
)


class Url(_Url):
    """One URL, in its own parts."""

    def __new__(
        cls,
        scheme=None,
        auth=None,
        host=None,
        port=None,
        path=None,
        query=None,
        fragment=None,
    ):
        if path and not path.startswith("/"):
            path = "/" + path
        if scheme is not None:
            scheme = scheme.lower()
        return _Url.__new__(cls, scheme, auth, host, port, path, query, fragment)

    @property
    def hostname(self):
        return self.host

    @property
    def request_uri(self):
        """The target one request line carries: the path and the query, as sent."""
        uri = self.path or "/"
        if self.query is not None:
            return "%s?%s" % (uri, self.query)
        return uri

    @property
    def netloc(self):
        if self.host is None:
            return None
        if self.port is not None:
            return "%s:%d" % (self.host, self.port)
        return self.host

    @property
    def authority(self):
        if self.auth is not None and self.netloc is not None:
            return "%s@%s" % (self.auth, self.netloc)
        return self.netloc

    @property
    def url(self):
        """The URL these parts spell, built back up."""
        spelled = ""
        if self.scheme is not None:
            spelled += self.scheme + "://"
        if self.authority is not None:
            spelled += self.authority
        if self.path is not None:
            spelled += self.path
        if self.query is not None:
            spelled += "?" + self.query
        if self.fragment is not None:
            spelled += '#' + self.fragment
        return spelled

    def __str__(self):
        return self.url


def parse_url(url):
    """The parts `url` carries.

    The reading itself is the standard library's (`urllib.parse.urlsplit`), which is the
    parser this runtime already has for the form: what is done here is only what THIS library
    adds to it — a value that names a host without a scheme is accepted, which `urlsplit`
    reads as a path, so the `//` that says "this is an authority" is supplied before it is
    read; and a port that is not a number names no location at all.
    """
    if not url:
        return Url()
    target = url
    if "://" not in target and not target.startswith("//") and not target.startswith("/"):
        target = "//" + target
    split = urlsplit(target)
    try:
        port = split.port
    except ValueError as error:
        raise LocationParseError(url) from error
    host = split.hostname
    # `urlsplit` hands an IPv6 literal over without the brackets a request carries it in.
    if host is not None and ":" in host:
        host = "[%s]" % (host,)
    auth = None
    if split.username is not None or split.password is not None:
        auth = split.username or ""
        if split.password is not None:
            auth = "%s:%s" % (auth, split.password)
    return Url(
        scheme=split.scheme or None,
        auth=auth,
        host=host or None,
        port=port,
        path=split.path or None,
        query=split.query or None,
        fragment=split.fragment or None,
    )
"#;

/// `urllib3.util.timeout`: how long one request may take.
const UTIL_TIMEOUT_SOURCE: &str = r#"
"""The time one request is allowed."""

import time

from urllib3.exceptions import TimeoutStateError

# The sentinel standing for "whatever the default is", which is what a caller that states
# only one half of a timeout leaves the other at.
_DEFAULT = object()


class Timeout:
    """The connect and read spans one request is made under.

    The spans are carried for the caller that reads them back: what really bounds a request
    here is the host, which gives a run its own deadline and never lets one request outlast
    it.
    """

    DEFAULT_TIMEOUT = _DEFAULT

    def __init__(self, total=None, connect=_DEFAULT, read=_DEFAULT):
        self._connect = self._validate_timeout(connect, "connect")
        self._read = self._validate_timeout(read, "read")
        self.total = self._validate_timeout(total, "total")
        self._start_connect = None

    def __repr__(self):
        return "%s(connect=%r, read=%r, total=%r)" % (
            type(self).__name__,
            self._connect,
            self._read,
            self.total,
        )

    @classmethod
    def _validate_timeout(cls, value, name):
        if value is _DEFAULT or value is None:
            return value
        if isinstance(value, bool):
            raise ValueError("Timeout cannot be a boolean value.")
        try:
            number = float(value)
        except (TypeError, ValueError):
            raise ValueError(
                "Timeout value %s was %s, but it must be an int, float or None."
                % (name, value)
            )
        if number <= 0:
            raise ValueError(
                "Attempted to set %s timeout to %s, but the timeout cannot be set to a "
                "value less than or equal to 0." % (name, value)
            )
        return number

    @classmethod
    def from_float(cls, timeout):
        """One span standing for both halves, which is what a bare number means."""
        return cls(read=timeout, connect=timeout)

    def clone(self):
        return Timeout(connect=self._connect, read=self._read, total=self.total)

    def start_connect(self):
        if self._start_connect is not None:
            raise TimeoutStateError("Timeout timer has already been started.")
        self._start_connect = time.monotonic()
        return self._start_connect

    def get_connect_duration(self):
        if self._start_connect is None:
            raise TimeoutStateError(
                "Can't get connect duration for timer that has not started."
            )
        return time.monotonic() - self._start_connect

    @property
    def connect_timeout(self):
        if self.total is None:
            return self._connect
        if self._connect is None or self._connect is _DEFAULT:
            return self.total
        return min(self._connect, self.total)

    @property
    def read_timeout(self):
        if (
            self.total is not None
            and self.total is not _DEFAULT
            and self._read is not None
            and self._read is not _DEFAULT
        ):
            if self._start_connect is not None:
                return max(0, min(self.total - self.get_connect_duration(), self._read))
            return max(0, min(self.total, self._read))
        if self.total is not None and self.total is not _DEFAULT:
            return max(0, self.total)
        return self._read
"#;

/// `urllib3.util.retry`: when a failed request is made again.
const UTIL_RETRY_SOURCE: &str = r#"
"""How a failed request is retried, and how a redirect is counted."""

import collections
import email.utils
import re
import time

from urllib3.exceptions import (
    ConnectTimeoutError,
    InvalidHeader,
    MaxRetryError,
    ProtocolError,
    ReadTimeoutError,
    ResponseError,
)

RequestHistory = collections.namedtuple(
    "RequestHistory", ["method", "url", "error", "status", "redirect_location"]
)


class Retry:
    """What is left of a request's retries, and what each one is spent on.

    Every count is decremented into a NEW value, as the library does it, so the settings a
    caller passed are never changed under it.
    """

    DEFAULT_ALLOWED_METHODS = frozenset(
        ["HEAD", "GET", "PUT", "DELETE", "OPTIONS", "TRACE"]
    )
    RETRY_AFTER_STATUS_CODES = frozenset([413, 429, 503])
    DEFAULT_REMOVE_HEADERS_ON_REDIRECT = frozenset(
        ["Cookie", "Authorization", "Proxy-Authorization"]
    )
    DEFAULT_BACKOFF_MAX = 120
    BACKOFF_MAX = 120

    def __init__(
        self,
        total=10,
        connect=None,
        read=None,
        redirect=None,
        status=None,
        other=None,
        allowed_methods=DEFAULT_ALLOWED_METHODS,
        status_forcelist=None,
        backoff_factor=0,
        backoff_max=DEFAULT_BACKOFF_MAX,
        backoff_jitter=0.0,
        raise_on_redirect=True,
        raise_on_status=True,
        history=None,
        respect_retry_after_header=True,
        remove_headers_on_redirect=DEFAULT_REMOVE_HEADERS_ON_REDIRECT,
    ):
        self.total = total
        self.connect = connect
        self.read = read
        self.status = status
        self.other = other
        if redirect is False or total is False:
            redirect = 0
            raise_on_redirect = False
        self.redirect = redirect
        self.status_forcelist = status_forcelist or set()
        self.allowed_methods = allowed_methods
        self.backoff_factor = backoff_factor
        self.backoff_max = backoff_max
        self.backoff_jitter = backoff_jitter
        self.raise_on_redirect = raise_on_redirect
        self.raise_on_status = raise_on_status
        self.history = history or tuple()
        self.respect_retry_after_header = respect_retry_after_header
        self.remove_headers_on_redirect = frozenset(
            header.lower() for header in remove_headers_on_redirect
        )

    def new(self, **kw):
        settings = dict(
            total=self.total,
            connect=self.connect,
            read=self.read,
            redirect=self.redirect,
            status=self.status,
            other=self.other,
            allowed_methods=self.allowed_methods,
            status_forcelist=self.status_forcelist,
            backoff_factor=self.backoff_factor,
            backoff_max=self.backoff_max,
            backoff_jitter=self.backoff_jitter,
            raise_on_redirect=self.raise_on_redirect,
            raise_on_status=self.raise_on_status,
            history=self.history,
            respect_retry_after_header=self.respect_retry_after_header,
            remove_headers_on_redirect=self.remove_headers_on_redirect,
        )
        settings.update(kw)
        return type(self)(**settings)

    @classmethod
    def from_int(cls, retries, redirect=True, default=None):
        """The settings a caller's `retries` value stands for: a `Retry` as given, a count as
        that many of them, and nothing at all as the default."""
        if retries is None:
            retries = default if default is not None else 3
        if isinstance(retries, Retry):
            return retries
        redirect = bool(redirect) and None
        return cls(retries, redirect=redirect)

    def get_backoff_time(self):
        """How long to wait before the next attempt, which is nothing until a request has
        really failed once."""
        spent = list(
            reversed([entry for entry in self.history if entry.redirect_location is None])
        )
        consecutive = 0
        for entry in spent:
            if entry.status is None and entry.error is None:
                break
            consecutive += 1
        if consecutive <= 1:
            return 0
        return min(self.backoff_max, self.backoff_factor * (2 ** (consecutive - 1)))

    def parse_retry_after(self, retry_after):
        """The seconds a `Retry-After` header names, whether it names a count or a date."""
        if re.match(r"^\s*[0-9]+\s*$", retry_after):
            return int(retry_after)
        parsed = email.utils.parsedate_to_datetime(retry_after)
        if parsed is None:
            raise InvalidHeader("Invalid Retry-After header: %s" % retry_after)
        return max(0, parsed.timestamp() - time.time())

    def get_retry_after(self, response):
        retry_after = response.headers.get("Retry-After")
        if retry_after is None:
            return None
        return self.parse_retry_after(retry_after)

    def sleep_for_retry(self, response=None):
        if response is None:
            return False
        waiting = self.get_retry_after(response)
        if waiting:
            time.sleep(waiting)
            return True
        return False

    def _sleep_backoff(self):
        backoff = self.get_backoff_time()
        if backoff <= 0:
            return
        time.sleep(backoff)

    def sleep(self, response=None):
        """Wait as long as this attempt owes: the answer's own `Retry-After` where it named
        one, and the backoff otherwise."""
        if self.respect_retry_after_header and response is not None:
            if self.sleep_for_retry(response):
                return
        self._sleep_backoff()

    def _is_connection_error(self, error):
        return isinstance(error, (ConnectTimeoutError,))

    def _is_read_error(self, error):
        return isinstance(error, (ReadTimeoutError, ProtocolError))

    def _is_method_retryable(self, method):
        if self.allowed_methods and method.upper() not in self.allowed_methods:
            return False
        return True

    def is_retry(self, method, status_code, has_retry_after=False):
        """Whether an answer with this status is one a retry is spent on."""
        if not self._is_method_retryable(method):
            return False
        if self.status_forcelist and status_code in self.status_forcelist:
            return True
        return bool(
            self.total
            and self.respect_retry_after_header
            and has_retry_after
            and status_code in self.RETRY_AFTER_STATUS_CODES
        )

    def is_exhausted(self):
        """Whether every count has been spent."""
        counts = [
            self.total,
            self.connect,
            self.read,
            self.redirect,
            self.status,
            self.other,
        ]
        counts = [count for count in counts if count is not None]
        if not counts:
            return False
        return min(counts) < 0

    def increment(
        self,
        method=None,
        url=None,
        response=None,
        error=None,
        _pool=None,
        _stacktrace=None,
    ):
        """What is left after this attempt, or `MaxRetryError` when nothing is."""
        if self.total is False and error:
            raise error
        total = self.total
        if total is not None:
            total -= 1
        connect = self.connect
        read = self.read
        redirect = self.redirect
        status_count = self.status
        other = self.other
        cause = "unknown"
        status = None
        redirect_location = None
        if error and self._is_connection_error(error):
            if connect is False:
                raise error
            if connect is not None:
                connect -= 1
        elif error and self._is_read_error(error):
            if read is False or (method is not None and not self._is_method_retryable(method)):
                raise error
            if read is not None:
                read -= 1
        elif error:
            if other is not None:
                other -= 1
        elif response is not None and response.get_redirect_location():
            if redirect is not None:
                redirect -= 1
            cause = "too many redirects"
            redirect_location = response.get_redirect_location()
            status = response.status
        else:
            cause = ResponseError.GENERIC_ERROR
            if response is not None and response.status:
                if status_count is not None:
                    status_count -= 1
                cause = ResponseError.SPECIFIC_ERROR.format(status_code=response.status)
                status = response.status
        history = self.history + (
            RequestHistory(method, url, error, status, redirect_location),
        )
        left = self.new(
            total=total,
            connect=connect,
            read=read,
            redirect=redirect,
            status=status_count,
            other=other,
            history=history,
        )
        if left.is_exhausted():
            raise MaxRetryError(_pool, url, error or ResponseError(cause))
        return left

    def __repr__(self):
        return (
            "%s(total=%r, connect=%r, read=%r, redirect=%r, status=%r)"
            % (
                type(self).__name__,
                self.total,
                self.connect,
                self.read,
                self.redirect,
                self.status,
            )
        )


# The settings a request is made under when its caller states none.
Retry.DEFAULT = Retry(3)
"#;

/// `urllib3.util.request`: the headers a caller asks the library to build.
const UTIL_REQUEST_SOURCE: &str = r#"
"""The headers a request carries that a caller does not write out itself."""

import base64

# The value a caller puts on a header it wants LEFT OFF a request, which is how the library
# lets one drop a header it would otherwise set itself.
SKIP_HEADER = "@@@SKIP_HEADER@@@"
SKIPPABLE_HEADERS = frozenset(["accept-encoding", "host", "user-agent"])

ACCEPT_ENCODING = "gzip,deflate"


def make_headers(
    keep_alive=None,
    accept_encoding=None,
    user_agent=None,
    basic_auth=None,
    proxy_basic_auth=None,
    disable_cache=None,
):
    """The headers these settings spell, as the library builds them."""
    headers = {}
    if accept_encoding:
        if isinstance(accept_encoding, str):
            pass
        elif isinstance(accept_encoding, list):
            accept_encoding = ",".join(accept_encoding)
        else:
            accept_encoding = ACCEPT_ENCODING
        headers["accept-encoding"] = accept_encoding
    if user_agent:
        headers["user-agent"] = user_agent
    if keep_alive:
        headers["connection"] = "keep-alive"
    if basic_auth:
        headers["authorization"] = "Basic " + base64.b64encode(
            basic_auth.encode("utf-8")
        ).decode("utf-8")
    if proxy_basic_auth:
        headers["proxy-authorization"] = "Basic " + base64.b64encode(
            proxy_basic_auth.encode("utf-8")
        ).decode("utf-8")
    if disable_cache:
        headers["cache-control"] = "no-cache"
    return headers
"#;

/// `urllib3.response`: the answer a request is answered with, and the header map it carries.
const RESPONSE_SOURCE: &str = r#"
"""One answer, read the way this library's own response is read."""

import io
import json as _json

from urllib3.exceptions import DecodeError

# The statuses whose `Location` a redirect is followed to.
REDIRECT_STATUSES = [301, 302, 303, 307, 308]


class HTTPHeaderDict:
    """The headers of a request or an answer: keyed case-insensitively, holding every value a
    repeated header carried, and reporting the names as they arrived."""

    def __init__(self, headers=None, **kwargs):
        self._container = {}
        if headers is not None:
            self.extend(headers)
        if kwargs:
            self.extend(kwargs)

    def __setitem__(self, key, value):
        self._container[key.lower()] = [key, value]

    def __getitem__(self, key):
        held = self._container[key.lower()]
        return ", ".join(held[1:])

    def __delitem__(self, key):
        del self._container[key.lower()]

    def __contains__(self, key):
        return isinstance(key, str) and key.lower() in self._container

    def __len__(self):
        return len(self._container)

    def __iter__(self):
        for held in list(self._container.values()):
            yield held[0]

    def __eq__(self, other):
        if not isinstance(other, (HTTPHeaderDict, dict)):
            return NotImplemented
        other = HTTPHeaderDict(other)
        mine = dict((name.lower(), self[name]) for name in self)
        theirs = dict((name.lower(), other[name]) for name in other)
        return mine == theirs

    def __ne__(self, other):
        equal = self.__eq__(other)
        if equal is NotImplemented:
            return equal
        return not equal

    def __repr__(self):
        return "%s(%r)" % (type(self).__name__, dict(self.itermerged()))

    def get(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            return default

    def getlist(self, key, default=None):
        """Every value the header `key` carried, in the order they arrived."""
        held = self._container.get(key.lower())
        if held is None:
            return [] if default is None else default
        return list(held[1:])

    getheaders = getlist
    getallmatchingheaders = getlist
    iget = getlist

    def add(self, key, value, combine=False):
        """Keep `value` BESIDE the values this header already carried, which is what a
        repeated header does."""
        held = self._container.setdefault(key.lower(), [key])
        if combine and len(held) > 1:
            held[1] = held[1] + ", " + value
        else:
            held.append(value)

    def extend(self, *args, **kwargs):
        if len(args) > 1:
            raise TypeError(
                "extend() takes at most 1 positional arguments (%d given)" % (len(args),)
            )
        other = args[0] if args else None
        if isinstance(other, HTTPHeaderDict):
            for name in other:
                for value in other.getlist(name):
                    self.add(name, value)
        elif isinstance(other, dict):
            for name, value in other.items():
                self.add(name, value)
        elif other is not None:
            for pair in other:
                name, value = pair
                self.add(name, value)
        for name, value in kwargs.items():
            self.add(name, value)

    def items(self):
        """One entry per VALUE, which is what a repeated header is read as."""
        found = []
        for name in self:
            for value in self.getlist(name):
                found.append((name, value))
        return found

    def keys(self):
        return list(self)

    def values(self):
        return [value for _, value in self.items()]

    def itermerged(self):
        for name in self:
            yield name, self[name]

    def copy(self):
        return HTTPHeaderDict(self)

    def discard(self, key):
        self._container.pop(key.lower(), None)

    def setdefault(self, key, default=""):
        if key in self:
            return self[key]
        self[key] = default
        return default

    def _prepare_for_method_change(self):
        """The headers a redirect that changed the method carries: the ones describing a body
        it no longer sends are dropped."""
        for name in ("content-length", "content-type", "transfer-encoding"):
            self.discard(name)
        return self


def _as_bytes(body):
    """The bytes a body of any shape a caller passes carries."""
    if body is None:
        return b""
    if isinstance(body, (bytes, bytearray, memoryview)):
        return bytes(body)
    if isinstance(body, str):
        return body.encode("utf-8")
    read = getattr(body, "read", None)
    if read is not None:
        return _as_bytes(read())
    raise TypeError("a response body cannot be %r" % (type(body).__name__,))


class BaseHTTPResponse(io.IOBase):
    """What every answer of this library carries."""

    def __init__(
        self,
        body=b"",
        headers=None,
        status=0,
        version=11,
        version_string="HTTP/1.1",
        reason=None,
        preload_content=True,
        decode_content=True,
        original_response=None,
        pool=None,
        connection=None,
        msg=None,
        retries=None,
        enforce_content_length=True,
        request_method=None,
        request_url=None,
        auto_close=True,
    ):
        self.headers = HTTPHeaderDict(headers)
        self.status = status
        self.version = version
        self.version_string = version_string
        self.reason = reason
        self.decode_content = decode_content
        self.retries = retries
        self.auto_close = auto_close
        self.chunked = False
        self._pool = pool
        self._connection = connection
        self._original_response = original_response
        self.msg = msg if msg is not None else self.headers
        self._request_method = request_method
        self._request_url = request_url
        self._raw = _as_bytes(body)
        self._decoded = None
        self._position = 0
        self.length_remaining = len(self._raw)
        self._body = None
        if preload_content:
            self._body = self.read(decode_content=decode_content)

    # What the answer IS, for a caller that asked for the whole of it.

    @property
    def data(self):
        """The whole body of the answer, decoded as its own `Content-Encoding` says."""
        if self._body is not None:
            return self._body
        return self._decoded_body()

    @property
    def url(self):
        return self._request_url

    @url.setter
    def url(self, value):
        self._request_url = value

    @property
    def connection(self):
        return self._connection

    @property
    def closed(self):
        return self._position >= len(self._decoded_body())

    def _decoded_body(self):
        """The body with its content encoding undone, computed once."""
        if self._decoded is not None:
            return self._decoded
        self._decoded = self._raw
        if self.decode_content:
            encoding = (self.headers.get("content-encoding") or "").lower().strip()
            if encoding in ("gzip", "x-gzip"):
                self._decoded = _gunzipped(self._raw)
            elif encoding == "deflate":
                self._decoded = _inflated(self._raw)
        return self._decoded

    # Reading it, as a stream.

    def read(self, amt=None, decode_content=None, cache_content=False):
        if decode_content is not None:
            self.decode_content = decode_content
            self._decoded = None
        body = self._decoded_body()
        if amt is None:
            read = body[self._position :]
            self._position = len(body)
        else:
            read = body[self._position : self._position + amt]
            self._position += len(read)
        self.length_remaining = max(0, len(self._raw) - self._position)
        if cache_content:
            self._body = read
        return read

    def read1(self, amt=None, decode_content=None):
        return self.read(amt, decode_content=decode_content)

    def readinto(self, buffer):
        read = self.read(len(buffer))
        buffer[: len(read)] = read
        return len(read)

    def readable(self):
        return True

    def writable(self):
        return False

    def seekable(self):
        return False

    def tell(self):
        return self._position

    def stream(self, amt=65536, decode_content=None):
        """The body in chunks, which is how a caller reads an answer it did not preload."""
        while True:
            read = self.read(amt, decode_content=decode_content)
            if not read:
                return
            yield read

    def __iter__(self):
        held = b""
        for chunk in self.stream(decode_content=True):
            held += chunk
            while b"\n" in held:
                line, _, held = held.partition(b"\n")
                yield line + b"\n"
        if held:
            yield held

    def json(self):
        """The answer's body read as the JSON document it carries."""
        return _json.loads(self.data.decode("utf-8"))

    # What a caller reads ABOUT the answer.

    def info(self):
        return self.headers

    def geturl(self):
        return self._request_url

    def getheaders(self):
        return self.headers

    def getheader(self, name, default=None):
        return self.headers.get(name, default)

    def get_redirect_location(self):
        """Where this answer redirects to, or `False` for one that does not."""
        if self.status in REDIRECT_STATUSES:
            return self.headers.get("location")
        return False

    def release_conn(self):
        return None

    def drain_conn(self):
        return None

    def close(self):
        self._position = len(self._decoded_body())
        return None

    def __enter__(self):
        return self

    def __exit__(self, *_arguments):
        self.close()
        return False


class HTTPResponse(BaseHTTPResponse):
    """The answer one request made through a pool is answered with."""


def _gunzipped(body):
    """`body` with its gzip encoding undone."""
    try:
        import gzip
    except ImportError as error:
        raise DecodeError("Received response with content-encoding: gzip") from error
    try:
        return gzip.decompress(body)
    except Exception as error:
        raise DecodeError(
            "Received response with content-encoding: gzip, but failed to decode it."
        ) from error


def _inflated(body):
    """`body` with its deflate encoding undone, in either of the two framings a server may
    have sent it in."""
    try:
        import zlib
    except ImportError as error:
        raise DecodeError("Received response with content-encoding: deflate") from error
    try:
        return zlib.decompress(body)
    except Exception:
        try:
            return zlib.decompress(body, -zlib.MAX_WBITS)
        except Exception as error:
            raise DecodeError(
                "Received response with content-encoding: deflate, but failed to decode it."
            ) from error
"#;

/// `urllib3.connection`: the connections a pool makes, which are this runtime's own.
const CONNECTION_SOURCE: &str = r#"
"""The connections a pool of this library makes: the runtime's own HTTP client."""

import http.client

port_by_scheme = {"http": 80, "https": 443}


class HTTPConnection(http.client.HTTPConnection):
    """One connection to a host, over the runtime's own client."""

    default_port = port_by_scheme["http"]


class HTTPSConnection(http.client.HTTPSConnection):
    """The same for an `https` URL."""

    default_port = port_by_scheme["https"]


# The name the library's earlier line exported the HTTPS connection under.
VerifiedHTTPSConnection = HTTPSConnection
"#;

/// `urllib3.connectionpool`: the pool one host's requests are made through.
const CONNECTIONPOOL_SOURCE: &str = r#"
"""The pool of connections to ONE host, which is what makes a request."""

import socket

from urllib3._request_methods import RequestMethods
from urllib3.connection import HTTPConnection, HTTPSConnection, port_by_scheme
from urllib3.exceptions import (
    ClosedPoolError,
    HostChangedError,
    LocationValueError,
    NewConnectionError,
    ProtocolError,
    ReadTimeoutError,
    SSLError,
)
from urllib3.exceptions import TimeoutError as Urllib3TimeoutError
from urllib3.response import HTTPResponse
from urllib3.util.retry import Retry
from urllib3.util.timeout import Timeout
from urllib3.util.url import parse_url

_DEFAULT = Timeout.DEFAULT_TIMEOUT


class ConnectionPool:
    """What every pool of this library carries: the host it makes requests to."""

    scheme = None

    def __init__(self, host, port=None):
        if not host:
            raise LocationValueError("No host specified.")
        self.host = host
        self.port = port

    def __str__(self):
        return "%s(host=%r, port=%r)" % (type(self).__name__, self.host, self.port)

    def __enter__(self):
        return self

    def __exit__(self, *_arguments):
        self.close()
        return False

    def close(self):
        return None


class HTTPConnectionPool(ConnectionPool, RequestMethods):
    """The pool one host's `http` requests are made through."""

    scheme = "http"
    ConnectionCls = HTTPConnection

    def __init__(
        self,
        host,
        port=None,
        timeout=_DEFAULT,
        maxsize=1,
        block=False,
        headers=None,
        retries=None,
        _proxy=None,
        _proxy_headers=None,
        _proxy_config=None,
        **conn_kw,
    ):
        ConnectionPool.__init__(self, host, port)
        RequestMethods.__init__(self, headers)
        if not isinstance(timeout, Timeout):
            timeout = Timeout.from_float(timeout) if timeout is not _DEFAULT else Timeout()
        self.timeout = timeout
        self.retries = retries
        self.proxy = _proxy
        self.proxy_headers = _proxy_headers or {}
        self.conn_kw = conn_kw
        self.num_connections = 0
        self.num_requests = 0
        self._closed = False

    def close(self):
        self._closed = True

    def _new_conn(self):
        """A connection to this pool's host, as the library builds one."""
        self.num_connections += 1
        return self.ConnectionCls(host=self.host, port=self.port)

    def is_same_host(self, url):
        """Whether `url` addresses this pool's own host, which is what a pool answers for."""
        if url.startswith("/"):
            return True
        parsed = parse_url(url)
        scheme = parsed.scheme or "http"
        host = parsed.host
        port = parsed.port or port_by_scheme.get(scheme)
        mine_port = self.port or port_by_scheme.get(self.scheme)
        return (scheme, host, port) == (self.scheme, self.host, mine_port)

    def urlopen(
        self,
        method,
        url,
        body=None,
        headers=None,
        retries=None,
        redirect=True,
        assert_same_host=True,
        timeout=_DEFAULT,
        pool_timeout=None,
        release_conn=None,
        chunked=False,
        body_pos=None,
        preload_content=True,
        decode_content=True,
        **response_kw,
    ):
        """Make one request and answer the response it was answered with.

        A request that could not be carried at all is retried as the `Retry` in force allows,
        and a redirect this pool's own host answers is followed; a redirect to another host is
        the answer, which is what the pool manager above follows.
        """
        if self._closed:
            raise ClosedPoolError(self, "Pool is closed.")
        if headers is None:
            headers = self.headers
        if not isinstance(retries, Retry):
            retries = Retry.from_int(retries, redirect=redirect, default=self.retries)
        if assert_same_host and not self.is_same_host(url):
            raise HostChangedError(self, url, retries)
        while True:
            parsed = parse_url(url)
            try:
                answered = self._make_request(
                    method,
                    parsed.request_uri,
                    body,
                    headers,
                    url,
                    preload_content,
                    decode_content,
                    retries,
                )
            except (
                NewConnectionError,
                ProtocolError,
                ReadTimeoutError,
                SSLError,
                Urllib3TimeoutError,
            ) as error:
                retries = retries.increment(method, url, error=error, _pool=self)
                retries.sleep()
                continue
            location = redirect and answered.get_redirect_location()
            if location and self.is_same_host(location):
                if answered.status == 303 and method.upper() != "HEAD":
                    method = "GET"
                    body = None
                retries = retries.increment(method, url, response=answered, _pool=self)
                retries.sleep(answered)
                url = location
                continue
            if retries.is_retry(
                method,
                answered.status,
                has_retry_after=bool(answered.headers.get("Retry-After")),
            ):
                retries = retries.increment(method, url, response=answered, _pool=self)
                retries.sleep(answered)
                continue
            return answered

    def _make_request(
        self,
        method,
        target,
        body,
        headers,
        url,
        preload_content,
        decode_content,
        retries,
    ):
        """One request on one connection, and the answer read off it."""
        self.num_requests += 1
        connection = self._new_conn()
        sent = dict(headers.items()) if hasattr(headers, "items") else dict(headers or {})
        sent = dict(
            (name, value)
            for name, value in sent.items()
            if value != "@@@SKIP_HEADER@@@"
        )
        try:
            connection.request(method.upper(), target or "/", body=body, headers=sent)
            answered = connection.getresponse()
            carried = answered.read()
        except socket.gaierror as error:
            raise NewConnectionError(self, str(error)) from error
        except TimeoutError as error:
            raise ReadTimeoutError(self, url, "Read timed out.") from error
        except OSError as error:
            raise ProtocolError("Connection aborted.", error) from error
        return HTTPResponse(
            body=carried,
            headers=answered.getheaders(),
            status=answered.status,
            version=getattr(answered, "version", 11),
            reason=answered.reason,
            preload_content=preload_content,
            decode_content=decode_content,
            original_response=answered,
            pool=self,
            connection=connection,
            retries=retries,
            request_method=method.upper(),
            request_url=url,
        )


class HTTPSConnectionPool(HTTPConnectionPool):
    """The pool one host's `https` requests are made through."""

    scheme = "https"
    ConnectionCls = HTTPSConnection

    def __init__(
        self,
        host,
        port=None,
        timeout=_DEFAULT,
        maxsize=1,
        block=False,
        headers=None,
        retries=None,
        _proxy=None,
        _proxy_headers=None,
        key_file=None,
        cert_file=None,
        cert_reqs=None,
        ca_certs=None,
        ssl_version=None,
        assert_hostname=None,
        assert_fingerprint=None,
        ca_cert_dir=None,
        **conn_kw,
    ):
        HTTPConnectionPool.__init__(
            self,
            host,
            port=port,
            timeout=timeout,
            maxsize=maxsize,
            block=block,
            headers=headers,
            retries=retries,
            _proxy=_proxy,
            _proxy_headers=_proxy_headers,
            **conn_kw,
        )
        # A request of this runtime reaches no socket, so there is no TLS to negotiate and
        # nothing to verify: what a request carries instead is the host's own signature.
        self.key_file = key_file
        self.cert_file = cert_file
        self.cert_reqs = cert_reqs
        self.ca_certs = ca_certs
        self.ca_cert_dir = ca_cert_dir
        self.ssl_version = ssl_version
        self.assert_hostname = assert_hostname
        self.assert_fingerprint = assert_fingerprint


def connection_from_url(url, **kw):
    """A pool for the host `url` names."""
    parsed = parse_url(url)
    scheme = parsed.scheme or "http"
    port = parsed.port or port_by_scheme.get(scheme, 80)
    if scheme == "https":
        return HTTPSConnectionPool(parsed.host, port=port, **kw)
    return HTTPConnectionPool(parsed.host, port=port, **kw)
"#;

/// `urllib3.poolmanager`: the pools a caller makes every request through.
const POOLMANAGER_SOURCE: &str = r#"
"""One pool per host, and the request a caller makes without naming either."""

from urllib.parse import urljoin

import collections
import functools

from urllib3._request_methods import RequestMethods
from urllib3.connectionpool import HTTPConnectionPool, HTTPSConnectionPool, port_by_scheme
from urllib3.exceptions import (
    LocationValueError,
    MaxRetryError,
    ProxyError,
    URLSchemeUnknown,
)
from urllib3.response import HTTPHeaderDict
from urllib3.util.retry import Retry
from urllib3.util.url import parse_url

__all__ = ["PoolManager", "ProxyManager", "proxy_from_url"]

pool_classes_by_scheme = {"http": HTTPConnectionPool, "https": HTTPSConnectionPool}

# The settings that are only a TLS connection's, which a plain `http` pool is never given.
SSL_KEYWORDS = (
    "key_file",
    "cert_file",
    "cert_reqs",
    "ca_certs",
    "ca_cert_data",
    "ssl_version",
    "ssl_minimum_version",
    "ssl_maximum_version",
    "ca_cert_dir",
    "ssl_context",
    "key_password",
    "server_hostname",
)

# WHICH pool a request is made through is decided by these fields of it: two requests whose
# every field matches share a pool, and one that differs in any of them gets its own. A caller
# that adds a setting of its own adds a field here, which is why the key is a type.
_key_fields = (
    "key_scheme",
    "key_host",
    "key_port",
    "key_timeout",
    "key_retries",
    "key_block",
    "key_source_address",
    "key_key_file",
    "key_key_password",
    "key_cert_file",
    "key_cert_reqs",
    "key_ca_certs",
    "key_ca_cert_data",
    "key_ssl_version",
    "key_ssl_minimum_version",
    "key_ssl_maximum_version",
    "key_ca_cert_dir",
    "key_ssl_context",
    "key_maxsize",
    "key_headers",
    "key__proxy",
    "key__proxy_headers",
    "key__proxy_config",
    "key_socket_options",
    "key__socks_options",
    "key_assert_hostname",
    "key_assert_fingerprint",
    "key_server_hostname",
    "key_blocksize",
)

PoolKey = collections.namedtuple("PoolKey", _key_fields)


def _default_key_normalizer(key_class, request_context):
    """The pool key `request_context` states, as `key_class` holds one.

    Every value of the key has to be one that can be HELD in a mapping, so the mappings and
    the lists among them are frozen first; a field the context does not state is empty.
    """
    context = dict(request_context)
    if context.get("scheme") is not None:
        context["scheme"] = context["scheme"].lower()
    if context.get("host") is not None:
        context["host"] = context["host"].lower()
    for key in ("headers", "_proxy_headers", "_socks_options"):
        if key in context and context[key] is not None:
            context[key] = frozenset(dict(context[key]).items())
    socket_options = context.get("socket_options")
    if socket_options is not None:
        context["socket_options"] = tuple(socket_options)
    for key in list(context.keys()):
        context["key_" + key] = context.pop(key)
    for field in key_class._fields:
        if field not in context:
            context[field] = None
    if context.get("key_block") is None:
        context["key_block"] = False
    return key_class(**context)


key_fn_by_scheme = {
    "http": functools.partial(_default_key_normalizer, PoolKey),
    "https": functools.partial(_default_key_normalizer, PoolKey),
}


class PoolManager(RequestMethods):
    """The pools every request of one caller is made through, one per host addressed."""

    proxy = None
    proxy_config = None

    def __init__(self, num_pools=10, headers=None, **connection_pool_kw):
        RequestMethods.__init__(self, headers)
        self.num_pools = num_pools
        self.connection_pool_kw = connection_pool_kw
        self.pools = {}
        self.key_fn_by_scheme = key_fn_by_scheme.copy()

    def __enter__(self):
        return self

    def __exit__(self, *_arguments):
        self.clear()
        return False

    def clear(self):
        self.pools = {}

    def _new_pool(self, scheme, host, port, request_context=None):
        pool_class = pool_classes_by_scheme[scheme]
        settings = dict(self.connection_pool_kw)
        if request_context:
            settings = dict(request_context)
        for named in ("scheme", "host", "port"):
            settings.pop(named, None)
        if scheme == "http":
            # A TLS setting is no setting of a plain connection, so it is not handed to one.
            for named in SSL_KEYWORDS:
                settings.pop(named, None)
        return pool_class(host, port=port, **settings)

    def connection_from_host(self, host, port=None, scheme="http", pool_kwargs=None):
        """The pool for one host, made on the first request addressed to it."""
        if not host:
            raise LocationValueError("No host specified.")
        request_context = self._merge_pool_kwargs(pool_kwargs)
        request_context["scheme"] = (scheme or "http").lower()
        if request_context["scheme"] not in pool_classes_by_scheme:
            raise URLSchemeUnknown(request_context["scheme"])
        request_context["port"] = port or port_by_scheme.get(
            request_context["scheme"], 80
        )
        request_context["host"] = host
        return self.connection_from_context(request_context)

    def connection_from_context(self, request_context):
        """The pool the settings `request_context` states is served by."""
        scheme = request_context.get("scheme")
        pool_key_constructor = self.key_fn_by_scheme.get(scheme)
        if not pool_key_constructor:
            raise URLSchemeUnknown(scheme)
        pool_key = pool_key_constructor(request_context)
        return self.connection_from_pool_key(pool_key, request_context=request_context)

    def connection_from_pool_key(self, pool_key, request_context=None):
        """The pool held under `pool_key`, made on the first request that needs it."""
        held = self.pools.get(pool_key)
        if held is not None:
            return held
        context = request_context or {}
        held = self._new_pool(
            context.get("scheme"),
            context.get("host"),
            context.get("port"),
            request_context=request_context,
        )
        self.pools[pool_key] = held
        return held

    def _merge_pool_kwargs(self, override):
        """This manager's own settings with `override` applied: a value stated there wins,
        and one stated as nothing takes the setting away."""
        merged = dict(self.connection_pool_kw)
        if override:
            for key, value in override.items():
                if value is None:
                    merged.pop(key, None)
                else:
                    merged[key] = value
        return merged

    def connection_from_url(self, url, pool_kwargs=None):
        parsed = parse_url(url)
        return self.connection_from_host(
            parsed.host,
            port=parsed.port,
            scheme=parsed.scheme or "http",
            pool_kwargs=pool_kwargs,
        )

    def urlopen(self, method, url, redirect=True, **kw):
        """Make one request against whichever host `url` names, following a redirect it
        answers — including one to another host, which is what a pool of its own cannot do."""
        parsed = parse_url(url)
        scheme = (parsed.scheme or "http").lower()
        if scheme not in pool_classes_by_scheme:
            raise URLSchemeUnknown(scheme)
        pool = self.connection_from_host(parsed.host, port=parsed.port, scheme=scheme)
        kw["assert_same_host"] = False
        kw["redirect"] = False
        if "headers" not in kw or kw["headers"] is None:
            kw["headers"] = self.headers
        answered = pool.urlopen(method, parsed.request_uri, **kw)
        location = redirect and answered.get_redirect_location()
        if not location:
            return answered
        redirected = urljoin(url, location)
        if answered.status == 303 and method.upper() != "HEAD":
            method = "GET"
            kw["body"] = None
            kw["headers"] = HTTPHeaderDict(kw["headers"])._prepare_for_method_change()
        retries = kw.get("retries")
        if not isinstance(retries, Retry):
            retries = Retry.from_int(retries, redirect=redirect)
        if retries.remove_headers_on_redirect and parse_url(redirected).host != parsed.host:
            kept = HTTPHeaderDict(kw["headers"])
            for header in retries.remove_headers_on_redirect:
                kept.discard(header)
            kw["headers"] = kept
        try:
            retries = retries.increment(method, url, response=answered, _pool=pool)
        except MaxRetryError:
            if retries.raise_on_redirect:
                raise
            return answered
        kw["retries"] = retries
        kw["redirect"] = redirect
        retries.sleep_for_retry(answered)
        return self.urlopen(method, redirected, **kw)


class ProxyManager(PoolManager):
    """The pools every request of one caller is made through a PROXY with.

    A caller that configures a proxy is given one of these, as the published library gives
    one, and the proxy it names is the one it states (`proxy`, `proxy_headers`). What this
    runtime cannot do is REACH one: a request of a handler is carried into this same emulator
    rather than onto a network, so there is nothing that forwards a request on a proxy's
    behalf. A request made through this manager is therefore reported as one that was not
    made, naming the proxy, rather than sent directly to the destination — which would be a
    request the caller did not ask for.
    """

    def __init__(
        self,
        proxy_url,
        num_pools=10,
        headers=None,
        proxy_headers=None,
        proxy_ssl_context=None,
        use_forwarding_for_https=False,
        proxy_assert_hostname=None,
        proxy_assert_fingerprint=None,
        **connection_pool_kw
    ):
        if isinstance(proxy_url, HTTPConnectionPool):
            scheme = "https" if isinstance(proxy_url, HTTPSConnectionPool) else "http"
            proxy_url = "%s://%s:%i" % (scheme, proxy_url.host, proxy_url.port)
        proxy = parse_url(proxy_url)
        if proxy.scheme not in ("http", "https"):
            raise ProxyError(
                "Unable to determine proxy scheme from %s" % proxy_url, None
            )
        if not proxy.port:
            port = port_by_scheme.get(proxy.scheme, 80)
            proxy = proxy._replace(port=port)
        self.proxy = proxy
        self.proxy_headers = proxy_headers or {}
        self.proxy_ssl_context = proxy_ssl_context
        self.proxy_config = None
        PoolManager.__init__(self, num_pools, headers, **connection_pool_kw)

    def connection_from_host(self, host, port=None, scheme="http", pool_kwargs=None):
        return PoolManager.connection_from_host(
            self, host, port, scheme, pool_kwargs=pool_kwargs
        )

    def _set_proxy_headers(self, url, headers=None):
        """The headers a request through a proxy carries beside its own."""
        headers_ = {"Accept": "*/*"}
        host = parse_url(url).netloc
        if host:
            headers_["Host"] = host
        if headers:
            headers_.update(headers)
        return headers_

    def urlopen(self, method, url, redirect=True, **kw):
        raise ProxyError(
            "this runtime reaches no proxy, so the request to %s was not made through %s"
            % (url, self.proxy.url),
            None,
        )


def proxy_from_url(url, **kw):
    """The pools a caller makes every request through the proxy `url` names with."""
    return ProxyManager(proxy_url=url, **kw)
"#;

/// `urllib3.fields`: one field of a form a request carries.
const FIELDS_SOURCE: &str = r#"
"""One field of a multipart form, as this library carries one."""


def format_header_param(name, value):
    """One parameter of a header, quoted as a header carries it."""
    quoted = str(value).replace('"', '\\"')
    return '%s="%s"' % (name, quoted)


class RequestField:
    """One field of a form: its name, its data and the headers describing it."""

    def __init__(self, name, data, filename=None, headers=None, header_formatter=None):
        self._name = name
        self._filename = filename
        self.data = data
        self.headers = dict(headers) if headers else {}
        self.header_formatter = header_formatter or format_header_param

    @classmethod
    def from_tuples(cls, fieldname, value, header_formatter=None):
        """One field out of the pair a caller passed: a bare value, or the
        `(filename, data[, content type])` tuple a file field is passed as."""
        filename = None
        content_type = None
        if isinstance(value, tuple):
            if len(value) == 3:
                filename, data, content_type = value
            else:
                filename, data = value
                content_type = "application/octet-stream"
        else:
            data = value
        field = cls(fieldname, data, filename=filename, header_formatter=header_formatter)
        field.make_multipart(content_type=content_type)
        return field

    @property
    def name(self):
        return self._name

    @property
    def filename(self):
        return self._filename

    def render_headers(self):
        """The header block this field is sent under."""
        lines = []
        for name in ("Content-Disposition", "Content-Type", "Content-Location"):
            value = self.headers.get(name)
            if value:
                lines.append("%s: %s" % (name, value))
        for name, value in self.headers.items():
            if name not in (
                "Content-Disposition",
                "Content-Type",
                "Content-Location",
            ) and value:
                lines.append("%s: %s" % (name, value))
        lines.append("\r\n")
        return "\r\n".join(lines)

    def make_multipart(self, content_disposition=None, content_type=None, content_location=None):
        """Describe this field as a part of a multipart form."""
        disposition = (content_disposition or "form-data") + "; "
        parameters = [self.header_formatter("name", self._name)]
        if self._filename is not None:
            parameters.append(self.header_formatter("filename", self._filename))
        self.headers["Content-Disposition"] = disposition + "; ".join(parameters)
        self.headers["Content-Type"] = content_type
        self.headers["Content-Location"] = content_location
"#;

/// `urllib3.filepost`: the body a form posted through this client travels as.
const FILEPOST_SOURCE: &str = r#"
"""Encoding the fields of a form as the body one request carries."""

import binascii
import os

from urllib3.fields import RequestField


def choose_boundary():
    """A boundary no part of the body can carry."""
    return binascii.hexlify(os.urandom(16)).decode("ascii")


def iter_field_objects(fields):
    """Each field of `fields` as a `RequestField`."""
    if hasattr(fields, "items"):
        fields = list(fields.items())
    for field in fields:
        if isinstance(field, RequestField):
            yield field
        else:
            name, value = field
            yield RequestField.from_tuples(name, value)


def encode_multipart_formdata(fields, boundary=None):
    """`fields` as the body and the content type of one multipart form post."""
    body = []
    if boundary is None:
        boundary = choose_boundary()
    for field in iter_field_objects(fields):
        body.append(("--%s\r\n" % (boundary,)).encode("utf-8"))
        body.append(field.render_headers().encode("utf-8"))
        data = field.data
        if isinstance(data, int):
            data = str(data)
        if isinstance(data, str):
            data = data.encode("utf-8")
        body.append(data)
        body.append(b"\r\n")
    body.append(("--%s--\r\n" % (boundary,)).encode("utf-8"))
    return b"".join(body), "multipart/form-data; boundary=%s" % (boundary,)
"#;

/// `urllib3._request_methods`: the `request` every pool and pool manager answers.
const REQUEST_METHODS_SOURCE: &str = r#"
"""The request methods a pool and a pool manager share."""

import json as _json
from urllib.parse import urlencode

from urllib3.filepost import encode_multipart_formdata


class RequestMethods:
    """`request`, and the two encodings a form of fields travels in.

    The request itself is made by `urlopen`, which the pool and the pool manager each
    implement: this is the interface above it, exactly as the library layers them.
    """

    # The methods whose fields travel in the URL rather than in a body.
    _encode_url_methods = frozenset(["DELETE", "GET", "HEAD", "OPTIONS"])

    def __init__(self, headers=None):
        self.headers = headers or {}

    def urlopen(
        self,
        method,
        url,
        body=None,
        headers=None,
        encode_multipart=True,
        multipart_boundary=None,
        **kw,
    ):
        raise NotImplementedError(
            "Classes extending RequestMethods must implement their own ``urlopen`` method."
        )

    def request(
        self,
        method,
        url,
        body=None,
        fields=None,
        headers=None,
        json=None,
        **urlopen_kw,
    ):
        """One request: its fields encoded the way this method carries them."""
        method = method.upper()
        if json is not None and body is not None:
            raise TypeError(
                "request got values for both 'body' and 'json' parameters which are mutually "
                "exclusive"
            )
        if json is not None:
            if headers is None:
                headers = dict(self.headers.items()) if self.headers else {}
            if not any(name.lower() == "content-type" for name in headers):
                headers["Content-Type"] = "application/json"
            body = _json.dumps(json, separators=(",", ":"), ensure_ascii=False).encode(
                "utf-8"
            )
        if body is not None:
            urlopen_kw["body"] = body
        if fields:
            if method in self._encode_url_methods:
                return self.request_encode_url(
                    method, url, fields=fields, headers=headers, **urlopen_kw
                )
            return self.request_encode_body(
                method, url, fields=fields, headers=headers, **urlopen_kw
            )
        return self.urlopen(method, url, headers=headers, **urlopen_kw)

    def request_encode_url(self, method, url, fields=None, headers=None, **urlopen_kw):
        """A request whose fields travel in the URL's own query."""
        if headers is None:
            headers = self.headers
        if fields:
            url = url + "?" + urlencode(fields)
        return self.urlopen(method, url, headers=headers, **urlopen_kw)

    def request_encode_body(
        self,
        method,
        url,
        fields=None,
        headers=None,
        encode_multipart=True,
        multipart_boundary=None,
        **urlopen_kw,
    ):
        """A request whose fields travel in its body, as a form."""
        if headers is None:
            headers = self.headers
        extra = {}
        if fields:
            if "body" in urlopen_kw:
                raise TypeError(
                    "request got values for both 'fields' and 'body', can only specify one."
                )
            if encode_multipart:
                body, content_type = encode_multipart_formdata(
                    fields, boundary=multipart_boundary
                )
            else:
                body = urlencode(fields)
                content_type = "application/x-www-form-urlencoded"
            extra["body"] = body
            sent = dict(headers.items()) if hasattr(headers, "items") else dict(headers)
            sent.setdefault("Content-Type", content_type)
            headers = sent
        extra.update(urlopen_kw)
        return self.urlopen(method, url, headers=headers, **extra)
"#;

/// `urllib3.request`: the module the earlier line of the library kept the request methods in.
const REQUEST_SOURCE: &str = r#"
"""The request methods, under the module name the library's earlier line kept them at."""

from urllib3._request_methods import RequestMethods

__all__ = ["RequestMethods"]
"#;
