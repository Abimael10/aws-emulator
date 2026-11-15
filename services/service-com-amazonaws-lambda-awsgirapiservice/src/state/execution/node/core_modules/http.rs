// SPDX-License-Identifier: CC0-1.0

//! `http` and `https`: the HTTP CLIENT a node program sends a request of its own with,
//! over the one host seam that carries a guest's request into this same emulator
//! ([`crate::state::execution::guest_http`]).
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
//! path, so there is no server half here: a program that would LISTEN finds no
//! `createServer`, exactly as it finds no `net`.
//!
//! Nothing about an answer is synthesised. The status, the headers and the bytes a handler
//! reads back are the ones the emulated service answered, and a request the host did not
//! make is reported as the failure a node program reads off a request that was not made —
//! never as an answer nothing sent.

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

function host() {
    return globalThis.__lambdaRuntime;
}

function inherit(child, parent) {
    child.prototype = Object.create(parent.prototype);
    child.prototype.constructor = child;
}

// ------------------------------------------------------------------ the answer

// The answer one request came back with, as the readable a node program reads it through:
// the status and the headers are readable as soon as it is handed over, and the body
// arrives as this stream's own data.
function IncomingMessage(request, answered) {
    Readable.call(this, {});
    this.httpVersion = '1.1';
    this.httpVersionMajor = 1;
    this.httpVersionMinor = 1;
    this.statusCode = answered.status;
    this.statusMessage = typeof answered.reason === 'string' ? answered.reason : '';
    this.method = request.method;
    this.url = request.path;
    this.req = request;
    this.complete = false;
    this.headers = {};
    this.rawHeaders = [];
    var self = this;
    var carried = Array.isArray(answered.headers) ? answered.headers : [];
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
}

inherit(IncomingMessage, Readable);

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

// Hand ONE request to the host, and report what came back to the handlers the program
// attached to it.
function send(request) {
    if (request.destroyed || request.aborted || request._sent) {
        return;
    }
    request._sent = true;
    request._clearTimer();
    var answered;
    try {
        answered = JSON.parse(host().httpSend(
            request.method,
            addressOf(request),
            JSON.stringify(headerPairs(request)),
            Buffer.concat(request._chunks).toString('base64')
        ));
    } catch (error) {
        raise(request, error);
        return;
    }
    request.finished = true;
    request.emit('finish');
    if (answered.refused !== undefined && answered.refused !== null) {
        raise(request, notSent(answered, addressOf(request)));
        return;
    }
    var response = new IncomingMessage(request, answered);
    request.res = response;
    request.emit('response', response);
    response._deliver(Buffer.from(text(answered.body), 'base64'));
}

// The URL the host is asked to send to: the scheme, the authority — which carries the port
// only when it is not the scheme's own — and the request target.
function addressOf(request) {
    var port = text(request.port);
    var authority = request.host
        + (port === '' || port === DEFAULT_PORT[request.protocol] ? '' : ':' + port);
    var path = request.path.charAt(0) === '/' ? request.path : '/' + request.path;
    return request.protocol + '//' + authority + path;
}

// The headers the request carries, as the name/value pairs the host is handed: a header
// set to a list is carried once per entry, as HTTP carries a repeated header.
function headerPairs(request) {
    var held = request._headers;
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
exports.Agent = Agent;
exports.globalAgent = new Agent({});
exports.ClientRequest = ClientRequest;
exports.IncomingMessage = IncomingMessage;
"##;
