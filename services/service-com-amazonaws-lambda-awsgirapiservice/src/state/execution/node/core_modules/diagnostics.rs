// SPDX-License-Identifier: CC0-1.0

//! `perf_hooks`, `async_hooks` and `v8`: the three modules a Node program observes its own
//! execution through.
//!
//! Each of them is COMPUTATION over what the run already holds — the clock the runtime reads,
//! a store a call is made under, the serialisation of a value — and none of them reaches a host
//! file, socket or environment.
//!
//! What is stated here is only what this execution environment can answer for:
//!
//! * `performance` is the clock the run reads, with the mark and measure entries a program
//!   records on it. The same object is the runtime's `performance` global, so a program that
//!   imports the module and one that reads the global time one run against one origin. Its
//!   GRANULARITY is the one reading of time this engine has — the millisecond clock `Date` is
//!   read from — so a measure over work that takes less than a millisecond reports zero rather
//!   than a sub-millisecond figure nothing measured.
//! * `AsyncLocalStorage` is a real store: what `run` was given is what `getStore` answers for
//!   the extent of that call and for every callback made inside it. The asynchronous
//!   RESOURCE events node reports beside it — `createHook`'s `init`/`before`/`after` — are
//!   events this runtime does not report, so a hook registered here is never called; it is
//!   registered all the same, because a program that installs one and never reads it is a
//!   program that runs.
//! * `v8`'s serialisation round-trips a value through this runtime's own encoding of it. The
//!   format V8 itself writes is version-specific and documented as not portable, so what is
//!   stated here is a serialisation whose own reader is this module — which is the one promise
//!   `v8.serialize`/`v8.deserialize` make. A reading of the engine's heap is NOT stated: this
//!   runtime holds no such figure, and nothing here invents one.

/// `perf_hooks`: the performance clock and the entries a program records on it.
pub(super) const PERF_HOOKS_SOURCE: &str = r##"
'use strict';

// The instant this execution environment's clock is read from, which every reading of
// `performance.now()` is an offset into: "the high resolution millisecond timestamp at which
// the current node process began".
var TIME_ORIGIN = Date.now();

// The entries a program recorded, in the order it recorded them.
var entries = [];

function PerformanceEntry(name, type, start, duration, detail) {
    this.name = String(name);
    this.entryType = type;
    this.startTime = start;
    this.duration = duration;
    this.detail = detail === undefined ? null : detail;
}

PerformanceEntry.prototype.toJSON = function () {
    return {
        name: this.name,
        entryType: this.entryType,
        startTime: this.startTime,
        duration: this.duration
    };
};

function now() {
    return Date.now() - TIME_ORIGIN;
}

function markNamed(name) {
    for (var index = entries.length - 1; index >= 0; index -= 1) {
        if (entries[index].entryType === 'mark' && entries[index].name === name) {
            return entries[index];
        }
    }
    return null;
}

var performance = {
    timeOrigin: TIME_ORIGIN,
    now: now,
    mark: function (name, options) {
        var held = options === null || options === undefined ? {} : options;
        var entry = new PerformanceEntry(
            name,
            'mark',
            held.startTime === undefined ? now() : Number(held.startTime),
            0,
            held.detail
        );
        entries.push(entry);
        return entry;
    },
    // "Creates a new PerformanceMeasure entry": the span between two marks, or between a mark
    // and the instant this call was made.
    measure: function (name, start, end) {
        var from = typeof start === 'string' ? markNamed(start) : null;
        var to = typeof end === 'string' ? markNamed(end) : null;
        var opened = from === null ? 0 : from.startTime;
        var closed = to === null ? now() : to.startTime;
        var entry = new PerformanceEntry(name, 'measure', opened, closed - opened);
        entries.push(entry);
        return entry;
    },
    getEntries: function () {
        return entries.slice();
    },
    getEntriesByName: function (name, type) {
        return entries.filter(function (entry) {
            return entry.name === String(name)
                && (type === undefined || entry.entryType === type);
        });
    },
    getEntriesByType: function (type) {
        return entries.filter(function (entry) { return entry.entryType === type; });
    },
    clearMarks: function (name) {
        entries = entries.filter(function (entry) {
            return entry.entryType !== 'mark'
                || (name !== undefined && entry.name !== String(name));
        });
    },
    clearMeasures: function (name) {
        entries = entries.filter(function (entry) {
            return entry.entryType !== 'measure'
                || (name !== undefined && entry.name !== String(name));
        });
    },
    // "nodeTiming … specific to Node.js": the one span of an execution environment this
    // runtime knows about is its own start, which is this origin.
    nodeTiming: {
        name: 'node',
        entryType: 'node',
        startTime: 0,
        duration: 0,
        nodeStart: 0,
        environment: 0
    },
    toJSON: function () {
        return { timeOrigin: TIME_ORIGIN, nodeTiming: performance.nodeTiming };
    }
};

