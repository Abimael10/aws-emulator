// SPDX-License-Identifier: CC0-1.0

//! `http` and `https`: the HTTP CLIENT a node program sends a request of its own with,
//! over the one host seam that carries a guest's request into this same emulator
//! ([`crate::state::execution::guest_http`]), and the SERVER a node program answers its own
//! requests with.
//!
//! A managed node runtime supplies these two modules, and a handler that answers a
//! callback URL, reads a resource's own endpoint or signs a data-plane request of its own
//! builds that request with them — `https.request(options, callback)`, then `write` and
//! `end`, and the answer arrives as an `IncomingMessage`. This is that client: the request
//! the guest describes is handed to the host as a method, a URL, its headers and its body,
//! and what comes back is the emulated service's own status, headers and body.
//!
//! What the guest can and cannot do here is the HOST's decision and not this source's: the
//! addresses a URL may name, the signature the request carries, the sizes it may carry and
//! how many requests one run may make are all
//! [`crate::state::execution::guest_http`]'s, reached through the one function the runtime
//! exposes for it ([`super::super::runtime`]'s `httpSend`). No socket is opened on any
//! path, and none is needed for the SERVER half: a server a handler listens with is
//! reachable at the LOOPBACK address it bound, which on a managed runtime is an address of
//! the execution environment alone — nothing outside one reaches it — so a request this
//! run addresses at a port a server of this run is listening on is carried to that server
//! in this process, and every other address is the host's to decide on. A port is held
//! while a server listens on it, and the two modules share ONE port space, as one
//! environment has.
//!
//! Nothing about an answer is synthesised. The status, the headers and the bytes a handler
//! reads back are the ones the emulated service answered — or, for a request this run
//! served itself, the ones its own server wrote. A request the host did not make is
//! reported as the failure a node program reads off a request that was not made — never as
//! an answer nothing sent.
//!
//! A request this run serves itself is bounded by the run's own bounds and by nothing else
//! it could ask for: it is guest computation, so the engine's step ceiling, the run's
//! deadline and its allocation ceiling ([`super::super::Bounds`]) are what end a handler
//! that would serve itself without stopping, exactly as they end any other loop it writes.
//! The ports one run holds are its own and are bounded by the range they come from.


/// The client, as both modules: `http` and `https` differ in the SCHEME a request that
/// states no protocol of its own is sent under, so one source is both and the module's own
/// id ([`super::super::runtime`] names a core module `node:<name>`) says which it is being
/// loaded as.
pub(super) const HTTP_SOURCE: &str = r##"
'use strict';

var EventEmitter = require('events');
var Buffer = require('buffer').Buffer;
var Readable = require('stream').Readable;

// WHICH of the two modules this is: the scheme a request that states none is sent under.
var PROTOCOL = module.id === 'node:https' ? 'https:' : 'http:';

// The port each scheme is spoken on when an address states none, as the schemes define
// them — so an authority carries a port only when it is not its own scheme's.
var DEFAULT_PORT = { 'http:': '80', 'https:': '443' };

// The host a request that names none is addressed to, as node documents the default for
// `options.host`.
var DEFAULT_HOST = 'localhost';

// The hosts that name THIS execution environment: a request addressed at one of them is
// answered by a server of this run listening on the port it names, the way a loopback
// connection is answered by the process that bound the port.
var LOOPBACK = ['localhost', '127.0.0.1', '::1', '[::1]', '0.0.0.0'];

// The ports a server of this run is listening on, keyed by the port as text. ONE port space
// serves both modules — a listening port is reachable whichever scheme addresses it, as one
// environment has one port space — so `http` holds it and `https` reads the same object.
var PORTS = PROTOCOL === 'https:' ? require('http').__ports : {};

// The range a port nobody named is taken from: the ports IANA registers for ephemeral use,
// which is where a `listen(0)` port comes from.
var FIRST_EPHEMERAL = 49152;
var LAST_EPHEMERAL = 65535;

function host() {
    return globalThis.__lambdaRuntime;
}

function inherit(child, parent) {
    child.prototype = Object.create(parent.prototype);
    child.prototype.constructor = child;
}

// ------------------------------------------------------------------ the message

