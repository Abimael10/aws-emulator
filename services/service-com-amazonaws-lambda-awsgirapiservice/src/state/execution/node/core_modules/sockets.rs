// SPDX-License-Identifier: CC0-1.0

//! `net` and `tls`: the stream-socket interfaces of this runtime.
//!
//! A managed `nodejs*` runtime is a promise about an interpreter AND about the standard library
//! that comes with it, so a handler — or a bundle it ships — whose module body says
//! `require('net')` or `require('tls')` loads on the runtime it was written for, and so it has
//! to here. A module of that standard library this emulator registers no capability for is
//! SUPPLIED on the resolver path instead of being absent from it: what is refused is the CALL
//! that needs the capability, never the import, because an import that fails points the
//! developer at a deployment package that is not at fault. The same decision stands behind this
//! crate's `child_process` ([`super::child_process`]).
//!
//! What is this module's own, and what it refuses:
//!
//! * the NAMES a program reads off `net` and `tls` are all here, so a module that imports one
//!   of them to call it later, or to name it at all, imports and runs;
//! * the pieces that are COMPUTATION are performed rather than refused: which form of address
//!   a string is, the identity check a certificate's names are matched by, and the arguments of
//!   each entry point as node reads them;
//! * HOLDING a socket is not. The JavaScript engine an invocation runs in reaches the host
//!   through the host functions the runtime installs and through nothing else ([`super::super`]
//!   module docs), and none of them is a socket, so a call that would open or accept a
//!   connection reports the failure a host answers for a system call that is not there —
//!   `ENOSYS` — through that entry point's own channel: thrown where node throws, and emitted
//!   on the socket or the server where node reports it there.
//!
//! What a handler CAN reach of the network is the one request this runtime sends for it, which
//! is the `http`/`https` client ([`super::http`]) and the HTTP server of this run those two
//! modules also hold; a name a handler resolves is answered by the resolver of the execution
//! environment ([`super::dns`]). Nothing here invents a connection: no socket is reported as
//! open, no byte as sent and no peer certificate as seen.

/// `net`: the address readings, and the entry points that would hold a socket.
pub(super) const NET_SOURCE: &str = r##"
'use strict';

var EventEmitter = require('events');

// The number a system call that is not there answers with, which is what a connection nothing
// carried reports. node carries it negated in `errno`, as the result it came from.
var ENOSYS = 38;

function inherit(child, parent) {
    child.prototype = Object.create(parent.prototype);
    child.prototype.constructor = child;
}

// The failure of a call that would have held a socket, as node builds one: the message is the
// syscall that failed and the code it failed with, and the properties beside it are what a
// caller branches on.
function notOpened(syscall, address, port) {
    var error = new Error(syscall + ' ENOSYS'
        + (address === undefined ? '' : ' ' + address + ':' + String(port)));
    error.errno = -ENOSYS;
    error.code = 'ENOSYS';
    error.syscall = syscall;
    if (address !== undefined) {
        error.address = address;
        error.port = port;
    }
    return error;
}

// "Tests if input is an IPv4 address": four decimal octets, each inside one byte and written
// without a leading zero, which is the form a dotted-quad address has.
function isIPv4(input) {
    var parts = String(input).split('.');
    if (parts.length !== 4) {
        return false;
    }
    for (var index = 0; index < 4; index += 1) {
        var part = parts[index];
        if (!/^\d{1,3}$/.test(part) || Number(part) > 255) {
            return false;
        }
        if (part.length > 1 && part.charAt(0) === '0') {
            return false;
        }
    }
    return true;
}

// "Tests if input is an IPv6 address": up to eight groups of hexadecimal digits, with at most
// one run of omitted zero groups, and a trailing IPv4 form standing for the last two groups.
function isIPv6(input) {
    var text = String(input);
    if (text.indexOf(':') < 0) {
        return false;
    }
    var halves = text.split('::');
    if (halves.length > 2) {
        return false;
    }
    var groups = 0;
    for (var half = 0; half < halves.length; half += 1) {
        if (halves[half] === '') {
            continue;
        }
        var parts = halves[half].split(':');
        for (var index = 0; index < parts.length; index += 1) {
            var part = parts[index];
            if (part === '') {
                return false;
            }
            if (index === parts.length - 1 && half === halves.length - 1 && isIPv4(part)) {
                groups += 2;
                continue;
            }
            if (!/^[0-9a-fA-F]{1,4}$/.test(part)) {
                return false;
            }
            groups += 1;
        }
    }
    return halves.length === 2 ? groups < 8 : groups === 8;
}

function isIP(input) {
    if (isIPv4(input)) {
        return 4;
    }
    return isIPv6(input) ? 6 : 0;
}