// "The PerformanceObserver … is notified of new PerformanceEntry instances": the entries
// recorded after it began observing, handed to it as the list node hands one.
function PerformanceObserver(callback) {
    this._callback = callback;
    this._types = [];
    this._from = entries.length;
}

PerformanceObserver.prototype.observe = function (options) {
    var held = options === null || options === undefined ? {} : options;
    this._types = held.entryTypes === undefined
        ? (held.type === undefined ? [] : [held.type])
        : held.entryTypes;
    this._from = entries.length;
    var self = this;
    // The entries are reported as a job of the run's own queue, which is where a program
    // written against this interface reads them.
    Promise.resolve().then(function () {
        var taken = self.takeRecords();
        if (taken.length > 0 && typeof self._callback === 'function') {
            self._callback({
                getEntries: function () { return taken.slice(); },
                getEntriesByName: function (name) {
                    return taken.filter(function (entry) { return entry.name === name; });
                },
                getEntriesByType: function (type) {
                    return taken.filter(function (entry) { return entry.entryType === type; });
                }
            }, self);
        }
    });
};

PerformanceObserver.prototype.takeRecords = function () {
    var self = this;
    var taken = entries.slice(this._from).filter(function (entry) {
        return self._types.length === 0 || self._types.indexOf(entry.entryType) >= 0;
    });
    this._from = entries.length;
    return taken;
};

PerformanceObserver.prototype.disconnect = function () {
    this._types = [];
};

PerformanceObserver.supportedEntryTypes = Object.freeze(['mark', 'measure']);

module.exports = {
    performance: performance,
    PerformanceEntry: PerformanceEntry,
    PerformanceObserver: PerformanceObserver,
    constants: {
        NODE_PERFORMANCE_GC_MAJOR: 4,
        NODE_PERFORMANCE_GC_MINOR: 1,
        NODE_PERFORMANCE_GC_INCREMENTAL: 8,
        NODE_PERFORMANCE_GC_WEAKCB: 16
    }
};
"##;

/// `async_hooks`: the store a call is made under, and the hook registry beside it.
pub(super) const ASYNC_HOOKS_SOURCE: &str = r##"
'use strict';

// The id of the "asynchronous resource" one call is inside. An execution environment of this
// emulator runs one invocation at a time, so what a program reads here is the one id of the
// work it is doing, and a store entered inside it nests under it.
var nextId = 1;
var depth = [];

// "This class creates stores that stay coherent through asynchronous operations": the stack of
// stores one storage holds, innermost last, so a nested `run` sees its own store and the outer
// one sees the outer store again when it returns.
function AsyncLocalStorage() {
    this._stores = [];
    this._disabled = false;
}

AsyncLocalStorage.prototype.run = function (store, callback) {
    if (typeof callback !== 'function') {
        throw new TypeError('The "callback" argument must be of type function.');
    }
    var args = Array.prototype.slice.call(arguments, 2);
    this._stores.push(store);
    nextId += 1;
    depth.push(nextId);
    try {
        return callback.apply(undefined, args);
    } finally {
        this._stores.pop();
        depth.pop();
    }
};

// "Runs a function synchronously outside of a context": the stores are set aside for the
// extent of the call and restored afterwards.
AsyncLocalStorage.prototype.exit = function (callback) {
    var args = Array.prototype.slice.call(arguments, 1);
    var held = this._stores;
    this._stores = [];
    try {
        return callback.apply(undefined, args);
    } finally {
        this._stores = held;
    }
};

AsyncLocalStorage.prototype.getStore = function () {
    if (this._disabled || this._stores.length === 0) {
        return undefined;
    }
    return this._stores[this._stores.length - 1];
};

// "Transitions into the context for the remainder of the current synchronous execution and
// then persists the store through any following asynchronous calls".
AsyncLocalStorage.prototype.enterWith = function (store) {
    this._stores.push(store);
};

AsyncLocalStorage.prototype.disable = function () {
    this._disabled = true;
    this._stores = [];
};

AsyncLocalStorage.bind = function (callback) {
    return callback;
};