// One HTTP message as a node program reads it: the head is readable as soon as the message
// is handed over, and the body arrives as this stream's own data. It is what a client reads
// an ANSWER through and what a server reads a REQUEST through, which is the one class node
// supplies for both.
function IncomingMessage(socket) {
    Readable.call(this, {});
    this.httpVersion = '1.1';
    this.httpVersionMajor = 1;
    this.httpVersionMinor = 1;
    this.socket = socket === undefined ? null : socket;
    this.connection = this.socket;
    this.statusCode = null;
    this.statusMessage = null;
    this.method = null;
    this.url = '';
    this.req = null;
    this.complete = false;
    this.headers = {};
    this.rawHeaders = [];
}

inherit(IncomingMessage, Readable);

// The headers the message carried, as a node program reads them: every name as it was sent
// under `rawHeaders`, and the folded reading under `headers`.
IncomingMessage.prototype._carried = function (pairs) {
    var self = this;
    var carried = Array.isArray(pairs) ? pairs : [];
    carried.forEach(function (pair) {
        var name = String(pair[0]);
        var value = String(pair[1]);
        var lower = name.toLowerCase();
        self.rawHeaders.push(name, value);
        // A header carried more than once is read as one value, joined — except the one
        // header a node program reads as a list of its own.
        if (lower === 'set-cookie') {
            if (!Array.isArray(self.headers[lower])) {
                self.headers[lower] = [];
            }
            self.headers[lower].push(value);
            return;
        }
        self.headers[lower] = self.headers[lower] === undefined
            ? value
            : self.headers[lower] + ', ' + value;
    });
    return this;
};

// The answer one request came back with, as the message its sender reads it through.
function answerMessage(request, answered) {
    var message = new IncomingMessage();
    message.statusCode = answered.status;
    message.statusMessage = typeof answered.reason === 'string' ? answered.reason : '';
    message.method = request.method;
    message.url = request.path;
    message.req = request;
    return message._carried(answered.headers);
}

// The bytes the answer carried, delivered as this stream's own data — deferred, because a
// node program attaches its reader to the message it has just been handed, and a push
// before that would be a read nothing could have asked for.
IncomingMessage.prototype._deliver = function (bytes) {
    var self = this;
    Promise.resolve().then(function () {
        if (bytes.length > 0) {
            var encoding = self._encoding;
            self.push(typeof encoding === 'string' && encoding !== ''
                ? bytes.toString(encoding)
                : bytes);
        }
        self.complete = true;
        self.push(null);
    });
};


// ------------------------------------------------------------------ the request

function ClientRequest(options, callback) {
    EventEmitter.call(this);
    var held = options === null || options === undefined ? {} : options;
    this.protocol = text(held.protocol) === '' ? PROTOCOL : text(held.protocol);
    this.method = text(held.method) === '' ? 'GET' : text(held.method).toUpperCase();
    this.host = hostnameOf(held);
    this.port = portOf(held);
    this.path = text(held.path) === '' ? '/' : text(held.path);
    this.res = null;
    this.finished = false;
    this.writableEnded = false;
    this.aborted = false;
    this.destroyed = false;
    this._headers = {};
    this._chunks = [];
    this._ended = false;
    this._sent = false;
    this._timer = null;
    var self = this;
    var headers = held.headers;
    if (headers !== null && headers !== undefined && typeof headers === 'object') {
        Object.keys(headers).forEach(function (name) {
            self.setHeader(name, headers[name]);
        });
    }
    if (typeof callback === 'function') {
        this.once('response', callback);
    }
}

inherit(ClientRequest, EventEmitter);

ClientRequest.prototype.setHeader = function (name, value) {
    if (this._sent) {
        var error = new Error('Cannot set headers after they are sent to the client');
        error.code = 'ERR_HTTP_HEADERS_SENT';
        throw error;
    }
    this._headers[String(name).toLowerCase()] = { name: String(name), value: value };
    return this;
};

ClientRequest.prototype.getHeader = function (name) {
    var held = this._headers[String(name).toLowerCase()];
    return held === undefined ? undefined : held.value;
};

ClientRequest.prototype.hasHeader = function (name) {
    return this._headers[String(name).toLowerCase()] !== undefined;
};

ClientRequest.prototype.removeHeader = function (name) {
    delete this._headers[String(name).toLowerCase()];
    return undefined;
};

ClientRequest.prototype.getHeaders = function () {
    var held = this._headers;
    var out = {};
    Object.keys(held).forEach(function (lower) {
        out[lower] = held[lower].value;
    });
    return out;
};

ClientRequest.prototype.getHeaderNames = function () {
    return Object.keys(this._headers);
};

