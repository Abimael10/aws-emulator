// SPDX-License-Identifier: CC0-1.0

//! The JavaScript a Node.js GUEST reaches that is the same for every service running one:
//! the pure core modules of Node's standard library, and the module registry that resolves
//! and loads a package's own files.
//!
//! Several services in this emulator run caller-supplied Node.js code — a function's
//! handler, a web function's HTTP server — each in its own engine, with its own host seams
//! and its own bounds. What they do NOT each need is their own copy of the parts that reach
//! nothing of the host: the path algebra, the URL and query codecs, the value formatter and
//! the event emitter are COMPUTATION over values the run already holds, and Node's module
//! resolution ([`MODULE_REGISTRY_SOURCE`]) is a documented rule over the files of a package
//! rather than anything a service decides. Those live here, once, so two services cannot
//! answer a guest differently about the same documented behaviour.
//!
//! What stays in the service crate is what is genuinely the service's: the engine and its
//! ceilings, the host functions a run reaches, the environment and `process` its execution
//! environment publishes, and the protocol it hands the guest its work over (an invocation's
//! event, or one HTTP request).
//!
//! Each core module below is the source of a COMMONJS module — the form the registry
//! compiles — so a consumer mounts it by name ([`PURE_CORE_MODULES`]) and the guest reaches
//! it under both its bare and its `node:` name, exactly as it reaches a file of its own
//! package. None of them opens a path, a socket or a process, and none reads a host
//! environment variable: `url` is the only one that requires another (`querystring`), so a
//! consumer that mounts it mounts that one too.

/// The pure core modules of Node's standard library this emulator supplies to a guest, by
/// the name a program imports each under.
///
/// A consumer mounts these beside the modules its own execution environment answers for
/// (`process`, `os`, a filesystem, an HTTP client or server), and the registry resolves a
/// bare specifier to one of them.
pub const PURE_CORE_MODULES: &[(&str, &str)] = &[
    ("path", PATH_SOURCE),
    ("util", UTIL_SOURCE),
    ("events", EVENTS_SOURCE),
    ("querystring", QUERYSTRING_SOURCE),
    ("url", URL_SOURCE),
];

/// The source of the pure core module `name` names, or `None` when it names none of them.
///
/// The `node:` prefix a modern import spells a core module with resolves to the same module
/// as the bare name.
#[must_use]
pub fn pure_core_module_source(name: &str) -> Option<&'static str> {
    let bare = name.strip_prefix("node:").unwrap_or(name);
    PURE_CORE_MODULES
        .iter()
        .find(|(module, _)| *module == bare)
        .map(|(_, source)| *source)
}

/// `path`: the POSIX path algebra, over strings alone.
///
/// The working directory a relative path resolves against is the task root a function's
/// package is unpacked at: no host directory is consulted, and no path here is opened.
pub const PATH_SOURCE: &str = r##"
'use strict';

var TASK_ROOT = '/var/task';

function assertPath(value) {
    if (typeof value !== 'string') {
        throw new TypeError('Path must be a string. Received ' + JSON.stringify(value));
    }
}

function normalizeSegments(segments, allowAboveRoot) {
    var out = [];
    for (var index = 0; index < segments.length; index += 1) {
        var segment = segments[index];
        if (segment === '' || segment === '.') {
            continue;
        }
        if (segment === '..') {
            if (out.length > 0 && out[out.length - 1] !== '..') {
                out.pop();
            } else if (allowAboveRoot) {
                out.push('..');
            }
            continue;
        }
        out.push(segment);
    }
    return out;
}

function normalize(path) {
    assertPath(path);
    if (path === '') {
        return '.';
    }
    var absolute = path.charAt(0) === '/';
    var trailing = path.length > 1 && path.charAt(path.length - 1) === '/';
    var joined = normalizeSegments(path.split('/'), !absolute).join('/');
    if (joined === '') {
        return absolute ? '/' : (trailing ? './' : '.');
    }
    if (trailing) {
        joined = joined + '/';
    }
    return absolute ? '/' + joined : joined;
}

function join() {
    var parts = [];
    for (var index = 0; index < arguments.length; index += 1) {
        assertPath(arguments[index]);
        if (arguments[index] !== '') {
            parts.push(arguments[index]);
        }
    }
    if (parts.length === 0) {
        return '.';
    }
    return normalize(parts.join('/'));
}

function resolve() {
    var resolved = '';
    var absolute = false;
    for (var index = arguments.length - 1; index >= 0 && !absolute; index -= 1) {
        var part = arguments[index];
        assertPath(part);
        if (part === '') {
            continue;
        }
        resolved = resolved === '' ? part : part + '/' + resolved;
        absolute = part.charAt(0) === '/';
    }
    if (!absolute) {
        resolved = resolved === '' ? TASK_ROOT : TASK_ROOT + '/' + resolved;
    }
    return '/' + normalizeSegments(resolved.split('/'), false).join('/');
}