AsyncLocalStorage.snapshot = function () {
    return function (callback) {
        var args = Array.prototype.slice.call(arguments, 1);
        return callback.apply(undefined, args);
    };
};

// "The AsyncResource class is designed to be extended by the embedder's async resources":
// what it carries is its own id and the type it was created with; the lifecycle events a hook
// would be told about are events this runtime does not report (see this module's docs).
function AsyncResource(type) {
    nextId += 1;
    this._type = String(type);
    this._id = nextId;
}

AsyncResource.prototype.asyncId = function () { return this._id; };
AsyncResource.prototype.triggerAsyncId = function () {
    return depth.length === 0 ? 1 : depth[depth.length - 1];
};
AsyncResource.prototype.runInAsyncScope = function (callback, bound) {
    var args = Array.prototype.slice.call(arguments, 2);
    return callback.apply(bound, args);
};
AsyncResource.prototype.emitDestroy = function () { return this; };
AsyncResource.prototype.bind = function (callback) { return callback; };

module.exports = {
    AsyncLocalStorage: AsyncLocalStorage,
    AsyncResource: AsyncResource,
    executionAsyncId: function () {
        return depth.length === 0 ? 1 : depth[depth.length - 1];
    },
    triggerAsyncId: function () {
        return depth.length < 2 ? 0 : depth[depth.length - 2];
    },
    executionAsyncResource: function () {
        return {};
    },
    // The hook is registered and never called: this runtime reports no asynchronous resource
    // lifecycle events (see this module's docs), and a hook is never handed an event that did
    // not happen.
    createHook: function () {
        return {
            enable: function () { return this; },
            disable: function () { return this; }
        };
    }
};
"##;

/// `v8`: the serialisation of a value, and the settings a program states on the engine.
pub(super) const V8_SOURCE: &str = r##"
'use strict';

var Buffer = require('buffer').Buffer;

// The tag this runtime's own serialisation opens with, and the version beside it: what
// `deserialize` reads to know it is being handed something this module wrote.
var TAG = 'nodejs-lambda-v8';
var VERSION = 1;

// The values the encoding states a form for. A value no form is stated for is refused, so
// nothing is answered for a value that was not serialised.
function encoded(value, seen) {
    if (value === undefined) {
        return { kind: 'undefined' };
    }
    if (value === null || typeof value === 'boolean' || typeof value === 'string') {
        return { kind: 'value', value: value };
    }
    if (typeof value === 'number') {
        // The three numbers JSON states no form for are stated by name.
        if (Number.isNaN(value)) {
            return { kind: 'nan' };
        }
        if (value === Infinity || value === -Infinity) {
            return { kind: 'infinity', negative: value < 0 };
        }
        return { kind: 'value', value: value };
    }
    if (typeof value === 'bigint') {
        return { kind: 'bigint', value: String(value) };
    }
    if (typeof value === 'function' || typeof value === 'symbol') {
        var error = new Error(String(value) + ' could not be cloned.');
        error.code = 'ERR_INVALID_ARG_TYPE';
        throw error;
    }
    if (seen.indexOf(value) >= 0) {
        var cyclic = new Error('Circular reference cannot be serialized.');
        cyclic.code = 'ERR_INVALID_ARG_VALUE';
        throw cyclic;
    }
    var held = seen.concat([value]);
    if (value instanceof Date) {
        return { kind: 'date', value: value.getTime() };
    }
    if (value instanceof RegExp) {
        return { kind: 'regexp', source: value.source, flags: value.flags };
    }
    if (Buffer.isBuffer(value)) {
        return { kind: 'buffer', value: value.toString('base64') };
    }
    if (ArrayBuffer.isView(value)) {
        return {
            kind: 'view',
            view: value.constructor.name,
            value: Buffer.from(value.buffer, value.byteOffset, value.byteLength)
                .toString('base64')
        };
    }
    if (typeof ArrayBuffer === 'function' && value instanceof ArrayBuffer) {
        return { kind: 'arraybuffer', value: Buffer.from(value).toString('base64') };
    }
    if (Array.isArray(value)) {
        return {
            kind: 'array',
            value: value.map(function (member) { return encoded(member, held); })
        };
    }
    if (typeof Map === 'function' && value instanceof Map) {
        var pairs = [];
        value.forEach(function (entry, key) {
            pairs.push([encoded(key, held), encoded(entry, held)]);
        });
        return { kind: 'map', value: pairs };
    }
    if (typeof Set === 'function' && value instanceof Set) {
        var members = [];
        value.forEach(function (member) { members.push(encoded(member, held)); });
        return { kind: 'set', value: members };
    }
    var keys = Object.keys(value);
    var object = {};
    for (var index = 0; index < keys.length; index += 1) {
        object[keys[index]] = encoded(value[keys[index]], held);
    }
    return { kind: 'object', value: object };
}