// The arguments of a connect, read the way node reads them: a port and an optional host, or an
// options object, with an optional connect listener behind either.
function connectArguments(first, second, third) {
    var options = {};
    var listener = null;
    if (first !== null && typeof first === 'object') {
        options = first;
        listener = typeof second === 'function' ? second : null;
    } else {
        options = { port: first };
        if (typeof second === 'string') {
            options.host = second;
            listener = typeof third === 'function' ? third : null;
        } else {
            listener = typeof second === 'function' ? second : null;
        }
    }
    return { options: options, listener: listener };
}

// "This class is an abstraction of a TCP socket": the object a program holds a connection by.
// Nothing is connected, so every reading of a peer is absent and no byte is reported written.
function Socket() {
    EventEmitter.call(this);
    this.connecting = false;
    this.pending = true;
    this.destroyed = false;
    this.readyState = 'closed';
    this.bytesRead = 0;
    this.bytesWritten = 0;
    this.remoteAddress = undefined;
    this.remotePort = undefined;
    this.remoteFamily = undefined;
    this.localAddress = undefined;
    this.localPort = undefined;
    this.timeout = undefined;
}

inherit(Socket, EventEmitter);

// The connection this runtime cannot open, reported on the socket the program was handed —
// deferred, because a program attaches its `connect` and `error` handlers to that socket after
// the call that asked for it.
Socket.prototype.connect = function (first, second, third) {
    var taken = connectArguments(first, second, third);
    var self = this;
    if (taken.listener !== null) {
        this.once('connect', taken.listener);
    }
    var failure = notOpened(
        'connect',
        taken.options.host === undefined ? 'localhost' : String(taken.options.host),
        taken.options.port === undefined ? 0 : taken.options.port
    );
    Promise.resolve().then(function () {
        self.destroyed = true;
        self.emit('error', failure);
        self.emit('close', true);
    });
    return this;
};

Socket.prototype.write = function (data, encoding, callback) {
    var done = typeof encoding === 'function' ? encoding : callback;
    var failure = notOpened('write');
    if (typeof done === 'function') {
        Promise.resolve().then(function () { done(failure); });
    }
    return false;
};

Socket.prototype.end = function () {
    this.destroyed = true;
    var self = this;
    Promise.resolve().then(function () { self.emit('close', false); });
    return this;
};

Socket.prototype.destroy = function (error) {
    this.destroyed = true;
    var self = this;
    Promise.resolve().then(function () {
        if (error !== undefined && error !== null) {
            self.emit('error', error);
        }
        self.emit('close', error !== undefined && error !== null);
    });
    return this;
};

Socket.prototype.setTimeout = function (milliseconds, listener) {
    this.timeout = milliseconds;
    if (typeof listener === 'function') {
        this.once('timeout', listener);
    }
    return this;
};

Socket.prototype.setNoDelay = function () { return this; };
Socket.prototype.setKeepAlive = function () { return this; };
Socket.prototype.ref = function () { return this; };
Socket.prototype.unref = function () { return this; };
Socket.prototype.address = function () { return {}; };
Socket.prototype.pause = function () { return this; };
Socket.prototype.resume = function () { return this; };

// "This class is used to create a TCP … server": nothing can be accepted, so a listen is the
// failure a bind that is not there answers with, reported on the server the program holds.
function Server(options, listener) {
    EventEmitter.call(this);
    this.listening = false;
    this.maxConnections = undefined;
    this.connections = 0;
    if (typeof options === 'function') {
        this.on('connection', options);
    } else if (typeof listener === 'function') {
        this.on('connection', listener);
    }
}

inherit(Server, EventEmitter);

Server.prototype.listen = function () {
    var self = this;
    var args = Array.prototype.slice.call(arguments);
    var listener = args.length > 0 && typeof args[args.length - 1] === 'function'
        ? args[args.length - 1]
        : null;
    if (listener !== null) {
        this.once('listening', listener);
    }
    var failure = notOpened('listen');
    Promise.resolve().then(function () {
        self.emit('error', failure);
    });
    return this;
};

Server.prototype.close = function (callback) {
    this.listening = false;
    var self = this;
    Promise.resolve().then(function () {
        if (typeof callback === 'function') {
            callback(null);
        }
        self.emit('close');
    });
    return this;
};

Server.prototype.address = function () { return null; };
Server.prototype.getConnections = function (callback) {
    if (typeof callback === 'function') {
        Promise.resolve().then(function () { callback(null, 0); });
    }
    return this;
};
Server.prototype.ref = function () { return this; };
Server.prototype.unref = function () { return this; };

function connect(first, second, third) {
    var socket = new Socket();
    socket.connect(first, second, third);
    return socket;
}

module.exports = {
    Socket: Socket,
    Stream: Socket,
    Server: Server,
    connect: connect,
    createConnection: connect,
    createServer: function (options, listener) { return new Server(options, listener); },
    isIP: isIP,
    isIPv4: isIPv4,
    isIPv6: isIPv6
};
"##;