function dirname(path) {
    assertPath(path);
    var normalized = path.length > 1 ? path.replace(/\/+$/, '') : path;
    var cut = normalized.lastIndexOf('/');
    if (cut < 0) {
        return '.';
    }
    if (cut === 0) {
        return '/';
    }
    return normalized.slice(0, cut);
}

function basename(path, suffix) {
    assertPath(path);
    var normalized = path.length > 1 ? path.replace(/\/+$/, '') : path;
    var cut = normalized.lastIndexOf('/');
    var name = cut < 0 ? normalized : normalized.slice(cut + 1);
    if (typeof suffix === 'string' && suffix !== '' && name !== suffix) {
        if (name.length >= suffix.length && name.slice(name.length - suffix.length) === suffix) {
            name = name.slice(0, name.length - suffix.length);
        }
    }
    return name;
}

function extname(path) {
    var name = basename(path);
    var cut = name.lastIndexOf('.');
    if (cut <= 0) {
        return '';
    }
    return name.slice(cut);
}

function isAbsolute(path) {
    assertPath(path);
    return path.charAt(0) === '/';
}

function relative(from, to) {
    var fromParts = resolve(from).split('/');
    var toParts = resolve(to).split('/');
    var shared = 0;
    while (shared < fromParts.length && shared < toParts.length
        && fromParts[shared] === toParts[shared]) {
        shared += 1;
    }
    var up = [];
    for (var index = shared; index < fromParts.length; index += 1) {
        if (fromParts[index] !== '') {
            up.push('..');
        }
    }
    return up.concat(toParts.slice(shared)).join('/');
}

function parse(path) {
    assertPath(path);
    var base = basename(path);
    var ext = extname(path);
    return {
        root: isAbsolute(path) ? '/' : '',
        dir: dirname(path),
        base: base,
        ext: ext,
        name: ext === '' ? base : base.slice(0, base.length - ext.length)
    };
}

function format(parsed) {
    if (parsed === null || typeof parsed !== 'object') {
        throw new TypeError('The "pathObject" argument must be of type object.');
    }
    var dir = parsed.dir || parsed.root || '';
    var base = parsed.base || ((parsed.name || '') + (parsed.ext || ''));
    if (dir === '') {
        return base;
    }
    return dir === parsed.root ? dir + base : dir + '/' + base;
}

var path = {
    normalize: normalize,
    join: join,
    resolve: resolve,
    dirname: dirname,
    basename: basename,
    extname: extname,
    isAbsolute: isAbsolute,
    relative: relative,
    parse: parse,
    format: format,
    sep: '/',
    delimiter: ':'
};
path.posix = path;
module.exports = path;
"##;

/// `util`: the formatting and promise adapters a Node program reads values with.
pub const UTIL_SOURCE: &str = r##"
'use strict';

