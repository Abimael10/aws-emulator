// SPDX-License-Identifier: CC0-1.0

//! `worker_threads`: the workers a Node program hands part of its work to.
//!
//! A worker's code RUNS. Its entry point — a module of the deployment package, or the source
//! the caller passed — is compiled and evaluated by the same engine the handler runs in, in a
//! module scope of its own with its own module cache ([`super::super::runtime`]'s
//! `freshModule`), and what it posts back is what its own code computed. Nothing here invents
//! a message, an exit code or an error for work nothing ran.
//!
//! What a worker here is NOT is a THREAD of this process. An invocation of this emulator is one
//! engine on one thread, and a second thread running the same engine would escape every bound
//! that engine holds — the deadline, the step ceiling and the allocation ceiling are all the
//! one run's. So a worker is COOPERATIVE: its entry point runs when it is constructed, and
//! every message either side posts is delivered as a job of the run's own queue, with that
//! worker's thread context made current around its own code. The consequences are stated, not
//! hidden:
//!
//! * a worker runs its own code to completion before the thread that started it goes on, so
//!   nothing here makes a program faster — what it makes is a program that uses workers RUN;
//! * two workers never run at the same instant, so a program that waits for one worker to
//!   make progress from inside another makes none, and is ended by the run's own deadline;
//! * `threadId` is this worker's own number in this run, as node's is, and the main thread is
//!   thread 0.
//!
//! The queue, the clone of a posted message and the lifetime of a worker are all this module's;
//! the bounds are the host's ([`super`]), and nothing here can lift one.

/// `worker_threads`: workers, the channel between them, and the ports they hold it by.
pub(super) const WORKER_THREADS_SOURCE: &str = r##"
'use strict';

var EventEmitter = require('events');

// The host half of this module: the one protocol call that compiles a fresh instance of a
// module of this function's own mounts, which is what a worker's entry point is.
function host() {
    return globalThis.__lambdaRuntime;
}

// How many workers one invocation may start. A HOST-side bound would be the engine's own
// step ceiling and deadline, which already end a run that starts workers without end; this
// one is here so that such a program is answered by the error node answers for a worker that
// could not be started, rather than by a run that spends its whole budget starting them.
var MAX_WORKERS = 64;

// The root of the deployment package, which is the only mount a worker's entry point is read
// from, and the path an entry point passed as SOURCE is compiled under — the name node gives
// one, under the directory its own specifiers resolve from.
var TASK_ROOT = '/var/task';
var EVAL_PATH = TASK_ROOT + '/[worker eval]';

// The thread one piece of code is running in: the main thread, or one of the workers started
// from it. It is swapped by this module alone, around the worker's own code — its entry
// point, and every message delivered to it — so the `parentPort`, `workerData` and `threadId`
// that code reads are its own, and the main thread's reading is never a worker's.
var MAIN = { id: 0, port: null, data: null };
var current = MAIN;
var started = 0;

// The deep copy a posted message travels as: "the message is cloned using the structured
// clone algorithm", so neither side can reach the other's objects through a message. The
// algorithm is the runtime's own global.
function cloned(value) {
    return typeof globalThis.structuredClone === 'function'
        ? globalThis.structuredClone(value)
        : JSON.parse(JSON.stringify(value === undefined ? null : value));
}

// ------------------------------------------------------------------ delivery

// Hold `value` for `target` and drain what it holds, as a job of the run's own queue: a
// message is never delivered inside the call that posted it, which is what makes it a message
// rather than a call.
function post(target, value) {
    target._queue.push(value);
    Promise.resolve().then(function () {
        drain(target);
    });
}

// Deliver what `target` holds, in the thread `target` belongs to.
//
// A port delivers nothing until something is listening for it — "messages are queued until a
// 'message' listener is attached" — so a message posted at a port nobody reads stays held, and
// is what `receiveMessageOnPort` takes off it.
function drain(target) {
    while (!target._closed && target._queue.length > 0
        && target.listenerCount('message') > 0) {
        var value = target._queue.shift();
        var held = current;
        current = target._thread;
        try {
            target.emit('message', value);
        } finally {
            current = held;
        }
    }
    if (typeof target._settled === 'function') {
        target._settled();
    }
}

// ------------------------------------------------------------------ the ports

// "Instances of the MessagePort class represent one end of an asynchronous, two-way
// communications channel": what it is posted travels to its PEER, and what its peer is posted
// arrives here.
function MessagePort() {
    EventEmitter.call(this);
    this._queue = [];
    this._peer = null;
    this._closed = false;
    this._thread = MAIN;
}

MessagePort.prototype = Object.create(EventEmitter.prototype);
MessagePort.prototype.constructor = MessagePort;

MessagePort.prototype.postMessage = function (value) {
    if (this._peer === null || this._peer._closed) {
        return;
    }
    post(this._peer, cloned(value));
};

MessagePort.prototype.start = function () {
    var self = this;
    Promise.resolve().then(function () {
        drain(self);
    });
};