ClientRequest.prototype.write = function (chunk, encoding, callback) {
    var done = typeof encoding === 'function' ? encoding : callback;
    if (this._ended) {
        var error = new Error('write after end');
        error.code = 'ERR_STREAM_WRITE_AFTER_END';
        if (typeof done === 'function') {
            done(error);
        } else {
            raise(this, error);
        }
        return false;
    }
    if (chunk !== null && chunk !== undefined && chunk !== '') {
        this._chunks.push(bytesOf(chunk, typeof encoding === 'string' ? encoding : 'utf8'));
    }
    if (typeof done === 'function') {
        done(null);
    }
    return true;
};

ClientRequest.prototype.end = function (chunk, encoding, callback) {
    var done = typeof chunk === 'function'
        ? chunk
        : (typeof encoding === 'function' ? encoding : callback);
    if (typeof chunk !== 'function' && chunk !== null && chunk !== undefined) {
        this.write(chunk, typeof encoding === 'string' ? encoding : 'utf8');
    }
    if (this._ended) {
        return this;
    }
    this._ended = true;
    this.writableEnded = true;
    if (typeof done === 'function') {
        this.once('finish', done);
    }
    var self = this;
    // The request goes out once the statements AROUND this call have run: a node program
    // attaches its `error` and `response` handlers after `end`, and they are the handlers
    // the answer is reported to.
    Promise.resolve().then(function () {
        send(self);
    });
    return this;
};

ClientRequest.prototype.setTimeout = function (after, callback) {
    var self = this;
    this._clearTimer();
    if (typeof callback === 'function') {
        this.once('timeout', callback);
    }
    this._timer = globalThis.setTimeout(function () {
        self._timer = null;
        self.emit('timeout');
    }, after);
    return this;
};

ClientRequest.prototype.destroy = function (error) {
    this.destroyed = true;
    this._clearTimer();
    if (error !== null && error !== undefined) {
        raise(this, error);
    }
    this.emit('close');
    return this;
};

ClientRequest.prototype.abort = function () {
    this.aborted = true;
    this.destroy();
    return undefined;
};

// The connection-level calls a node program makes on a request. There is no socket to
// make them of — the request is carried by the host into this process — so each is
// accepted and changes nothing about the one request that is sent.
ClientRequest.prototype.flushHeaders = function () { return undefined; };
ClientRequest.prototype.setNoDelay = function () { return undefined; };
ClientRequest.prototype.setSocketKeepAlive = function () { return undefined; };

ClientRequest.prototype._clearTimer = function () {
    if (this._timer !== null) {
        globalThis.clearTimeout(this._timer);
        this._timer = null;
    }
};

// ------------------------------------------------------------------ sending

// Hand ONE request over — to a server of this run listening on the port it addresses, and
// otherwise to the host — and report what came back to the handlers the program attached to
// it.
function send(request) {
    if (request.destroyed || request.aborted || request._sent) {
        return;
    }
    request._sent = true;
    request._clearTimer();
    var body = Buffer.concat(request._chunks);
    var listening = listeningFor(request);
    if (listening !== null) {
        serveLocally(request, listening, body);
        return;
    }
    var answered;
    try {
        answered = JSON.parse(host().httpSend(
            request.method,
            addressOf(request),
            JSON.stringify(headerPairs(request._headers)),
            body.toString('base64')
        ));
    } catch (error) {
        raise(request, error);
        return;
    }
    request.finished = true;
    request.emit('finish');
    answer(request, answered);
}

// Report ONE answer to the handlers the program attached to the request it answers: the
// refusal of a request that was not made, and otherwise the status, the headers and the
// bytes that came back.
function answer(request, answered) {
    if (answered.refused !== undefined && answered.refused !== null) {
        // A LOOPBACK address names this execution environment, whose ports are held by the
        // run's own servers alone: a request there that nothing answered is a connection
        // nothing accepted, which is what a node program reads off one.
        raise(request, isLoopback(request) && answered.refused === 'unknown_service'
            ? notListening(request)
            : notSent(answered, addressOf(request)));
        return;
    }
    var response = answerMessage(request, answered);
    request.res = response;
    request.emit('response', response);
    response._deliver(Buffer.from(text(answered.body), 'base64'));
}

// The server of this run that answers `request`, or `null` for a request addressed anywhere
// but at a port a server of this run is listening on — which is every request the HOST is
// asked to make.
function listeningFor(request) {
    if (!isLoopback(request)) {
        return null;
    }
    var port = text(request.port) === '' ? DEFAULT_PORT[request.protocol] : text(request.port);
    var held = PORTS[port];
    if (held === undefined) {
        return null;
    }
    // A server speaks the scheme of the module it was created with, and a request under the
    // other scheme is no request that server answers.
    return held._protocol === request.protocol ? held : null;
}

