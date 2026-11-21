// SPDX-License-Identifier: CC0-1.0

//! The WHATWG GLOBALS a managed node runtime defines beside the language's own, in the
//! language the handler is written in.
//!
//! A managed runtime is a promise about the global object a handler is evaluated against, not
//! only about the modules it can import: a handler written for `nodejs18.x` or later reads
//! `structuredClone`, the event pair, the error class a web API raises, the performance clock,
//! the fetch types and the byte container off that object without importing anything. This
//! source is those names.
//!
//! Every one of them is COMPUTATION over values the run already holds — a deep copy, an event
//! dispatch, a byte container, a clock reading — with one exception: `fetch` is the ONE request
//! a handler sends itself, carried by the same `http` core module a handler that builds the
//! request by hand uses, so both go through the one seam the host bounds and no second route
//! out of a run exists.
//!
//! It is installed by the runtime BEFORE any module of the deployment package is loaded
//! ([`super::runtime`]'s `installGlobals`), which is where a managed runtime has these names:
//! a module BODY that reads one reads it. The installer is handed the `require` of the runtime
//! itself, so a global and the core module that implements it are the SAME object — a handler
//! that imports `perf_hooks` and one that reads `performance` read one clock — and the
//! installer is taken off the global object as soon as it has run, so no guest code can call
//! it again.