MessagePort.prototype.close = function () {
    if (this._closed) {
        return;
    }
    this._closed = true;
    this.emit('close');
    if (this._peer !== null && typeof this._peer._settled === 'function') {
        this._peer._settled();
    }
    if (this._peer !== null && typeof this._settled === 'function') {
        this._settled();
    }
};

MessagePort.prototype.ref = function () {
    return this;
};

MessagePort.prototype.unref = function () {
    return this;
};

// A listener attached for messages is what starts a port, so what it holds is delivered.
MessagePort.prototype.on = function (type, listener) {
    EventEmitter.prototype.on.call(this, type, listener);
    if (type === 'message') {
        this.start();
    }
    return this;
};

MessagePort.prototype.addListener = MessagePort.prototype.on;

MessagePort.prototype.once = function (type, listener) {
    EventEmitter.prototype.once.call(this, type, listener);
    if (type === 'message') {
        this.start();
    }
    return this;
};

// "Instances of the MessageChannel class represent an asynchronous, two-way communications
// channel": two ports, each the other's peer.
function MessageChannel() {
    this.port1 = new MessagePort();
    this.port2 = new MessagePort();
    this.port1._peer = this.port2;
    this.port2._peer = this.port1;
}

// "Receive a single message from a given MessagePort … If no message is available, undefined
// is returned": the message a port holds, taken off it without a listener.
function receiveMessageOnPort(port) {
    if (port === null || port === undefined || !Array.isArray(port._queue)
        || port._queue.length === 0) {
        return undefined;
    }
    return { message: port._queue.shift() };
}

// ------------------------------------------------------------------ the worker

// "The Worker class represents an independent JavaScript execution thread": the object the
// thread that started one holds it by. It is itself the main end of the channel to the worker
// — `worker.postMessage` posts to the worker's `parentPort`, and what that port posts is this
// object's `message` event.
function Worker(specifier, options) {
    EventEmitter.call(this);
    var held = options === null || options === undefined ? {} : options;
    var self = this;
    this._queue = [];
    this._closed = false;
    this._thread = MAIN;
    this._exited = false;
    started += 1;
    if (started > MAX_WORKERS) {
        throw workerFailed('one invocation may start at most ' + MAX_WORKERS + ' workers');
    }
    this.threadId = started;
    // The worker's own end of the channel, and the thread context its code runs in.
    var port = new MessagePort();
    var thread = {
        id: this.threadId,
        port: port,
        data: held.workerData === undefined ? null : cloned(held.workerData)
    };
    port._thread = thread;
    port._peer = this;
    this._peer = port;
    this._inner = port;
    // A worker is done when its own end has nothing left to deliver and nothing listening for
    // more, and the thread that started it has read everything that worker posted — which is
    // what an empty event loop is for a thread whose only input is its port. The same check
    // runs after every delivery in either direction, so `exit` lands after the last `message`
    // rather than in front of it.
    var settled = function () {
        if (port._queue.length === 0 && port.listenerCount('message') === 0
            && self._queue.length === 0) {
            self._finish(0);
        }
    };
    port._settled = settled;
    this._settled = settled;
    this.resourceLimits = {};
    this.stdin = null;
    this.stdout = null;
    this.stderr = null;
    var load;
    try {
        load = held.eval === true
            ? host().freshModule(EVAL_PATH, String(specifier))
            : host().freshModule(pathOf(specifier), null);
    } catch (error) {
        load = null;
    }
    if (load === null || load === undefined) {
        // Nothing was started, and the failure is reported the way node reports a worker whose
        // entry point could not be loaded: on the worker itself, deferred, because the program
        // attaches its handlers to the worker it has just been handed.
        var missing = moduleNotFound(held.eval === true ? EVAL_PATH : String(specifier));
        Promise.resolve().then(function () {
            self._raise(missing);
        });
        return;
    }
    Promise.resolve().then(function () {
        self.emit('online');
        var outer = current;
        current = thread;
        try {
            load();
        } catch (error) {
            current = outer;
            self._raise(error);
            return;
        }
        current = outer;
        // The entry point has run; whether the worker is done now is decided the same way it
        // is after every message — by whether anything is still listening on its own port.
        port._settled();
    });
}

Worker.prototype = Object.create(EventEmitter.prototype);
Worker.prototype.constructor = Worker;