function decoded(held) {
    if (held === null || typeof held !== 'object') {
        return undefined;
    }
    if (held.kind === 'undefined') {
        return undefined;
    }
    if (held.kind === 'value') {
        return held.value;
    }
    if (held.kind === 'nan') {
        return NaN;
    }
    if (held.kind === 'infinity') {
        return held.negative ? -Infinity : Infinity;
    }
    if (held.kind === 'bigint') {
        return BigInt(held.value);
    }
    if (held.kind === 'date') {
        return new Date(held.value);
    }
    if (held.kind === 'regexp') {
        return new RegExp(held.source, held.flags);
    }
    if (held.kind === 'buffer') {
        return Buffer.from(held.value, 'base64');
    }
    if (held.kind === 'arraybuffer') {
        var bytes = Buffer.from(held.value, 'base64');
        return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    }
    if (held.kind === 'view') {
        var read = Buffer.from(held.value, 'base64');
        var copied = read.buffer.slice(read.byteOffset, read.byteOffset + read.byteLength);
        var kind = globalThis[held.view];
        return typeof kind === 'function' ? new kind(copied) : new Uint8Array(copied);
    }
    if (held.kind === 'array') {
        return held.value.map(decoded);
    }
    if (held.kind === 'map') {
        var map = new Map();
        for (var pair = 0; pair < held.value.length; pair += 1) {
            map.set(decoded(held.value[pair][0]), decoded(held.value[pair][1]));
        }
        return map;
    }
    if (held.kind === 'set') {
        var set = new Set();
        for (var member = 0; member < held.value.length; member += 1) {
            set.add(decoded(held.value[member]));
        }
        return set;
    }
    var keys = Object.keys(held.value);
    var object = {};
    for (var index = 0; index < keys.length; index += 1) {
        object[keys[index]] = decoded(held.value[keys[index]]);
    }
    return object;
}

function serialize(value) {
    return Buffer.from(JSON.stringify({
        tag: TAG,
        version: VERSION,
        held: encoded(value, [])
    }), 'utf8');
}

function deserialize(bytes) {
    var document;
    try {
        document = JSON.parse(Buffer.from(bytes).toString('utf8'));
    } catch (error) {
        document = null;
    }
    if (document === null || document.tag !== TAG || document.version !== VERSION) {
        var failure = new Error('Unable to deserialize cloned data.');
        failure.code = 'ERR_BUFFER_OUT_OF_BOUNDS';
        throw failure;
    }
    return decoded(document.held);
}

function Serializer() {
    this._held = undefined;
}
Serializer.prototype.writeValue = function (value) {
    this._held = serialize(value);
    return true;
};
Serializer.prototype.releaseBuffer = function () {
    return this._held === undefined ? Buffer.alloc(0) : this._held;
};
Serializer.prototype.writeHeader = function () { return undefined; };

function Deserializer(bytes) {
    this._bytes = bytes;
}
Deserializer.prototype.readHeader = function () { return true; };
Deserializer.prototype.readValue = function () {
    return deserialize(this._bytes);
};

module.exports = {
    serialize: serialize,
    deserialize: deserialize,
    Serializer: Serializer,
    Deserializer: Deserializer,
    DefaultSerializer: Serializer,
    DefaultDeserializer: Deserializer,
    // The engine an invocation runs in takes no flags from guest code: the bounds it runs
    // under are the host's ([`super::super`] module docs), so a flag stated here changes
    // nothing and nothing is reported as changed.
    setFlagsFromString: function () { return undefined; },
    // "Returns statistics about the V8 heap spaces": this runtime reports no heap space of its
    // own, so the list is empty rather than holding a space with invented figures.
    getHeapSpaceStatistics: function () { return []; },
    getHeapCodeStatistics: function () { return {}; },
    takeCoverage: function () { return undefined; },
    stopCoverage: function () { return undefined; },
    cachedDataVersionTag: function () { return VERSION; },
    promiseHooks: {
        onInit: function () { return function () { return undefined; }; },
        onSettled: function () { return function () { return undefined; }; },
        createHook: function () { return function () { return undefined; }; }
    }
};
"##;
