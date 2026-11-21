// SPDX-License-Identifier: CC0-1.0

//! `dns`: the name service of the execution environment, as a Node program asks it.
//!
//! A lookup here is a REAL resolution: it is answered by the one resolver of the run
//! ([`crate::state::execution::guest_resolver`], reached through the runtime protocol's
//! `resolveAddresses`), which is the same resolver every other name a handler resolves is
//! answered by — so one name stands for one thing however the handler asks for it. That
//! resolver answers out of the zones this emulator can name an address in: this machine's own
//! loopback, where the planes this emulator opens are served; the private zone of the VPC the
//! function is attached to; and the public names a DNS standard itself fixes. No lookup leaves
//! this process.
//!
//! A name no such zone holds is answered the way a resolver answers a name that does not
//! exist — `ENOTFOUND`, which is what a program branches on — rather than with an address
//! nothing is served at: inventing one would report a resolution that never happened.
//!
//! The REVERSE direction is not a resolution this emulator can make: no zone it holds is a
//! pointer zone, so `reverse` and `lookupService` answer the way a resolver with no such
//! record does, and never with a name.

/// `dns`: the forward lookups, in both the callback and the promise form.
pub(super) const DNS_SOURCE: &str = r##"
'use strict';

var net = require('net');

// The host half of this module: the one reading of a name the whole runtime shares.
function host() {
    return globalThis.__lambdaRuntime;
}

// The addresses a name resolves to, each with the family of address it is.
function resolved(hostname) {
    var addresses = host().resolveAddresses(hostname);
    var out = [];
    for (var index = 0; index < addresses.length; index += 1) {
        out.push({
            address: addresses[index],
            family: net.isIPv6(addresses[index]) ? 6 : 4
        });
    }
    return out;
}

// The failure a lookup of a name no zone holds reports, as node builds one: the message is the
// syscall and the code, and the properties beside it are what a caller branches on.
function notFound(syscall, hostname) {
    var error = new Error(syscall + ' ENOTFOUND ' + hostname);
    error.errno = -3008;
    error.code = 'ENOTFOUND';
    error.syscall = syscall;
    error.hostname = hostname;
    return error;
}

// The lifetime a record this resolver answers carries, which a program reads off a record
// query (`resolveAny`, and `resolve4`/`resolve6` asked with `ttl`). The zones answered from are
// held in this process and change only when the resource behind a name does, so what is
// reported is the one lifetime those answers have: none to cache by.
var RECORD_TTL = 0;

// The arguments of a lookup, read the way node reads them: an optional options object — or the
// family as a bare number — before the trailing callback.
function lookupArguments(options, callback) {
    if (typeof options === 'function') {
        return { options: {}, callback: options };
    }
    if (typeof options === 'number') {
        return { options: { family: options }, callback: callback };
    }
    return {
        options: options === null || options === undefined ? {} : options,
        callback: typeof callback === 'function' ? callback : null
    };
}

// The addresses of `hostname` under `options`, or a failure to report: the family the caller
// asked for filters what the resolver answered, and `all` decides whether one address or every
// one of them is reported.
function lookupAnswer(hostname, options) {
    var named = hostname === null || hostname === undefined ? '' : String(hostname);
    var family = Number(options.family === undefined ? 0 : options.family);
    var found = resolved(named).filter(function (entry) {
        return family === 0 || family === entry.family;
    });
    if (found.length === 0) {
        return { error: notFound('getaddrinfo', named) };
    }
    return { answer: options.all === true ? found : found[0] };
}

function lookup(hostname, options, callback) {
    var taken = lookupArguments(options, callback);
    var done = taken.callback === null ? function () { return undefined; } : taken.callback;
    var answered = lookupAnswer(hostname, taken.options);
    Promise.resolve().then(function () {
        if (answered.error !== undefined) {
            done(answered.error);
            return;
        }
        if (Array.isArray(answered.answer)) {
            done(null, answered.answer);
            return;
        }
        // "The callback has arguments (err, address, family)" for a single answer.
        done(null, answered.answer.address, answered.answer.family);
    });
    return undefined;
}

// A query for records of one kind. The kinds this resolver holds are the address records; a
// kind it holds none of is answered the way a resolver with no such record answers.
//
// `records` says which SHAPE the answer takes: an address query reports the addresses
// themselves, and `resolveAny` reports the record objects node reports for it — the type, the
// address and the lifetime each record is answered with.
function queryAnswer(syscall, hostname, family, records) {
    var named = hostname === null || hostname === undefined ? '' : String(hostname);
    var found = resolved(named).filter(function (entry) {
        return family === 0 || entry.family === family;
    }).map(function (entry) {
        return records === true
            ? { type: entry.family === 6 ? 'AAAA' : 'A', address: entry.address, ttl: RECORD_TTL }
            : entry.address;
    });
    return found.length === 0
        ? { error: notFound(syscall, named) }
        : { answer: found };
}

function query(syscall, family, records) {
    return function (hostname, options, callback) {
        var done = typeof options === 'function' ? options : callback;
        var report = typeof done === 'function' ? done : function () { return undefined; };
        var answered = queryAnswer(syscall, hostname, family, records === true);
        Promise.resolve().then(function () {
            if (answered.error !== undefined) {
                report(answered.error);
                return;
            }
            report(null, answered.answer);
        });
        return undefined;
    };
}