// The path an entry point names, as an absolute path UNDER THE TASK ROOT, and `null` for a
// specifier that names anything else — which is what a worker of this function can be.
//
// The specifier is the guest's own string, so it is normalised and CONTAINED here, before it
// crosses into the host: the segments are collapsed (`.` dropped, `..` applied, so no traversal
// survives), a `file:` URL is read as the path it names and then checked like any other, and a
// path that does not come out under the task root — or that carries a NUL — names no entry
// point at all. The host's own resolution refuses such a path too ([`super::super::runtime`]'s
// `idOfPath`, which answers only the three mounts and collapses every id through one helper);
// this is the same check stated where the guest's string is shaped, so neither side relies on
// the other.
function pathOf(specifier) {
    var named = specifier === null || specifier === undefined ? '' : String(specifier);
    if (named.indexOf('file://') === 0) {
        named = named.slice('file://'.length);
    }
    if (named === '' || named.indexOf('\u0000') >= 0) {
        return null;
    }
    var joined = named.charAt(0) === '/' ? named : TASK_ROOT + '/' + named;
    var segments = joined.split('/');
    var out = [];
    for (var index = 0; index < segments.length; index += 1) {
        var segment = segments[index];
        if (segment === '' || segment === '.') {
            continue;
        }
        if (segment === '..') {
            if (out.length === 0) {
                return null;
            }
            out.pop();
            continue;
        }
        out.push(segment);
    }
    var path = '/' + out.join('/');
    return path.indexOf(TASK_ROOT + '/') === 0 ? path : null;
}

function moduleNotFound(named) {
    var error = new Error("Cannot find module '" + named + "'");
    error.code = 'MODULE_NOT_FOUND';
    return error;
}

function workerFailed(detail) {
    var error = new Error(detail);
    error.code = 'ERR_WORKER_INIT_FAILED';
    return error;
}

Worker.prototype.postMessage = function (value) {
    if (this._exited || this._inner === undefined || this._inner._closed) {
        return;
    }
    post(this._inner, cloned(value));
};

Worker.prototype.on = function (type, listener) {
    EventEmitter.prototype.on.call(this, type, listener);
    if (type === 'message') {
        var self = this;
        Promise.resolve().then(function () {
            drain(self);
        });
    }
    return this;
};

Worker.prototype.addListener = Worker.prototype.on;

Worker.prototype.once = function (type, listener) {
    EventEmitter.prototype.once.call(this, type, listener);
    if (type === 'message') {
        var self = this;
        Promise.resolve().then(function () {
            drain(self);
        });
    }
    return this;
};

// "an uncaught exception … the 'error' event is emitted": the worker's failure is reported on
// it, and the worker is over.
Worker.prototype._raise = function (error) {
    if (this._exited) {
        return;
    }
    this.emit('error', error);
    this._finish(1);
};

// "the 'exit' event is emitted once the worker has stopped", with the exit code: zero for a
// worker whose own event loop emptied, and one for a worker that failed or was terminated.
Worker.prototype._finish = function (code) {
    if (this._exited) {
        return;
    }
    this._exited = true;
    this.emit('exit', code);
};

// "Stop all JavaScript execution in the worker thread as soon as possible": the worker's own
// code runs to completion before the thread that started it goes on ([`WORKER_THREADS_SOURCE`]
// module docs), so what this stops is every message still to be delivered to it.
Worker.prototype.terminate = function () {
    var self = this;
    if (this._inner !== undefined) {
        this._inner._queue = [];
        this._inner._closed = true;
    }
    this._closed = true;
    return new Promise(function (resolve) {
        self._finish(1);
        resolve(1);
    });
};

Worker.prototype.ref = function () {
    return this;
};

Worker.prototype.unref = function () {
    return this;
};

Worker.prototype.getHeapSnapshot = function () {
    var error = new Error('a heap snapshot is not available in this execution environment');
    error.code = 'ERR_NOT_SUPPORTED';
    return Promise.reject(error);
};

// ------------------------------------------------------------------ the thread's own reading

// The environment data a thread hands the threads it starts, which is one map per run.
var environmentData = new Map();

// What a thread reads about ITSELF is read at the moment it asks, because the same module
// object serves every thread of the run: these are accessors over the current thread context,
// so the main thread reads the main thread's and a worker reads its own.
Object.defineProperty(exports, 'isMainThread', {
    enumerable: true,
    get: function () {
        return current === MAIN;
    }
});

Object.defineProperty(exports, 'parentPort', {
    enumerable: true,
    get: function () {
        return current.port;
    }
});

Object.defineProperty(exports, 'workerData', {
    enumerable: true,
    get: function () {
        return current.data;
    }
});

Object.defineProperty(exports, 'threadId', {
    enumerable: true,
    get: function () {
        return current.id;
    }
});

exports.Worker = Worker;
exports.MessageChannel = MessageChannel;
exports.MessagePort = MessagePort;
exports.receiveMessageOnPort = receiveMessageOnPort;
// "Mark an object as not transferable": nothing posted here is transferred — every message is
// cloned — so the marking is the object itself.
exports.markAsUntransferable = function (value) {
    return value;
};
exports.isMarkedAsUntransferable = function () {
    return false;
};
exports.setEnvironmentData = function (key, value) {
    if (value === undefined) {
        environmentData.delete(key);
        return;
    }
    environmentData.set(key, cloned(value));
};
exports.getEnvironmentData = function (key) {
    return environmentData.get(key);
};
// "the SHARE_ENV symbol … instructs the worker to share the environment": the environment of
// this execution environment is one map, and every thread of the run reads that one.
exports.SHARE_ENV = Symbol.for('nodejs.worker_threads.SHARE_ENV');
"##;