function quoted(text) {
    return "'" + text.replace(/\\/g, '\\\\').replace(/'/g, "\\'").replace(/\n/g, '\\n') + "'";
}

function inspect(value, seen) {
    seen = seen || [];
    if (value === undefined) {
        return 'undefined';
    }
    if (value === null) {
        return 'null';
    }
    var kind = typeof value;
    if (kind === 'string') {
        return quoted(value);
    }
    if (kind === 'number' || kind === 'boolean' || kind === 'symbol') {
        return String(value);
    }
    if (kind === 'bigint') {
        return String(value) + 'n';
    }
    if (kind === 'function') {
        return value.name ? '[Function: ' + value.name + ']' : '[Function (anonymous)]';
    }
    if (value instanceof Error) {
        return typeof value.stack === 'string' && value.stack !== ''
            ? value.stack
            : value.name + ': ' + value.message;
    }
    if (value instanceof Date) {
        return value.toISOString();
    }
    if (value instanceof RegExp) {
        return String(value);
    }
    for (var index = 0; index < seen.length; index += 1) {
        if (seen[index] === value) {
            return '[Circular *1]';
        }
    }
    seen = seen.concat([value]);
    if (Array.isArray(value)) {
        var items = [];
        for (var element = 0; element < value.length; element += 1) {
            items.push(inspect(value[element], seen));
        }
        return items.length === 0 ? '[]' : '[ ' + items.join(', ') + ' ]';
    }
    if (typeof Map === 'function' && value instanceof Map) {
        var entries = [];
        value.forEach(function (held, key) {
            entries.push(inspect(key, seen) + ' => ' + inspect(held, seen));
        });
        return 'Map(' + value.size + ') {' + (entries.length ? ' ' + entries.join(', ') + ' ' : '') + '}';
    }
    if (typeof Set === 'function' && value instanceof Set) {
        var members = [];
        value.forEach(function (held) {
            members.push(inspect(held, seen));
        });
        return 'Set(' + value.size + ') {' + (members.length ? ' ' + members.join(', ') + ' ' : '') + '}';
    }
    var keys = Object.keys(value);
    var pairs = [];
    for (var key = 0; key < keys.length; key += 1) {
        pairs.push(keys[key] + ': ' + inspect(value[keys[key]], seen));
    }
    var prefix = '';
    if (value.constructor && value.constructor.name && value.constructor.name !== 'Object') {
        prefix = value.constructor.name + ' ';
    }
    return pairs.length === 0 ? prefix + '{}' : prefix + '{ ' + pairs.join(', ') + ' }';
}

function format() {
    var args = Array.prototype.slice.call(arguments);
    var out = [];
    if (typeof args[0] === 'string' && args[0].indexOf('%') >= 0) {
        var template = args.shift();
        var rendered = '';
        var index = 0;
        while (index < template.length) {
            var character = template.charAt(index);
            if (character !== '%' || index + 1 >= template.length) {
                rendered = rendered + character;
                index += 1;
                continue;
            }
            var directive = template.charAt(index + 1);
            index += 2;
            if (directive === '%') {
                rendered = rendered + '%';
            } else if (directive === 's') {
                var next = args.shift();
                rendered = rendered + (typeof next === 'string' ? next : inspect(next));
            } else if (directive === 'd' || directive === 'i') {
                rendered = rendered + String(parseInt(args.shift(), 10));
            } else if (directive === 'f') {
                rendered = rendered + String(parseFloat(args.shift()));
            } else if (directive === 'j') {
                try {
                    rendered = rendered + JSON.stringify(args.shift());
                } catch (error) {
                    rendered = rendered + '[Circular]';
                }
            } else if (directive === 'o' || directive === 'O') {
                rendered = rendered + inspect(args.shift());
            } else {
                rendered = rendered + '%' + directive;
            }
        }
        out.push(rendered);
    }
    for (var rest = 0; rest < args.length; rest += 1) {
        out.push(typeof args[rest] === 'string' ? args[rest] : inspect(args[rest]));
    }
    return out.join(' ');
}

function promisify(original) {
    if (typeof original !== 'function') {
        throw new TypeError('The "original" argument must be of type function.');
    }
    return function () {
        var args = Array.prototype.slice.call(arguments);
        var self = this;
        return new Promise(function (resolve, reject) {
            args.push(function (error, value) {
                if (error) {
                    reject(error);
                } else {
                    resolve(value);
                }
            });
            try {
                original.apply(self, args);
            } catch (error) {
                reject(error);
            }
        });
    };
}

function callbackify(original) {
    if (typeof original !== 'function') {
        throw new TypeError('The "original" argument must be of type function.');
    }
    return function () {
        var args = Array.prototype.slice.call(arguments);
        var callback = args.pop();
        Promise.resolve(original.apply(this, args)).then(function (value) {
            callback(null, value);
        }, function (error) {
            callback(error || new Error('Promise was rejected with a falsy value'));
        });
    };
}

function inherits(constructor, parent) {
    constructor.super_ = parent;
    constructor.prototype = Object.create(parent.prototype, {
        constructor: {
            value: constructor,
            enumerable: false,
            writable: true,
            configurable: true
        }
    });
}

module.exports = {
    format: format,
    inspect: inspect,
    promisify: promisify,
    callbackify: callbackify,
    inherits: inherits,
    deprecate: function (fn) { return fn; },
    types: {
        isPromise: function (value) {
            return !!value && typeof value.then === 'function';
        },
        isDate: function (value) { return value instanceof Date; },
        isRegExp: function (value) { return value instanceof RegExp; },
        isMap: function (value) { return typeof Map === 'function' && value instanceof Map; },
        isSet: function (value) { return typeof Set === 'function' && value instanceof Set; }
    }
};
"##;

/// `events`: the `EventEmitter` a Node program dispatches its own events through.
pub const EVENTS_SOURCE: &str = r##"
'use strict';

function EventEmitter() {
    this._events = Object.create(null);
    this._maxListeners = 10;
}

EventEmitter.prototype._listenersFor = function (type) {
    if (!this._events) {
        this._events = Object.create(null);
    }
    if (!this._events[type]) {
        this._events[type] = [];
    }
    return this._events[type];
};

EventEmitter.prototype.addListener = function (type, listener) {
    if (typeof listener !== 'function') {
        throw new TypeError('The "listener" argument must be of type function.');
    }
    this._listenersFor(type).push(listener);
    return this;
};

EventEmitter.prototype.on = EventEmitter.prototype.addListener;

EventEmitter.prototype.prependListener = function (type, listener) {
    if (typeof listener !== 'function') {
        throw new TypeError('The "listener" argument must be of type function.');
    }
    this._listenersFor(type).unshift(listener);
    return this;
};

EventEmitter.prototype.once = function (type, listener) {
    var self = this;
    var wrapper = function () {
        self.removeListener(type, wrapper);
        listener.apply(self, Array.prototype.slice.call(arguments));
    };
    wrapper.listener = listener;
    return this.on(type, wrapper);
};

EventEmitter.prototype.removeListener = function (type, listener) {
    var listeners = this._listenersFor(type);
    for (var index = 0; index < listeners.length; index += 1) {
        if (listeners[index] === listener || listeners[index].listener === listener) {
            listeners.splice(index, 1);
            break;
        }
    }
    return this;
};

EventEmitter.prototype.off = EventEmitter.prototype.removeListener;

EventEmitter.prototype.removeAllListeners = function (type) {
    if (type === undefined) {
        this._events = Object.create(null);
    } else {
        this._events[type] = [];
    }
    return this;
};

EventEmitter.prototype.listeners = function (type) {
    return this._listenersFor(type).slice();
};

EventEmitter.prototype.listenerCount = function (type) {
    return this._listenersFor(type).length;
};

EventEmitter.prototype.eventNames = function () {
    return Object.keys(this._events || {});
};

EventEmitter.prototype.setMaxListeners = function (count) {
    this._maxListeners = count;
    return this;
};

EventEmitter.prototype.getMaxListeners = function () {
    return this._maxListeners;
};

EventEmitter.prototype.emit = function (type) {
    var args = Array.prototype.slice.call(arguments, 1);
    var listeners = this._listenersFor(type).slice();
    for (var index = 0; index < listeners.length; index += 1) {
        listeners[index].apply(this, args);
    }
    return listeners.length > 0;
};

EventEmitter.EventEmitter = EventEmitter;
EventEmitter.defaultMaxListeners = 10;
module.exports = EventEmitter;
"##;

/// `querystring`: the legacy query codec.
pub const QUERYSTRING_SOURCE: &str = r##"
'use strict';

function escapeText(text) {
    return encodeURIComponent(String(text)).replace(/%20/g, '+');
}

function unescapeText(text) {
    try {
        return decodeURIComponent(String(text).replace(/\+/g, ' '));
    } catch (error) {
        return String(text);
    }
}

exports.escape = escapeText;
exports.unescape = unescapeText;

exports.parse = function (text, separator, equals) {
    var parsed = Object.create(null);
    if (typeof text !== 'string' || text === '') {
        return parsed;
    }
    var pairSeparator = separator === undefined ? '&' : separator;
    var valueSeparator = equals === undefined ? '=' : equals;
    var pairs = text.split(pairSeparator);
    for (var index = 0; index < pairs.length; index += 1) {
        if (pairs[index] === '') {
            continue;
        }
        var cut = pairs[index].indexOf(valueSeparator);
        var name = cut < 0 ? pairs[index] : pairs[index].slice(0, cut);
        var value = cut < 0 ? '' : pairs[index].slice(cut + valueSeparator.length);
        var key = unescapeText(name);
        var held = unescapeText(value);
        if (parsed[key] === undefined) {
            parsed[key] = held;
        } else if (Array.isArray(parsed[key])) {
            parsed[key].push(held);
        } else {
            parsed[key] = [parsed[key], held];
        }
    }
    return parsed;
};

exports.stringify = function (values, separator, equals) {
    var pairSeparator = separator === undefined ? '&' : separator;
    var valueSeparator = equals === undefined ? '=' : equals;
    if (values === null || typeof values !== 'object') {
        return '';
    }
    var out = [];
    var keys = Object.keys(values);
    for (var index = 0; index < keys.length; index += 1) {
        var value = values[keys[index]];
        var name = escapeText(keys[index]);
        if (Array.isArray(value)) {
            for (var at = 0; at < value.length; at += 1) {
                out.push(name + valueSeparator + escapeText(value[at]));
            }
            continue;
        }
        out.push(name + valueSeparator + escapeText(value === undefined || value === null ? '' : value));
    }
    return out.join(pairSeparator);
};

exports.decode = exports.parse;
exports.encode = exports.stringify;
"##;

/// `url`: the URL object, its search parameters, and the two file-URL conversions an ES
/// module resolves its own siblings with.
pub const URL_SOURCE: &str = r##"
'use strict';

var querystring = require('querystring');

function URLSearchParams(init) {
    this._pairs = [];
    if (typeof init === 'string') {
        var parsed = querystring.parse(init.charAt(0) === '?' ? init.slice(1) : init);
        var names = Object.keys(parsed);
        for (var index = 0; index < names.length; index += 1) {
            var held = parsed[names[index]];
            if (Array.isArray(held)) {
                for (var at = 0; at < held.length; at += 1) {
                    this._pairs.push([names[index], held[at]]);
                }
            } else {
                this._pairs.push([names[index], held]);
            }
        }
    } else if (Array.isArray(init)) {
        for (var pair = 0; pair < init.length; pair += 1) {
            this._pairs.push([String(init[pair][0]), String(init[pair][1])]);
        }
    } else if (init !== null && typeof init === 'object') {
        var keys = Object.keys(init);
        for (var key = 0; key < keys.length; key += 1) {
            this._pairs.push([keys[key], String(init[keys[key]])]);
        }
    }
}

URLSearchParams.prototype.append = function (name, value) {
    this._pairs.push([String(name), String(value)]);
};
URLSearchParams.prototype.set = function (name, value) {
    var text = String(name);
    var kept = this._pairs.filter(function (pair) { return pair[0] !== text; });
    kept.push([text, String(value)]);
    this._pairs = kept;
};
URLSearchParams.prototype.get = function (name) {
    var text = String(name);
    for (var index = 0; index < this._pairs.length; index += 1) {
        if (this._pairs[index][0] === text) {
            return this._pairs[index][1];
        }
    }
    return null;
};
URLSearchParams.prototype.getAll = function (name) {
    var text = String(name);
    return this._pairs
        .filter(function (pair) { return pair[0] === text; })
        .map(function (pair) { return pair[1]; });
};
URLSearchParams.prototype.has = function (name) {
    return this.get(name) !== null;
};
URLSearchParams.prototype.delete = function (name) {
    var text = String(name);
    this._pairs = this._pairs.filter(function (pair) { return pair[0] !== text; });
};
URLSearchParams.prototype.forEach = function (visit, bound) {
    for (var index = 0; index < this._pairs.length; index += 1) {
        visit.call(bound, this._pairs[index][1], this._pairs[index][0], this);
    }
};
URLSearchParams.prototype.keys = function () {
    return this._pairs.map(function (pair) { return pair[0]; })[Symbol.iterator]();
};
URLSearchParams.prototype.values = function () {
    return this._pairs.map(function (pair) { return pair[1]; })[Symbol.iterator]();
};
URLSearchParams.prototype.entries = function () {
    return this._pairs.map(function (pair) { return pair.slice(); })[Symbol.iterator]();
};
URLSearchParams.prototype[Symbol.iterator] = URLSearchParams.prototype.entries;
URLSearchParams.prototype.toString = function () {
    return this._pairs
        .map(function (pair) {
            return querystring.escape(pair[0]) + '=' + querystring.escape(pair[1]);
        })
        .join('&');
};
Object.defineProperty(URLSearchParams.prototype, 'size', {
    get: function () { return this._pairs.length; }
});

// The generic form of an absolute URL: scheme, optional authority, path, query, fragment.
var ABSOLUTE = /^([A-Za-z][A-Za-z0-9+.-]*):(\/\/([^/?#]*))?([^?#]*)(\?[^#]*)?(#.*)?$/;

function URL(input, base) {
    var text = String(input);
    var matched = ABSOLUTE.exec(text);
    if (matched === null) {
        if (base === undefined || base === null) {
            var error = new TypeError('Invalid URL');
            error.code = 'ERR_INVALID_URL';
            error.input = text;
            throw error;
        }
        var parent = base instanceof URL ? base : new URL(base);
        var joined = text.charAt(0) === '/'
            ? text
            : parent.pathname.replace(/[^/]*$/, '') + text;
        return new URL(parent.protocol + '//' + parent.host + joined);
    }
    var authority = matched[3] === undefined ? '' : matched[3];
    var credentials = authority.indexOf('@') < 0 ? '' : authority.slice(0, authority.indexOf('@'));
    var host = authority.indexOf('@') < 0 ? authority : authority.slice(authority.indexOf('@') + 1);
    var colon = host.lastIndexOf(':');
    this.protocol = matched[1].toLowerCase() + ':';
    this.username = credentials.indexOf(':') < 0 ? credentials : credentials.slice(0, credentials.indexOf(':'));
    this.password = credentials.indexOf(':') < 0 ? '' : credentials.slice(credentials.indexOf(':') + 1);
    this.hostname = colon > 0 ? host.slice(0, colon) : host;
    this.port = colon > 0 ? host.slice(colon + 1) : '';
    this.host = host;
    this.pathname = matched[4] === undefined || matched[4] === '' ? (authority === '' ? '' : '/') : matched[4];
    this.search = matched[5] === undefined ? '' : matched[5];
    this.hash = matched[6] === undefined ? '' : matched[6];
    this.searchParams = new URLSearchParams(this.search);
    this.origin = authority === '' ? 'null' : this.protocol + '//' + host;
    this.href = this.toString();
}

URL.prototype.toString = function () {
    var credentials = this.username === '' ? '' : this.username
        + (this.password === '' ? '' : ':' + this.password) + '@';
    var authority = this.host === '' ? '' : '//' + credentials + this.host;
    var query = this.searchParams.toString();
    return this.protocol + authority + this.pathname
        + (query === '' ? '' : '?' + query) + this.hash;
};
URL.prototype.toJSON = function () { return this.toString(); };

function fileURLToPath(url) {
    var held = url instanceof URL ? url : new URL(String(url));
    if (held.protocol !== 'file:') {
        var error = new TypeError('The URL must be of scheme file');
        error.code = 'ERR_INVALID_URL_SCHEME';
        throw error;
    }
    return decodeURIComponent(held.pathname);
}

function pathToFileURL(path) {
    var text = String(path);
    var absolute = text.charAt(0) === '/' ? text : '/' + text;
    return new URL('file://' + absolute.split('/').map(encodeURIComponent).join('/'));
}

exports.URL = URL;
exports.URLSearchParams = URLSearchParams;
exports.fileURLToPath = fileURLToPath;
exports.pathToFileURL = pathToFileURL;
exports.parse = function (text) {
    var held = new URL(String(text));
    return {
        protocol: held.protocol,
        host: held.host,
        hostname: held.hostname,
        port: held.port === '' ? null : held.port,
        pathname: held.pathname,
        search: held.search === '' ? null : held.search,
        query: held.search === '' ? null : held.search.slice(1),
        hash: held.hash === '' ? null : held.hash,
        href: held.href,
        path: held.pathname + held.search
    };
};
exports.format = function (held) {
    return held instanceof URL ? held.toString() : String(held.href);
};
exports.resolve = function (base, target) {
    return new URL(String(target), String(base)).toString();
};
"##;

/// The MODULE REGISTRY a guest's own files are resolved and loaded through: Node's module
/// resolution over the files of a package held in memory, and nothing else.
///
/// It is the source of a CommonJS module exporting one function, `create(options)`, which
/// answers with the registry a consumer's runtime drives:
///
/// * `options.files` — the package's files, keyed by their path inside it;
/// * `options.core` — the core modules the consumer mounts, keyed by the name a program
///   imports them under: the SOURCE of a CommonJS module (as [`PURE_CORE_MODULES`] supplies
///   it) or a live object the consumer's own execution environment answers with;
/// * `options.accessor` — the JavaScript expression a generated ES module reads a loaded
///   CommonJS module back from, called with the resolved path (for example
///   `globalThis.__myRuntime.module`).
///
/// The registry it answers with:
///
/// * `resolve(base, specifier)` — the path the specifier resolves to from the module `base`,
///   `"core:<name>"` for a core module, or `null` for a specifier that resolves to nothing.
///   The rule is Node's own: a `node:` prefix is stripped, a relative specifier is joined
///   onto the importing module's directory and tried under the extensions and the
///   `index` files Node tries, and a bare specifier is a core module or a dependency the
///   package BUNDLED under `node_modules` (that package's own `main`, then its `index`);
/// * `load(path)` — the module at a resolved path, compiled once and cached: a CommonJS file
///   through the module scope Node gives one (`exports`, `require`, `module`, `__filename`,
///   `__dirname`), a JSON file parsed, and an ES module refused with Node's own
///   `ERR_REQUIRE_ESM`;
/// * `requireFrom(base)` — the `require` a module at `base` is given;
/// * `source(path)` — the source an ENGINE's own module loader compiles for a resolved path:
///   the file itself where it is an ES module, and a generated re-export of the loaded
///   CommonJS module (or of the parsed JSON) where it is not;
/// * `isModule(path)` — whether the file at a path is an ES module, by Node's rule: `.mjs`
///   is, `.cjs` and `.json` are not, and `.js` is whichever the package's `"type"` names;
/// * `entry()` — the module Node would start the package on: its `package.json` `main`, then
///   its `index`, with the source and the kind of what was found.
pub const MODULE_REGISTRY_SOURCE: &str = r##"
'use strict';

// The words that cannot be a generated export's name.
var RESERVED = {
    'default': true, 'class': true, 'function': true, 'var': true, 'let': true,
    'const': true, 'new': true, 'delete': true, 'in': true, 'instanceof': true,
    'typeof': true, 'void': true, 'return': true, 'if': true, 'else': true, 'do': true,
    'while': true, 'for': true, 'switch': true, 'case': true, 'break': true,
    'continue': true, 'try': true, 'catch': true, 'finally': true, 'throw': true,
    'this': true, 'super': true, 'extends': true, 'export': true, 'import': true,
    'yield': true, 'await': true, 'enum': true, 'null': true, 'true': true,
    'false': true, 'with': true, 'debugger': true
};

exports.create = function (options) {
    var held = options || {};
    var files = held.files || {};
    var core = held.core || {};
    var accessor = String(held.accessor || '');
    var root = String(held.taskRoot || '/var/task');
    var manifest = {};
    try {
        manifest = JSON.parse(files['package.json'] || '{}') || {};
    } catch (error) {
        manifest = {};
    }
    var loaded = {};

    function has(path) {
        return Object.prototype.hasOwnProperty.call(files, path);
    }
    function hasCore(name) {
        return Object.prototype.hasOwnProperty.call(core, name);
    }
    function isModule(path) {
        if (/\.mjs$/.test(path)) { return true; }
        if (/\.cjs$/.test(path) || /\.json$/.test(path)) { return false; }
        return manifest.type === 'module';
    }
    function candidates(path) {
        return [path, path + '.js', path + '.mjs', path + '.cjs', path + '.json',
                path + '/index.js', path + '/index.mjs', path + '/index.cjs'];
    }
    function firstHeld(path) {
        var tried = candidates(path);
        for (var index = 0; index < tried.length; index += 1) {
            if (has(tried[index])) { return tried[index]; }
        }
        return null;
    }
    function joinFrom(base, name) {
        var directory = base.indexOf('/') < 0 ? '' : base.slice(0, base.lastIndexOf('/'));
        var parts = directory === '' ? [] : directory.split('/');
        name.split('/').forEach(function (segment) {
            if (segment === '.' || segment === '') { return; }
            if (segment === '..') { parts.pop(); return; }
            parts.push(segment);
        });
        return parts.join('/');
    }
    function resolve(base, name) {
        var request = String(name).replace(/^node:/, '');
        if (request === '') { return null; }
        if (request.charAt(0) === '.' || request.charAt(0) === '/') {
            var from = String(base).indexOf('core:') === 0 ? '' : String(base);
            return firstHeld(joinFrom(from, request.replace(/^\//, '')));
        }
        if (hasCore(request)) { return 'core:' + request; }
        // A dependency the package BUNDLED, resolved the way Node resolves one: its
        // `node_modules` directory, that package's own `main`, then its `index`.
        var bundled = 'node_modules/' + request;
        var main = null;
        if (has(bundled + '/package.json')) {
            try {
                main = JSON.parse(files[bundled + '/package.json']).main;
            } catch (error) {
                main = null;
            }
        }
        if (main) {
            var found = firstHeld(joinFrom(bundled + '/package.json', String(main)));
            if (found !== null) { return found; }
        }
        return firstHeld(bundled);
    }
    function notFound(request) {
        var missing = new Error("Cannot find module '" + String(request) + "'");
        missing.code = 'MODULE_NOT_FOUND';
        return missing;
    }
    function load(path, request) {
        var resolved = String(path);
        if (resolved.indexOf('core:') === 0) {
            var name = resolved.slice('core:'.length);
            if (!hasCore(name)) { throw notFound(request === undefined ? name : request); }
            if (typeof core[name] !== 'string') { return core[name]; }
            if (Object.prototype.hasOwnProperty.call(loaded, resolved)) {
                return loaded[resolved].exports;
            }
            return compile(resolved, core[name], name, '');
        }
        if (!has(resolved)) { throw notFound(request === undefined ? resolved : request); }
        if (Object.prototype.hasOwnProperty.call(loaded, resolved)) {
            return loaded[resolved].exports;
        }
        if (/\.json$/.test(resolved)) {
            loaded[resolved] = { exports: JSON.parse(files[resolved]) };
            return loaded[resolved].exports;
        }
        if (isModule(resolved)) {
            var esm = new Error('require() of ES Module ' + resolved + ' is not supported');
            esm.code = 'ERR_REQUIRE_ESM';
            throw esm;
        }
        var directory = resolved.indexOf('/') < 0 ? '' : resolved.slice(0, resolved.lastIndexOf('/'));
        return compile(resolved, files[resolved], root + '/' + resolved,
                       directory === '' ? root : root + '/' + directory);
    }
    // Compile one CommonJS module through the module scope Node gives one. The record is
    // cached BEFORE the body runs, as Node does, so a cycle sees a partial export rather
    // than running twice.
    function compile(key, source, filename, directory) {
        var record = { exports: {}, id: key, filename: filename, loaded: false };
        loaded[key] = record;
        var compiled = new Function(
            'exports', 'require', 'module', '__filename', '__dirname', source
        );
        compiled.call(record.exports, record.exports, requireFrom(key), record,
                      filename, directory);
        record.loaded = true;
        return record.exports;
    }
    function requireFrom(base) {
        return function (request) {
            var resolved = resolve(base, request);
            if (resolved === null) { throw notFound(request); }
            return load(resolved, request);
        };
    }
    function exportable(value) {
        if (value === null || (typeof value !== 'object' && typeof value !== 'function')) {
            return [];
        }
        var names = [];
        Object.keys(value).forEach(function (name) {
            if (!/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(name)) { return; }
            if (Object.prototype.hasOwnProperty.call(RESERVED, name)) { return; }
            names.push(name);
        });
        return names;
    }
    // The ES module source a generated re-export is: the module is loaded here, as a
    // `require` of it would load it, and what it exported is read back out of the registry
    // through the accessor the consumer gave.
    function reexport(path) {
        var value = load(path, path);
        var source = 'const __held = ' + accessor + '(' + JSON.stringify(path) + ');\n'
            + 'export default __held;\n';
        exportable(value).forEach(function (name) {
            source = source + 'export const ' + name + ' = __held.' + name + ';\n';
        });
        return source;
    }
    function source(path) {
        var resolved = String(path);
        if (resolved.indexOf('core:') === 0) {
            return hasCore(resolved.slice('core:'.length)) ? reexport(resolved) : null;
        }
        if (!has(resolved)) { return null; }
        if (isModule(resolved)) { return files[resolved]; }
        return reexport(resolved);
    }
    function entry() {
        var found = null;
        if (manifest.main) {
            found = firstHeld(joinFrom('package.json', String(manifest.main)));
        }
        if (found === null) { found = firstHeld('index'); }
        if (found === null) { return { entry: '' }; }
        return { entry: found, module: isModule(found), source: files[found] };
    }

    return {
        resolve: resolve,
        load: load,
        requireFrom: requireFrom,
        source: source,
        isModule: isModule,
        entry: entry,
        manifest: function () { return manifest; }
    };
};
"##;

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: every pure core module is reachable under its bare name and under the `node:`
    /// name a modern import spells it with, and nothing else resolves to one.
    #[test]
    fn a_pure_core_module_resolves_under_both_of_its_names() {
        for (name, _) in PURE_CORE_MODULES {
            for spelled in [(*name).to_string(), format!("node:{name}")] {
                assert!(
                    pure_core_module_source(&spelled).is_some(),
                    "case {spelled:?}: expected the core module's source, got none"
                );
            }
        }
        for absent in ["fs", "net", "child_process", "http", "os", "node:fs", "", "node:"] {
            assert!(
                pure_core_module_source(absent).is_none(),
                "case {absent:?}: this module supplies only the PURE core modules, so no \
                 module its consumer's own environment answers for may resolve here"
            );
        }
    }

    /// RULE: each source is the body of a CommonJS module — it assigns what it exports —
    /// because that is the form the registry compiles, and no two names share a source.
    #[test]
    fn every_pure_core_module_is_a_commonjs_module() {
        let mut names: Vec<&str> = Vec::new();
        for (name, source) in PURE_CORE_MODULES {
            assert!(
                source.contains("module.exports") || source.contains("exports."),
                "case {name:?}: expected a CommonJS module that assigns its exports"
            );
            assert!(
                source.contains("'use strict';"),
                "case {name:?}: expected the module to be strict"
            );
            names.push(name);
        }
        names.sort_unstable();
        let mut unique = names.clone();
        unique.dedup();
        assert_eq!(
            unique.len(),
            names.len(),
            "no two core modules may share a name, got {names:?}"
        );
    }

    /// RULE: `url` is built on `querystring`, so a consumer that mounts the catalogue mounts
    /// what each module requires — the catalogue is closed under `require`.
    #[test]
    fn the_catalogue_is_closed_under_require() {
        for (name, source) in PURE_CORE_MODULES {
            for required in source.split("require('").skip(1) {
                let required = required.split('\'').next().unwrap_or_default();
                assert!(
                    pure_core_module_source(required).is_some(),
                    "case {name:?}: it requires {required:?}, which this catalogue does not \
                     hold"
                );
            }
        }
    }

    /// RULE: the registry is the body of a CommonJS module exporting the one factory its
    /// consumers call.
    #[test]
    fn the_registry_exports_its_factory() {
        assert!(
            MODULE_REGISTRY_SOURCE.contains("exports.create = function (options)"),
            "expected the registry to export its factory"
        );
        for answered in [
            "resolve: resolve",
            "load: load",
            "requireFrom: requireFrom",
            "source: source",
            "isModule: isModule",
            "entry: entry",
        ] {
            assert!(
                MODULE_REGISTRY_SOURCE.contains(answered),
                "expected the registry it answers with to carry {answered:?}"
            );
        }
    }
}