/// `tls`: the certificate reading, and the entry points that would hold a secure socket.
pub(super) const TLS_SOURCE: &str = r##"
'use strict';

var net = require('net');

// "The minimum TLS version … to allow": node's own default, which is what a program reads to
// decide whether it has to raise it.
var DEFAULT_MIN_VERSION = 'TLSv1.2';
var DEFAULT_MAX_VERSION = 'TLSv1.3';

function inherit(child, parent) {
    child.prototype = Object.create(parent.prototype);
    child.prototype.constructor = child;
}

// A secure socket is a socket, and this runtime holds none ([`NET_SOURCE`]): the call that
// would open one is refused by the socket underneath it, and nothing here reports a handshake,
// a cipher or a peer certificate that no connection produced.
function TLSSocket(socket, options) {
    net.Socket.call(this, options);
    this.authorized = false;
    this.authorizationError = undefined;
    this.encrypted = true;
    this.alpnProtocol = false;
}

inherit(TLSSocket, net.Socket);

TLSSocket.prototype.getPeerCertificate = function () {
    // "If the peer does not provide a certificate … an empty object will be returned": no peer
    // was reached, so there is no certificate to report.
    return {};
};

TLSSocket.prototype.getCertificate = function () { return {}; };
TLSSocket.prototype.getCipher = function () { return {}; };
TLSSocket.prototype.getProtocol = function () { return null; };
TLSSocket.prototype.getSession = function () { return undefined; };
TLSSocket.prototype.isSessionReused = function () { return false; };
TLSSocket.prototype.renegotiate = function (options, callback) {
    if (typeof callback === 'function') {
        var self = this;
        Promise.resolve().then(function () {
            callback(new Error('renegotiate ENOSYS'));
        });
        return false;
    }
    return false;
};

// "Verifies the certificate cert is issued to hostname": the name matching RFC 6125 states —
// the subject alternative names first, the common name only where there are none, and one
// wildcard label matching exactly one label of the name. It is COMPUTATION over the
// certificate a caller passed, so it is performed here rather than refused.
function checkServerIdentity(hostname, cert) {
    var named = String(hostname).toLowerCase();
    var names = [];
    if (cert !== null && cert !== undefined && typeof cert.subjectaltname === 'string') {
        var parts = cert.subjectaltname.split(',');
        for (var index = 0; index < parts.length; index += 1) {
            var part = parts[index].trim();
            if (part.indexOf('DNS:') === 0) {
                names.push(part.slice(4).toLowerCase());
            }
        }
    }
    if (names.length === 0 && cert !== null && cert !== undefined && cert.subject
        && typeof cert.subject.CN === 'string') {
        names.push(cert.subject.CN.toLowerCase());
    }
    for (var at = 0; at < names.length; at += 1) {
        if (matchesName(named, names[at])) {
            return undefined;
        }
    }
    var error = new Error("Hostname/IP does not match certificate's altnames: Host: "
        + named + '. is not in the cert\'s altnames');
    error.code = 'ERR_TLS_CERT_ALTNAME_INVALID';
    error.host = named;
    error.cert = cert;
    return error;
}

function matchesName(hostname, pattern) {
    if (pattern === hostname) {
        return true;
    }
    if (pattern.indexOf('*.') !== 0) {
        return false;
    }
    var cut = hostname.indexOf('.');
    // One wildcard label stands for exactly one label of the name, and for no label at all it
    // stands for nothing.
    return cut > 0 && hostname.slice(cut + 1) === pattern.slice(2);
}

function connect(first, second, third) {
    var socket = new TLSSocket();
    net.Socket.prototype.connect.call(socket, first, second, third);
    return socket;
}

function Server(options, listener) {
    net.Server.call(this, options, listener);
}

inherit(Server, net.Server);

Server.prototype.addContext = function () { return undefined; };
Server.prototype.setSecureContext = function () { return undefined; };
Server.prototype.getTicketKeys = function () { return require('buffer').Buffer.alloc(0); };

module.exports = {
    TLSSocket: TLSSocket,
    Server: Server,
    connect: connect,
    createServer: function (options, listener) { return new Server(options, listener); },
    checkServerIdentity: checkServerIdentity,
    // "Returns an object containing commonly used constants for TLS": a secure context is the
    // options it was built from, which is all a program reads back off one here.
    createSecureContext: function (options) {
        return { context: options === undefined ? {} : options };
    },
    getCiphers: function () { return []; },
    // "An immutable array of strings representing the root certificates": this execution
    // environment reaches no peer to verify, so it carries no trust store of its own.
    rootCertificates: Object.freeze([]),
    DEFAULT_MIN_VERSION: DEFAULT_MIN_VERSION,
    DEFAULT_MAX_VERSION: DEFAULT_MAX_VERSION,
    DEFAULT_ECDH_CURVE: 'auto',
    CLIENT_RENEG_LIMIT: 3,
    CLIENT_RENEG_WINDOW: 600
};
"##;