// Whether a request is addressed at THIS execution environment, which is what the loopback
// names and addresses name.
function isLoopback(request) {
    return LOOPBACK.indexOf(String(request.host).toLowerCase()) >= 0;
}

// Carry ONE request to the server listening on the port it addresses: the server's own
// listener is handed the request and the response it writes, and what it writes is the
// answer the sender reads. Deferred, because a node program attaches its `response` and
// `error` handlers around the call that sends, and the answer is reported to those handlers.
function serveLocally(request, server, body) {
    request.finished = true;
    request.emit('finish');
    var incoming = new IncomingMessage();
    incoming.method = request.method;
    incoming.url = request.path;
    incoming._carried(serverHeaders(request));
    var response = new ServerResponse(incoming, function (answered) {
        answer(request, answered);
    });
    Promise.resolve().then(function () {
        if (!server.listening) {
            raise(request, notListening(request));
            return;
        }
        incoming._deliver(body);
        server.emit('request', incoming, response);
    });
}

// The headers a server reads off one request: the ones the sender set, and the authority it
// addressed — which every HTTP/1.1 request carries, whether or not the sender wrote it
// itself.
function serverHeaders(request) {
    var pairs = headerPairs(request._headers);
    for (var index = 0; index < pairs.length; index += 1) {
        if (String(pairs[index][0]).toLowerCase() === 'host') {
            return pairs;
        }
    }
    return [['Host', authorityOf(request)]].concat(pairs);
}

// The URL the host is asked to send to: the scheme, the authority — which carries the port
// only when it is not the scheme's own — and the request target.
function addressOf(request) {
    var path = request.path.charAt(0) === '/' ? request.path : '/' + request.path;
    return request.protocol + '//' + authorityOf(request) + path;
}

// The authority a request is addressed at: the host, carrying the port only when it is not
// the scheme's own.
function authorityOf(request) {
    var port = text(request.port);
    return request.host
        + (port === '' || port === DEFAULT_PORT[request.protocol] ? '' : ':' + port);
}

// The headers a message carries, as the name/value pairs they are sent as: a header set to a
// list is carried once per entry, as HTTP carries a repeated header.
function headerPairs(held) {
    var pairs = [];
    Object.keys(held).forEach(function (lower) {
        var name = held[lower].name;
        var value = held[lower].value;
        if (Array.isArray(value)) {
            value.forEach(function (each) {
                pairs.push([name, each === null || each === undefined ? '' : String(each)]);
            });
            return;
        }
        pairs.push([name, value === null || value === undefined ? '' : String(value)]);
    });
    return pairs;
}

// What a request the host did NOT make fails with: the failure a node program reads off a
// request that never reached anything, naming which refusal it was.
function notSent(answered, url) {
    var detail = text(answered.detail);
    if (answered.refused === 'unknown_service') {
        var unreachable = new Error(url + ' addresses no service this emulator serves'
            + (detail === '' ? '' : ': ' + detail));
        unreachable.code = 'ENOTFOUND';
        return unreachable;
    }
    if (answered.refused === 'timed_out') {
        var late = new Error(detail === '' ? 'socket hang up' : detail);
        late.code = 'ETIMEDOUT';
        return late;
    }
    var error = new Error(detail === '' ? 'the request was not made' : detail);
    error.code = 'ECONNREFUSED';
    return error;
}

// What a request addressed at a port NOTHING is listening on any more fails with: the
// failure a node program reads off a connection nothing accepted, naming the address it was
// made to.
function notListening(request) {
    var error = new Error('connect ECONNREFUSED ' + authorityOf(request));
    error.code = 'ECONNREFUSED';
    error.syscall = 'connect';
    error.address = request.host;
    error.port = Number(text(request.port) === ''
        ? DEFAULT_PORT[request.protocol]
        : text(request.port));
    return error;
}

// Report a failure of one request: to the program's own `error` handler, and — where it
// attached none — the way an unhandled `error` ends a node process, since the failure is
// then nothing this run can go on from.
function raise(request, error) {
    request.destroyed = true;
    if (request.listenerCount('error') === 0) {
        Promise.reject(error);
        return;
    }
    request.emit('error', error);
    request.emit('close');
}