var resolve4 = query('queryA', 4, false);
var resolve6 = query('queryAaaa', 6, false);
var resolveAny = query('queryAny', 0, true);

// The record kinds a query may name, and the syscall each is reported under. Only the address
// records are answered from a zone this resolver holds; every other kind is a record it holds
// none of, which is what `ENOTFOUND` says.
var ABSENT_RECORDS = [
    ['resolveMx', 'queryMx'],
    ['resolveTxt', 'queryTxt'],
    ['resolveSrv', 'querySrv'],
    ['resolveNs', 'queryNs'],
    ['resolveCname', 'queryCname'],
    ['resolveSoa', 'querySoa'],
    ['resolvePtr', 'queryPtr'],
    ['resolveNaptr', 'queryNaptr'],
    ['resolveCaa', 'queryCaa']
];

function absent(syscall) {
    return function (hostname, callback) {
        var report = typeof callback === 'function' ? callback : function () { return undefined; };
        var named = hostname === null || hostname === undefined ? '' : String(hostname);
        Promise.resolve().then(function () {
            report(notFound(syscall, named));
        });
        return undefined;
    };
}

function reverse(address, callback) {
    var report = typeof callback === 'function' ? callback : function () { return undefined; };
    var named = address === null || address === undefined ? '' : String(address);
    Promise.resolve().then(function () {
        report(notFound('getHostByAddr', named));
    });
    return undefined;
}

function lookupService(address, port, callback) {
    var report = typeof callback === 'function' ? callback : function () { return undefined; };
    var named = address === null || address === undefined ? '' : String(address);
    Promise.resolve().then(function () {
        report(notFound('getnameinfo', named));
    });
    return undefined;
}

function resolve(hostname, kind, callback) {
    var named = typeof kind === 'string' ? kind : 'A';
    var done = typeof kind === 'function' ? kind : callback;
    if (named === 'A') {
        return resolve4(hostname, done);
    }
    if (named === 'AAAA') {
        return resolve6(hostname, done);
    }
    if (named === 'ANY') {
        return resolveAny(hostname, done);
    }
    return absent('query' + named)(hostname, done);
}

// "An independent resolver for DNS requests": the same lookups, as an object a program holds —
// every instance answers from the one resolver of the execution environment.
function Resolver() {
    this._servers = [];
}

// The promise form of a callback lookup: the same answer, reported as the promise node's own
// `dns/promises` reports it as.
function promised(call, answers) {
    return function () {
        var args = Array.prototype.slice.call(arguments);
        return new Promise(function (resolve, reject) {
            args.push(function (error) {
                if (error) {
                    reject(error);
                    return;
                }
                var reported = Array.prototype.slice.call(arguments, 1);
                resolve(answers === 'lookup' && reported.length > 1
                    ? { address: reported[0], family: reported[1] }
                    : reported[0]);
            });
            call.apply(undefined, args);
        });
    };
}

var callbacks = {
    lookup: lookup,
    lookupService: lookupService,
    resolve: resolve,
    resolve4: resolve4,
    resolve6: resolve6,
    resolveAny: resolveAny,
    reverse: reverse
};

for (var index = 0; index < ABSENT_RECORDS.length; index += 1) {
    callbacks[ABSENT_RECORDS[index][0]] = absent(ABSENT_RECORDS[index][1]);
}

var names = Object.keys(callbacks);
for (var at = 0; at < names.length; at += 1) {
    exports[names[at]] = callbacks[names[at]];
    Resolver.prototype[names[at]] = callbacks[names[at]];
}

Resolver.prototype.getServers = function () { return this._servers.slice(); };
Resolver.prototype.setServers = function (servers) {
    this._servers = Array.isArray(servers) ? servers.slice() : [];
};
Resolver.prototype.cancel = function () { return undefined; };
Resolver.prototype.setLocalAddress = function () { return undefined; };

exports.Resolver = Resolver;
// "Returns an array of IP address strings … for the current DNS servers": the resolver of this
// execution environment is in this process, so no server address is reported for it.
exports.getServers = function () { return []; };
exports.setServers = function () { return undefined; };
exports.setDefaultResultOrder = function () { return undefined; };
exports.getDefaultResultOrder = function () { return 'verbatim'; };
exports.ADDRCONFIG = 1024;
exports.V4MAPPED = 8;
exports.ALL = 16;
exports.NODATA = 'ENODATA';
exports.NOTFOUND = 'ENOTFOUND';
exports.BADFAMILY = 'EBADFAMILY';
exports.TIMEOUT = 'ETIMEOUT';

// The promise form of the whole module, which is also the `dns/promises` module.
var promises = { Resolver: Resolver };
for (var promised_at = 0; promised_at < names.length; promised_at += 1) {
    promises[names[promised_at]] = promised(
        callbacks[names[promised_at]],
        names[promised_at] === 'lookup' ? 'lookup' : 'one'
    );
}
promises.getServers = exports.getServers;
promises.setServers = exports.setServers;
promises.setDefaultResultOrder = exports.setDefaultResultOrder;
promises.getDefaultResultOrder = exports.getDefaultResultOrder;
exports.promises = promises;
"##;

/// `dns/promises`: the same lookups, under the name the promise form is imported by.
pub(super) const DNS_PROMISES_SOURCE: &str = r##"
'use strict';

module.exports = require('dns').promises;
"##;