/// The source of the global surface, evaluated once per invocation as a unit of the runtime's
/// own internals.
pub(super) const WEB_GLOBALS_SOURCE: &str = r##"
// The installer the runtime calls with its own `require`, and takes off the global object
// again once it has run.
globalThis.__installWebGlobals = function (require) {
    var Buffer = require('buffer').Buffer;

    // ------------------------------------------------------------------ structured cloning

    // The error a value that cannot be cloned fails with: `DataCloneError`, which is what the
    // platform raises for a function, a symbol or anything else with no serialisation.
    function notCloneable(value) {
        var DOMException = globalThis.DOMException;
        var described = typeof value === 'function'
            ? String(value)
            : Object.prototype.toString.call(value);
        return new DOMException(described + ' could not be cloned.', 'DataCloneError');
    }

    // "Creates a deep clone of a given value using the structured clone algorithm": the
    // objects the algorithm states a serialisation for are copied by their own shape, a
    // reference seen twice is the same object in the copy — which is what makes a cycle a
    // cycle rather than an unbounded walk — and a value the algorithm states no
    // serialisation for is refused.
    function clone(value, seen) {
        if (value === null || typeof value !== 'object') {
            if (typeof value === 'function' || typeof value === 'symbol') {
                throw notCloneable(value);
            }
            return value;
        }
        var held = seen.get(value);
        if (held !== undefined) {
            return held;
        }
        if (value instanceof Date) {
            return new Date(value.getTime());
        }
        if (value instanceof RegExp) {
            return new RegExp(value.source, value.flags);
        }
        if (typeof ArrayBuffer === 'function' && value instanceof ArrayBuffer) {
            return value.slice(0);
        }
        if (ArrayBuffer.isView(value)) {
            // A view keeps its own kind and its own length over a copy of the bytes it reads.
            var copied = value.buffer.slice(
                value.byteOffset,
                value.byteOffset + value.byteLength
            );
            return value instanceof DataView
                ? new DataView(copied)
                : new value.constructor(copied);
        }
        if (value instanceof Error) {
            var raised = new value.constructor(value.message);
            raised.name = value.name;
            if (value.cause !== undefined) {
                raised.cause = clone(value.cause, seen);
            }
            return raised;
        }
        if (typeof Map === 'function' && value instanceof Map) {
            var map = new Map();
            seen.set(value, map);
            value.forEach(function (entry, key) {
                map.set(clone(key, seen), clone(entry, seen));
            });
            return map;
        }
        if (typeof Set === 'function' && value instanceof Set) {
            var set = new Set();
            seen.set(value, set);
            value.forEach(function (member) {
                set.add(clone(member, seen));
            });
            return set;
        }
        if (Array.isArray(value)) {
            var list = [];
            seen.set(value, list);
            for (var index = 0; index < value.length; index += 1) {
                list[index] = clone(value[index], seen);
            }
            return list;
        }
        // Everything else is copied as a plain object carrying its own enumerable members,
        // which is what the algorithm states for one: the prototype is NOT carried over, so a
        // class instance clones to a plain object exactly as it does on the platform.
        var object = {};
        seen.set(value, object);
        var keys = Object.keys(value);
        for (var key = 0; key < keys.length; key += 1) {
            object[keys[key]] = clone(value[keys[key]], seen);
        }
        return object;
    }

    function structuredClone(value) {
        return clone(value, new Map());
    }

    // ------------------------------------------------------------------ the error class

    // "The DOMException interface represents an abnormal event": the error a web API raises,
    // carrying the name the API named and the legacy code that name stands for.
    var CODES = {
        IndexSizeError: 1,
        HierarchyRequestError: 3,
        WrongDocumentError: 4,
        InvalidCharacterError: 5,
        NoModificationAllowedError: 7,
        NotFoundError: 8,
        NotSupportedError: 9,
        InUseAttributeError: 10,
        InvalidStateError: 11,
        SyntaxError: 12,
        InvalidModificationError: 13,
        NamespaceError: 14,
        InvalidAccessError: 15,
        SecurityError: 18,
        NetworkError: 19,
        AbortError: 20,
        QuotaExceededError: 22,
        TimeoutError: 23,
        InvalidNodeTypeError: 24,
        DataCloneError: 25
    };

    function DOMException(message, name) {
        var error = new Error(message === undefined ? '' : String(message));
        error.name = name === undefined ? 'Error' : String(name);
        Object.setPrototypeOf(error, DOMException.prototype);
        error.code = CODES[error.name] === undefined ? 0 : CODES[error.name];
        return error;
    }
    DOMException.prototype = Object.create(Error.prototype);
    DOMException.prototype.constructor = DOMException;

    // ------------------------------------------------------------------ events

    // "The Event interface represents an event which takes place in the DOM": the object a
    // dispatch carries, with the three members a listener reads off it and the two it sets.
    function Event(type, options) {
        var held = options === null || options === undefined ? {} : options;
        this.type = String(type);
        this.bubbles = held.bubbles === true;
        this.cancelable = held.cancelable === true;
        this.composed = held.composed === true;
        this.defaultPrevented = false;
        this.target = null;
        this.currentTarget = null;
        this.eventPhase = 0;
        this.timeStamp = Date.now();
        this.cancelBubble = false;
        this._stopped = false;
    }
    Event.prototype.preventDefault = function () {
        if (this.cancelable) {
            this.defaultPrevented = true;
        }
    };
    Event.prototype.stopPropagation = function () {
        this._stopped = true;
        this.cancelBubble = true;
    };
    Event.prototype.stopImmediatePropagation = Event.prototype.stopPropagation;
    Event.NONE = 0;
    Event.CAPTURING_PHASE = 1;
    Event.AT_TARGET = 2;
    Event.BUBBLING_PHASE = 3;

    function CustomEvent(type, options) {
        Event.call(this, type, options);
        var held = options === null || options === undefined ? {} : options;
        this.detail = held.detail === undefined ? null : held.detail;
    }
    CustomEvent.prototype = Object.create(Event.prototype);
    CustomEvent.prototype.constructor = CustomEvent;

    // "A MessageEvent object represents a message": the event a port delivers a message as.
    function MessageEvent(type, options) {
        Event.call(this, type, options);
        var held = options === null || options === undefined ? {} : options;
        this.data = held.data === undefined ? null : held.data;
        this.origin = held.origin === undefined ? '' : String(held.origin);
        this.lastEventId = held.lastEventId === undefined ? '' : String(held.lastEventId);
        this.ports = held.ports === undefined ? [] : held.ports;
    }
    MessageEvent.prototype = Object.create(Event.prototype);
    MessageEvent.prototype.constructor = MessageEvent;

    // "EventTarget is a DOM interface implemented by objects that can receive events": the
    // registry one object dispatches to, in the order the listeners were added — a listener
    // added twice for the same type is the one listener it is, and `once` takes it off again
    // as it fires.
    function EventTarget() {
        Object.defineProperty(this, '_registered', {
            value: Object.create(null),
            enumerable: false,
            writable: true
        });
    }
    EventTarget.prototype._listenersOf = function (type) {
        if (!this._registered) {
            Object.defineProperty(this, '_registered', {
                value: Object.create(null),
                enumerable: false,
                writable: true
            });
        }
        if (!this._registered[type]) {
            this._registered[type] = [];
        }
        return this._registered[type];
    };
    EventTarget.prototype.addEventListener = function (type, listener, options) {
        if (listener === null || listener === undefined) {
            return;
        }
        var held = options === true ? { capture: true } : (options || {});
        var listeners = this._listenersOf(String(type));
        for (var index = 0; index < listeners.length; index += 1) {
            if (listeners[index].listener === listener) {
                return;
            }
        }
        listeners.push({ listener: listener, once: held.once === true });
    };
    EventTarget.prototype.removeEventListener = function (type, listener) {
        var listeners = this._listenersOf(String(type));
        for (var index = 0; index < listeners.length; index += 1) {
            if (listeners[index].listener === listener) {
                listeners.splice(index, 1);
                return;
            }
        }
    };
    EventTarget.prototype.dispatchEvent = function (event) {
        var listeners = this._listenersOf(String(event.type)).slice();
        event.target = this;
        event.currentTarget = this;
        for (var index = 0; index < listeners.length; index += 1) {
            if (listeners[index].once) {
                this.removeEventListener(event.type, listeners[index].listener);
            }
            var listener = listeners[index].listener;
            if (typeof listener === 'function') {
                listener.call(this, event);
            } else if (typeof listener.handleEvent === 'function') {
                listener.handleEvent(event);
            }
            if (event._stopped) {
                break;
            }
        }
        return !event.defaultPrevented;
    };

    // ------------------------------------------------------------------ the byte container

    // "A Blob encapsulates immutable, raw data": the bytes its parts hold, in the order they
    // were given, with the type it was declared with.
    function blobBytes(parts) {
        var chunks = [];
        var held = parts === null || parts === undefined ? [] : parts;
        for (var index = 0; index < held.length; index += 1) {
            var part = held[index];
            if (part instanceof Blob) {
                chunks.push(part._bytes);
            } else if (ArrayBuffer.isView(part)) {
                chunks.push(Buffer.from(part.buffer, part.byteOffset, part.byteLength));
            } else if (typeof ArrayBuffer === 'function' && part instanceof ArrayBuffer) {
                chunks.push(Buffer.from(part));
            } else {
                chunks.push(Buffer.from(String(part), 'utf8'));
            }
        }
        return Buffer.concat(chunks);
    }

    function Blob(parts, options) {
        var held = options === null || options === undefined ? {} : options;
        this._bytes = blobBytes(parts);
        this.size = this._bytes.length;
        this.type = held.type === undefined ? '' : String(held.type).toLowerCase();
    }
    Blob.prototype.arrayBuffer = function () {
        var bytes = this._bytes;
        return Promise.resolve(bytes.buffer.slice(
            bytes.byteOffset,
            bytes.byteOffset + bytes.byteLength
        ));
    };
    Blob.prototype.bytes = function () {
        return Promise.resolve(new Uint8Array(this._bytes));
    };
    Blob.prototype.text = function () {
        return Promise.resolve(this._bytes.toString('utf8'));
    };
    Blob.prototype.slice = function (start, end, type) {
        return new Blob([this._bytes.slice(start, end)], { type: type });
    };

    // "A File object is a Blob object with a name attribute".
    function File(parts, name, options) {
        Blob.call(this, parts, options);
        var held = options === null || options === undefined ? {} : options;
        this.name = String(name);
        this.lastModified = held.lastModified === undefined
            ? Date.now()
            : Number(held.lastModified);
    }
    File.prototype = Object.create(Blob.prototype);
    File.prototype.constructor = File;

    // ------------------------------------------------------------------ the fetch types

    // "The Headers interface ... allows you to perform various actions on HTTP request and
    // response headers": one multi-map keyed by the lower-cased name, which is how a header
    // name compares.
    function Headers(init) {
        this._held = [];
        if (init === null || init === undefined) {
            return;
        }
        if (init instanceof Headers) {
            this._held = init._held.slice();
            return;
        }
        if (Array.isArray(init)) {
            for (var index = 0; index < init.length; index += 1) {
                this.append(init[index][0], init[index][1]);
            }
            return;
        }
        var keys = Object.keys(init);
        for (var key = 0; key < keys.length; key += 1) {
            this.append(keys[key], init[keys[key]]);
        }
    }
    Headers.prototype.append = function (name, value) {
        this._held.push([String(name).toLowerCase(), String(value)]);
    };
    Headers.prototype.set = function (name, value) {
        this.delete(name);
        this.append(name, value);
    };
    Headers.prototype.delete = function (name) {
        var named = String(name).toLowerCase();
        this._held = this._held.filter(function (pair) { return pair[0] !== named; });
    };
    Headers.prototype.get = function (name) {
        var named = String(name).toLowerCase();
        var found = this._held.filter(function (pair) { return pair[0] === named; });
        // "returns … all the values … joined by ', '".
        return found.length === 0 ? null : found.map(function (pair) {
            return pair[1];
        }).join(', ');
    };
    Headers.prototype.getSetCookie = function () {
        return this._held.filter(function (pair) {
            return pair[0] === 'set-cookie';
        }).map(function (pair) { return pair[1]; });
    };
    Headers.prototype.has = function (name) {
        return this.get(name) !== null;
    };
    Headers.prototype.forEach = function (visit, bound) {
        var names = [];
        var self = this;
        this._held.forEach(function (pair) {
            if (names.indexOf(pair[0]) < 0) {
                names.push(pair[0]);
            }
        });
        names.sort();
        names.forEach(function (name) {
            visit.call(bound, self.get(name), name, self);
        });
    };
    Headers.prototype.entries = function () {
        var out = [];
        this.forEach(function (value, name) { out.push([name, value]); });
        return out[Symbol.iterator]();
    };
    Headers.prototype.keys = function () {
        var out = [];
        this.forEach(function (value, name) { out.push(name); });
        return out[Symbol.iterator]();
    };
    Headers.prototype.values = function () {
        var out = [];
        this.forEach(function (value) { out.push(value); });
        return out[Symbol.iterator]();
    };
    Headers.prototype[Symbol.iterator] = Headers.prototype.entries;

    // The body of a request or a response, as the four readings a program takes it in.
    function installBody(target) {
        target.prototype.arrayBuffer = function () {
            var bytes = this._body;
            return Promise.resolve(bytes.buffer.slice(
                bytes.byteOffset,
                bytes.byteOffset + bytes.byteLength
            ));
        };
        target.prototype.bytes = function () {
            return Promise.resolve(new Uint8Array(this._body));
        };
        target.prototype.text = function () {
            return Promise.resolve(this._body.toString('utf8'));
        };
        target.prototype.json = function () {
            var text = this._body.toString('utf8');
            try {
                return Promise.resolve(JSON.parse(text));
            } catch (error) {
                return Promise.reject(error);
            }
        };
        target.prototype.blob = function () {
            return Promise.resolve(new Blob([this._body], {
                type: this.headers.get('content-type') || ''
            }));
        };
    }

    // The bytes a body of any of the forms a caller may pass holds.
    function bodyBytes(body) {
        if (body === null || body === undefined) {
            return Buffer.alloc(0);
        }
        if (body instanceof Blob) {
            return body._bytes;
        }
        if (Buffer.isBuffer(body)) {
            return body;
        }
        if (ArrayBuffer.isView(body)) {
            return Buffer.from(body.buffer, body.byteOffset, body.byteLength);
        }
        if (typeof ArrayBuffer === 'function' && body instanceof ArrayBuffer) {
            return Buffer.from(body);
        }
        if (typeof URLSearchParams === 'function' && body instanceof URLSearchParams) {
            return Buffer.from(body.toString(), 'utf8');
        }
        return Buffer.from(String(body), 'utf8');
    }

    function Request(input, init) {
        var held = init === null || init === undefined ? {} : init;
        var from = input instanceof Request ? input : null;
        this.url = from === null ? String(input) : from.url;
        this.method = String(
            held.method === undefined ? (from === null ? 'GET' : from.method) : held.method
        ).toUpperCase();
        this.headers = new Headers(
            held.headers === undefined ? (from === null ? undefined : from.headers) : held.headers
        );
        this.signal = held.signal === undefined ? null : held.signal;
        this.redirect = held.redirect === undefined ? 'follow' : String(held.redirect);
        this.bodyUsed = false;
        this._body = held.body === undefined
            ? (from === null ? Buffer.alloc(0) : from._body)
            : bodyBytes(held.body);
    }
    installBody(Request);
    Request.prototype.clone = function () {
        return new Request(this);
    };

    function Response(body, init) {
        var held = init === null || init === undefined ? {} : init;
        this.status = held.status === undefined ? 200 : Number(held.status);
        this.statusText = held.statusText === undefined ? '' : String(held.statusText);
        this.headers = new Headers(held.headers);
        this.url = held.url === undefined ? '' : String(held.url);
        // "ok … whether the response was successful (status in the range 200 – 299)".
        this.ok = this.status >= 200 && this.status <= 299;
        this.redirected = false;
        this.type = 'basic';
        this.bodyUsed = false;
        this._body = bodyBytes(body);
    }
    installBody(Response);
    Response.prototype.clone = function () {
        return new Response(this._body, {
            status: this.status,
            statusText: this.statusText,
            headers: this.headers,
            url: this.url
        });
    };
    Response.json = function (value, init) {
        var response = new Response(JSON.stringify(value), init);
        if (!response.headers.has('content-type')) {
            response.headers.set('content-type', 'application/json');
        }
        return response;
    };
    Response.error = function () {
        var response = new Response(null, { status: 0 });
        response.type = 'error';
        return response;
    };

    // "The FormData interface provides a way to construct a set of key/value pairs": the same
    // multi-map the headers are, without the name folding.
    function FormData() {
        this._held = [];
    }
    FormData.prototype.append = function (name, value, filename) {
        this._held.push([
            String(name),
            value instanceof Blob && filename !== undefined
                ? new File([value], filename, { type: value.type })
                : value
        ]);
    };
    FormData.prototype.set = function (name, value, filename) {
        this.delete(name);
        this.append(name, value, filename);
    };
    FormData.prototype.delete = function (name) {
        var named = String(name);
        this._held = this._held.filter(function (pair) { return pair[0] !== named; });
    };
    FormData.prototype.get = function (name) {
        var named = String(name);
        for (var index = 0; index < this._held.length; index += 1) {
            if (this._held[index][0] === named) {
                return this._held[index][1];
            }
        }
        return null;
    };
    FormData.prototype.getAll = function (name) {
        var named = String(name);
        return this._held.filter(function (pair) {
            return pair[0] === named;
        }).map(function (pair) { return pair[1]; });
    };
    FormData.prototype.has = function (name) {
        return this.get(name) !== null;
    };
    FormData.prototype.forEach = function (visit, bound) {
        var self = this;
        this._held.forEach(function (pair) { visit.call(bound, pair[1], pair[0], self); });
    };
    FormData.prototype.entries = function () {
        return this._held.map(function (pair) { return pair.slice(); })[Symbol.iterator]();
    };
    FormData.prototype[Symbol.iterator] = FormData.prototype.entries;

    // `fetch`: ONE request, sent by the very client a handler that builds the request by hand
    // uses — the `http`/`https` core module, whose single seam into the host the host itself
    // bounds. Nothing here opens a connection of its own.
    function fetch(resource, init) {
        var request;
        try {
            request = new Request(resource, init);
        } catch (error) {
            return Promise.reject(error);
        }
        return new Promise(function (resolve, reject) {
            var failed = function (error) {
                // "fetch() … rejects with a TypeError" for a request that could not be made,
                // carrying the failure that stopped it as its cause.
                var failure = new TypeError('fetch failed');
                failure.cause = error;
                reject(failure);
            };
            var address;
            try {
                address = new URL(request.url);
            } catch (error) {
                reject(new TypeError('Failed to parse URL from ' + request.url));
                return;
            }
            var client = address.protocol === 'https:' ? require('https') : require('http');
            var headers = [];
            request.headers.forEach(function (value, name) { headers.push([name, value]); });
            var sent;
            try {
                sent = client.request(request.url, {
                    method: request.method,
                    headers: headers
                }, function (incoming) {
                    var chunks = [];
                    incoming.on('data', function (chunk) { chunks.push(chunk); });
                    incoming.on('error', failed);
                    incoming.on('end', function () {
                        var pairs = [];
                        var held = incoming.rawHeaders || [];
                        for (var index = 0; index + 1 < held.length; index += 2) {
                            pairs.push([held[index], held[index + 1]]);
                        }
                        resolve(new Response(Buffer.concat(chunks), {
                            status: incoming.statusCode,
                            statusText: incoming.statusMessage,
                            headers: pairs,
                            url: request.url
                        }));
                    });
                });
            } catch (error) {
                failed(error);
                return;
            }
            sent.on('error', failed);
            if (request._body.length > 0) {
                sent.write(request._body);
            }
            sent.end();
        });
    }

    // ------------------------------------------------------------------ installation

    // What this process reports about itself to code written for a browser: the runtime it is
    // and one execution environment's own concurrency, which is the one invocation it serves.
    function navigatorOf() {
        var version = globalThis.process && globalThis.process.versions
            ? globalThis.process.versions.node
            : '';
        var major = String(version).split('.')[0];
        return Object.freeze({
            userAgent: major === '' ? 'Node.js' : 'Node.js/' + major,
            platform: globalThis.process ? globalThis.process.platform : '',
            hardwareConcurrency: 1,
            language: 'en-US',
            languages: Object.freeze(['en-US'])
        });
    }

    var installed = {
        structuredClone: structuredClone,
        DOMException: DOMException,
        Event: Event,
        CustomEvent: CustomEvent,
        MessageEvent: MessageEvent,
        EventTarget: EventTarget,
        Blob: Blob,
        File: File,
        Headers: Headers,
        Request: Request,
        Response: Response,
        FormData: FormData,
        fetch: fetch,
        navigator: navigatorOf()
    };
    var names = Object.keys(installed);
    for (var index = 0; index < names.length; index += 1) {
        // A name the engine already defines is the engine's own and is left as it is: the
        // runtime completes the global surface rather than replacing what the language states.
        if (globalThis[names[index]] === undefined) {
            globalThis[names[index]] = installed[names[index]];
        }
    }
    // "In browsers, the top-level scope is the global scope … In Node.js this is different":
    // `global` is that object, and it is this one.
    if (globalThis.global === undefined) {
        globalThis.global = globalThis;
    }
};
"##;