// ------------------------------------------------------------------ reading options

function text(value) {
    return value === null || value === undefined ? '' : String(value);
}

function bytesOf(chunk, encoding) {
    return Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk, encoding);
}

// The host a request is addressed to: `hostname` as stated, else the host without the port
// it may carry beside it (`url.parse` reports both), else the documented default.
function hostnameOf(options) {
    var hostname = text(options.hostname);
    if (hostname !== '') {
        return hostname;
    }
    var held = text(options.host);
    if (held === '') {
        return DEFAULT_HOST;
    }
    var colon = held.lastIndexOf(':');
    return colon > 0 && held.indexOf(']') < colon ? held.slice(0, colon) : held;
}

// The port it is addressed to: the one stated, else the one the host carries beside its
// name, else none — and a request stating none is sent on its scheme's own port.
function portOf(options) {
    var port = text(options.port);
    if (port !== '') {
        return port;
    }
    var held = text(options.host);
    var colon = held.lastIndexOf(':');
    return colon > 0 && held.indexOf(']') < colon ? held.slice(colon + 1) : '';
}

// The options one call states, as node reads them: a URL, an options object, or a URL with
// an options object over it, with the callback last.
function stated(args) {
    var list = Array.prototype.slice.call(args);
    var callback = typeof list[list.length - 1] === 'function' ? list.pop() : null;
    var options = {};
    var first = list.shift();
    if (typeof first === 'string' || (first !== null && typeof first === 'object'
        && typeof first.href === 'string' && typeof first.pathname === 'string')) {
        options = fromUrl(typeof first === 'string' ? first : first.href);
    } else if (first !== null && first !== undefined && typeof first === 'object') {
        options = carried(first);
    }
    var over = list.shift();
    if (over !== null && over !== undefined && typeof over === 'object') {
        var named = carried(over);
        Object.keys(named).forEach(function (name) {
            options[name] = named[name];
        });
    }
    return { options: options, callback: callback };
}

function carried(options) {
    var out = {};
    Object.keys(options).forEach(function (name) {
        out[name] = options[name];
    });
    return out;
}

// The options a URL states: the scheme, the authority and the request target it names.
function fromUrl(url) {
    var parsed = require('url').parse(String(url));
    return {
        protocol: parsed.protocol,
        hostname: parsed.hostname,
        port: parsed.port,
        path: parsed.path
    };
}

// ------------------------------------------------------------------ the server

// The reason phrase each status is written with, as HTTP registers them — what a response
// that states a status and no phrase of its own is answered with. (Source: the HTTP status
// code registry, which is what `http.STATUS_CODES` carries.)
var STATUS_CODES = {
    '100': 'Continue',
    '101': 'Switching Protocols',
    '102': 'Processing',
    '103': 'Early Hints',
    '200': 'OK',
    '201': 'Created',
    '202': 'Accepted',
    '203': 'Non-Authoritative Information',
    '204': 'No Content',
    '205': 'Reset Content',
    '206': 'Partial Content',
    '207': 'Multi-Status',
    '208': 'Already Reported',
    '226': 'IM Used',
    '300': 'Multiple Choices',
    '301': 'Moved Permanently',
    '302': 'Found',
    '303': 'See Other',
    '304': 'Not Modified',
    '305': 'Use Proxy',
    '307': 'Temporary Redirect',
    '308': 'Permanent Redirect',
    '400': 'Bad Request',
    '401': 'Unauthorized',
    '402': 'Payment Required',
    '403': 'Forbidden',
    '404': 'Not Found',
    '405': 'Method Not Allowed',
    '406': 'Not Acceptable',
    '407': 'Proxy Authentication Required',
    '408': 'Request Timeout',
    '409': 'Conflict',
    '410': 'Gone',
    '411': 'Length Required',
    '412': 'Precondition Failed',
    '413': 'Payload Too Large',
    '414': 'URI Too Long',
    '415': 'Unsupported Media Type',
    '416': 'Range Not Satisfiable',
    '417': 'Expectation Failed',
    '418': "I'm a Teapot",
    '421': 'Misdirected Request',
    '422': 'Unprocessable Entity',
    '423': 'Locked',
    '424': 'Failed Dependency',
    '425': 'Too Early',
    '426': 'Upgrade Required',
    '428': 'Precondition Required',
    '429': 'Too Many Requests',
    '431': 'Request Header Fields Too Large',
    '451': 'Unavailable For Legal Reasons',
    '500': 'Internal Server Error',
    '501': 'Not Implemented',
    '502': 'Bad Gateway',
    '503': 'Service Unavailable',
    '504': 'Gateway Timeout',
    '505': 'HTTP Version Not Supported',
    '506': 'Variant Also Negotiates',
    '507': 'Insufficient Storage',
    '508': 'Loop Detected',
    '509': 'Bandwidth Limit Exceeded',
    '510': 'Not Extended',
    '511': 'Network Authentication Required'
};

function reasonOf(status) {
    var held = STATUS_CODES[String(status)];
    return held === undefined ? '' : held;
}

// The response a server writes one request's answer through: the status, the headers and the
// bytes it writes are the answer the sender reads, and they are handed over once the
// response is ENDED, which is when a node server has written its last byte.
function ServerResponse(request, answered) {
    EventEmitter.call(this);
    this.req = request;
    this.statusCode = 200;
    this.statusMessage = undefined;
    this.headersSent = false;
    this.writableEnded = false;
    this.writableFinished = false;
    this.finished = false;
    this.sendDate = true;
    this._headers = {};
    this._chunks = [];
    this._answered = answered;
}

inherit(ServerResponse, EventEmitter);

// The header bag is the one a request carries, so the calls that read and write it are the
// same calls: both messages hold their headers the same way.
ServerResponse.prototype.getHeader = ClientRequest.prototype.getHeader;
ServerResponse.prototype.hasHeader = ClientRequest.prototype.hasHeader;
ServerResponse.prototype.removeHeader = ClientRequest.prototype.removeHeader;
ServerResponse.prototype.getHeaders = ClientRequest.prototype.getHeaders;
ServerResponse.prototype.getHeaderNames = ClientRequest.prototype.getHeaderNames;

ServerResponse.prototype.setHeader = function (name, value) {
    if (this.headersSent) {
        var error = new Error('Cannot set headers after they are sent to the client');
        error.code = 'ERR_HTTP_HEADERS_SENT';
        throw error;
    }
    this._headers[String(name).toLowerCase()] = { name: String(name), value: value };
    return this;
};

// "Sends a response header to the request": the status, the phrase where one is stated, and
// the headers the call carries — which are set over the ones already set on the response.
ServerResponse.prototype.writeHead = function (status, reason, headers) {
    var held = headers;
    var phrase = reason;
    if (phrase !== null && typeof phrase === 'object') {
        held = phrase;
        phrase = undefined;
    }
    this.statusCode = Number(status);
    if (typeof phrase === 'string') {
        this.statusMessage = phrase;
    }
    var self = this;
    if (held !== null && held !== undefined && typeof held === 'object') {
        if (Array.isArray(held)) {
            for (var index = 0; index + 1 < held.length; index += 2) {
                self.setHeader(held[index], held[index + 1]);
            }
        } else {
            Object.keys(held).forEach(function (name) {
                self.setHeader(name, held[name]);
            });
        }
    }
    this.headersSent = true;
    return this;
};

ServerResponse.prototype.writeProcessing = function () { return undefined; };
ServerResponse.prototype.flushHeaders = function () {
    this.headersSent = true;
    return undefined;
};
ServerResponse.prototype.setTimeout = function (after, callback) {
    if (typeof callback === 'function') {
        this.once('timeout', callback);
    }
    return this;
};

ServerResponse.prototype.write = function (chunk, encoding, callback) {
    var done = typeof encoding === 'function' ? encoding : callback;
    if (this.writableEnded) {
        var error = new Error('write after end');
        error.code = 'ERR_STREAM_WRITE_AFTER_END';
        if (typeof done === 'function') {
            done(error);
        } else {
            this.emit('error', error);
        }
        return false;
    }
    this.headersSent = true;
    if (chunk !== null && chunk !== undefined && chunk !== '') {
        this._chunks.push(bytesOf(chunk, typeof encoding === 'string' ? encoding : 'utf8'));
    }
    if (typeof done === 'function') {
        done(null);
    }
    return true;
};

ServerResponse.prototype.end = function (chunk, encoding, callback) {
    var done = typeof chunk === 'function'
        ? chunk
        : (typeof encoding === 'function' ? encoding : callback);
    // Whether the body was written in PARTS, which is what decides how it is framed: a
    // server holds nothing back to measure, so a body it had already begun writing when this
    // call came is framed as chunks rather than by its length.
    var inParts = this._chunks.length > 0;
    if (typeof chunk !== 'function' && chunk !== null && chunk !== undefined) {
        this.write(chunk, typeof encoding === 'string' ? encoding : 'utf8');
    }
    if (this.writableEnded) {
        return this;
    }
    this.writableEnded = true;
    this.headersSent = true;
    if (typeof done === 'function') {
        this.once('finish', done);
    }
    var self = this;
    // Handed over once the statements AROUND this call have run, the way a node server's
    // last write reaches the sender after the listener it was written in returns.
    Promise.resolve().then(function () {
        var body = Buffer.concat(self._chunks);
        self.finished = true;
        self.writableFinished = true;
        self.emit('finish');
        self._answered({
            status: self.statusCode,
            reason: typeof self.statusMessage === 'string'
                ? self.statusMessage
                : reasonOf(self.statusCode),
            headers: answeredHeaders(self, body, inParts),
            body: body.toString('base64')
        });
    });
    return this;
};

// The headers one answer CARRIES: the ones the handler set, and the ones a node server writes
// for every answer beside them — the instant it answered, unless the response turned that off
// (`sendDate`), and how the body is framed: its length for a body written in one call, and
// chunks for one written in parts. A status that carries no body at all is framed by neither,
// as HTTP defines those two statuses.
function answeredHeaders(response, body, inParts) {
    var held = {};
    Object.keys(response._headers).forEach(function (lower) {
        held[lower] = response._headers[lower];
    });
    if (response.sendDate && held.date === undefined) {
        held.date = { name: 'Date', value: new Date().toUTCString() };
    }
    var bodyless = response.statusCode === 204 || response.statusCode === 304;
    if (!bodyless && held['content-length'] === undefined
        && held['transfer-encoding'] === undefined) {
        held[inParts ? 'transfer-encoding' : 'content-length'] = inParts
            ? { name: 'Transfer-Encoding', value: 'chunked' }
            : { name: 'Content-Length', value: String(body.length) };
    }
    return headerPairs(held);
}

ServerResponse.prototype.destroy = function () {
    this.destroyed = true;
    return this;
};

// The server a handler answers its own requests with: it holds the port it bound while it is
// listening, and every request this run addresses at that port is handed to it.
function Server(options, requestListener) {
    EventEmitter.call(this);
    var listener = typeof options === 'function' ? options : requestListener;
    this.listening = false;
    // The scheme this server speaks, which is the module it was created with's.
    this._protocol = PROTOCOL;
    this.maxHeadersCount = null;
    this.maxRequestsPerSocket = 0;
    this.timeout = 0;
    this.keepAliveTimeout = 5000;
    this.headersTimeout = 60000;
    this.requestTimeout = 300000;
    this._bound = null;
    if (typeof listener === 'function') {
        this.on('request', listener);
    }
}

inherit(Server, EventEmitter);

// "Starts a server listening for connections." The port is taken as the call states it — and
// for the port `0`, which asks for any free one, from the ephemeral range — so the address
// the server bound reads back as soon as this returns, which is where a program reads the
// port it was given. The `listening` event is emitted after the statements around this call
// have run, as a node server emits it.
Server.prototype.listen = function () {
    var held = listenOptions(arguments);
    var self = this;
    if (this.listening) {
        var already = new Error(
            'Listen method has been called more than once without closing.'
        );
        already.code = 'ERR_SERVER_ALREADY_LISTEN';
        throw already;
    }
    if (typeof held.callback === 'function') {
        this.once('listening', held.callback);
    }
    var port = held.port === 0 ? freePort() : held.port;
    if (port === null || PORTS[String(port)] !== undefined) {
        var taken = new Error('listen EADDRINUSE: address already in use '
            + held.host + ':' + held.port);
        taken.code = 'EADDRINUSE';
        taken.syscall = 'listen';
        taken.address = held.host;
        taken.port = held.port;
        Promise.resolve().then(function () {
            self.emit('error', taken);
        });
        return this;
    }
    PORTS[String(port)] = this;
    this.listening = true;
    this._bound = {
        address: held.host,
        family: held.host.indexOf(':') >= 0 ? 'IPv6' : 'IPv4',
        port: port
    };
    Promise.resolve().then(function () {
        self.emit('listening');
    });
    return this;
};

// "Returns the bound address […] of the server as reported by the operating system",
// and `null` for a server that is not listening.
Server.prototype.address = function () {
    return this._bound;
};

// "Stops the server from accepting new connections": the port is released, so a request
// addressed at it is refused the way a connection nothing accepts is.
Server.prototype.close = function (callback) {
    var self = this;
    if (!this.listening) {
        var notRunning = new Error('Server is not running.');
        notRunning.code = 'ERR_SERVER_NOT_RUNNING';
        if (typeof callback === 'function') {
            Promise.resolve().then(function () {
                callback(notRunning);
            });
        }
        return this;
    }
    if (typeof callback === 'function') {
        this.once('close', callback);
    }
    delete PORTS[String(this._bound.port)];
    this.listening = false;
    this._bound = null;
    Promise.resolve().then(function () {
        self.emit('close');
    });
    return this;
};

// A connection is carried in this process and never held open, so there is none of these
// calls' work to do: each is accepted and changes nothing about the server.
Server.prototype.closeAllConnections = function () { return undefined; };
Server.prototype.closeIdleConnections = function () { return undefined; };
Server.prototype.getConnections = function (callback) {
    if (typeof callback === 'function') {
        callback(null, 0);
    }
    return undefined;
};
Server.prototype.ref = function () { return this; };
Server.prototype.unref = function () { return this; };
Server.prototype.setTimeout = function (after, callback) {
    this.timeout = after;
    if (typeof callback === 'function') {
        this.once('timeout', callback);
    }
    return this;
};

// The port and the host one `listen` call states, in each of the forms node reads it: a
// port, a port with a host, an options object, and a call stating nothing but its callback —
// which asks for any free port.
function listenOptions(args) {
    var list = Array.prototype.slice.call(args);
    var callback = typeof list[list.length - 1] === 'function' ? list.pop() : null;
    var first = list.shift();
    var port = 0;
    var bound = null;
    if (first !== null && first !== undefined && typeof first === 'object') {
        port = Number(text(first.port) === '' ? 0 : first.port);
        bound = text(first.host) === '' ? null : String(first.host);
    } else if (first !== null && first !== undefined) {
        port = Number(first);
        var next = list.shift();
        bound = typeof next === 'string' ? next : null;
    }
    return {
        port: isFinite(port) && port >= 0 ? port : 0,
        // A server that names no host of its own binds every address, which is the host node
        // reports for one.
        host: bound === null ? '::' : bound,
        callback: callback
    };
}

// A port nothing of this run is listening on, taken from the ephemeral range; `null` where
// the whole range is held, which is what makes such a `listen` fail as one for an address
// already in use.
function freePort() {
    for (var port = FIRST_EPHEMERAL; port <= LAST_EPHEMERAL; port += 1) {
        if (PORTS[String(port)] === undefined) {
            return port;
        }
    }
    return null;
}

// ------------------------------------------------------------------ the module

function request() {
    var held = stated(arguments);
    return new ClientRequest(held.options, held.callback);
}

// "Since most requests are GET requests without bodies, Node.js provides this convenience
// method […] the only difference […] is that it sets the method to GET and calls req.end()
// automatically."
function get() {
    var held = stated(arguments);
    held.options.method = text(held.options.method) === '' ? 'GET' : held.options.method;
    var sending = new ClientRequest(held.options, held.callback);
    sending.end();
    return sending;
}

// "Returns a new instance of `http.Server`", with the listener it is handed — where one is —
// attached to its `request` event.
function createServer(options, requestListener) {
    return new Server(options, requestListener);
}

// The connection POOL a node program configures its requests with. There is no connection
// to pool — every request is carried by the host into this process, one at a time — so an
// agent holds the options it was given and nothing else, and passing one changes nothing
// about the request that is sent.
function Agent(options) {
    this.options = options === null || options === undefined ? {} : options;
    this.requests = {};
    this.sockets = {};
    this.freeSockets = {};
    this.maxSockets = Infinity;
    this.maxFreeSockets = 256;
}

Agent.prototype.destroy = function () { return undefined; };

exports.request = request;
exports.get = get;
exports.createServer = createServer;
exports.Server = Server;
exports.ServerResponse = ServerResponse;
exports.STATUS_CODES = STATUS_CODES;
exports.Agent = Agent;
exports.globalAgent = new Agent({});
exports.ClientRequest = ClientRequest;
exports.IncomingMessage = IncomingMessage;

// The port space, which the two modules SHARE: not an export of either module — a node
// program reads no such member off one — so it is held where the other module reads it and
// nowhere a program looks.
Object.defineProperty(exports, '__ports', { value: PORTS, enumerable: false });
"##;

