// SPDX-License-Identifier: CC0-1.0

//! `vm`: the source a Node program compiles and runs at runtime.
//!
//! The source RUNS. It is compiled and evaluated by the very engine the handler runs in, so
//! what a call here answers is what the caller's own source computed — and it runs under the
//! same bounds every other line of the run does ([`super::super`] module docs): the deadline,
//! the step ceiling and the allocation ceiling are the engine's and the host's, and source
//! evaluated here cannot reach past them or see anything of the host the handler itself
//! cannot.
//!
//! What a CONTEXT is here is stated exactly: this engine gives a run ONE global object, so a
//! "new context" is the scope the names of the sandbox object are bound in for the extent of
//! the call. Those names are in scope for the source, and the value each holds when the source
//! has run is written back onto the sandbox — which is what a program that passes a sandbox
//! reads its answer out of. A name the source introduces that the sandbox did not carry is
//! not a member of it.

/// `vm`: compiling and running source, and the sandbox a call may name.
pub(super) const VM_SOURCE: &str = r##"
'use strict';

// The names a sandbox may bind. A key that is not a plain identifier is no name source can
// read, so it is left out of the scope rather than spliced into the compiled text.
function bindableKeys(sandbox) {
    if (sandbox === null || sandbox === undefined || typeof sandbox !== 'object') {
        return [];
    }
    return Object.keys(sandbox).filter(function (key) {
        return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(key);
    });
}

// Run `code` with the sandbox's names in scope, and write back what each holds afterwards.
//
// The names are the compiled function's own parameters, and the source is evaluated inside it,
// so a read of one of them is a read of the sandbox's value and an assignment to one is visible
// to the readback below. The engine states no second global object, so this is what a context
// of this runtime is (see this module's docs).
function runWith(code, sandbox, options) {
    var keys = bindableKeys(sandbox);
    var values = keys.map(function (key) { return sandbox[key]; });
    var text = String(code);
    var body = 'var __vmValue = eval(' + JSON.stringify(text) + ');\n'
        + 'return { value: __vmValue, names: [' + keys.join(', ') + '] };';
    var runner = new Function(keys.join(','), body);
    var answered = runner.apply(undefined, values);
    for (var index = 0; index < keys.length; index += 1) {
        sandbox[keys[index]] = answered.names[index];
    }
    return answered.value;
}

// "Compiles code … to be run multiple times": the source is held here and compiled by the
// engine at every run, which is what makes each run the caller's own source and not a cached
// answer.
function Script(code, options) {
    this.code = String(code);
    this.filename = options && options.filename ? String(options.filename) : 'evalmachine.<anonymous>';
}

Script.prototype.runInThisContext = function () {
    // An INDIRECT eval, which is what runs source in the global scope rather than in this
    // function's.
    var evaluate = eval;
    return evaluate(this.code);
};

Script.prototype.runInNewContext = function (sandbox, options) {
    return runWith(this.code, sandbox === undefined ? {} : sandbox, options);
};

Script.prototype.runInContext = Script.prototype.runInNewContext;

Script.prototype.createCachedData = function () {
    // "Creates a code cache": this runtime caches no compiled code, so there is no cache to
    // hand out and nothing is handed out that would read back as one.
    var error = new Error('a code cache is not available in this execution environment');
    error.code = 'ERR_NOT_SUPPORTED';
    throw error;
};

module.exports = {
    Script: Script,
    runInThisContext: function (code) {
        return new Script(code).runInThisContext();
    },
    runInNewContext: function (code, sandbox, options) {
        return runWith(code, sandbox === undefined ? {} : sandbox, options);
    },
    runInContext: function (code, sandbox, options) {
        return runWith(code, sandbox === undefined ? {} : sandbox, options);
    },
    // "Contextifies the given object": the object itself is the context here (see this
    // module's docs), so it is handed back and carries the marking `isContext` reads.
    createContext: function (sandbox) {
        var held = sandbox === undefined || sandbox === null ? {} : sandbox;
        Object.defineProperty(held, '__vmContext', {
            value: true,
            enumerable: false,
            configurable: true
        });
        return held;
    },
    isContext: function (sandbox) {
        return sandbox !== null && typeof sandbox === 'object'
            && sandbox.__vmContext === true;
    },
    // "Compiles the given code into the provided context": the parameters are the function's
    // own and the body is the caller's source, which is what the engine's own function
    // constructor states.
    compileFunction: function (code, params, options) {
        var named = Array.isArray(params) ? params.map(String) : [];
        return new Function(named.join(','), String(code));
    },
    measureMemory: function () {
        var error = new Error('a memory measurement is not available in this execution '
            + 'environment');
        error.code = 'ERR_NOT_SUPPORTED';
        return Promise.reject(error);
    },
    constants: { USE_MAIN_CONTEXT_DEFAULT_LOADER: 0, DONT_CONTEXTIFY: 1 }
};
"##;

/// `readline`: the lines a Node program reads off a stream.
pub(super) const READLINE_SOURCE: &str = r##"
'use strict';

var EventEmitter = require('events');

// "The Interface class … represents a readline or readline/promises interface with an input
// stream": the lines of what it is handed, one event each, and the close that follows the last
// of them. It is COMPUTATION over a stream of this run — no terminal and no host descriptor is
// reached, because the input is whatever stream the caller passed.
function Interface(options) {
    EventEmitter.call(this);
    var held = options === null || options === undefined ? {} : options;
    var input = held.input === undefined ? held : held.input;
    this.input = input;
    this.output = held.output;
    this.terminal = held.terminal === true;
    this.closed = false;
    this.line = '';
    this.cursor = 0;
    this._held = '';
    this._lines = [];
    this._questions = [];
    var self = this;
    if (input !== null && input !== undefined && typeof input.on === 'function') {
        input.on('data', function (chunk) {
            self._carry(String(chunk));
        });
        input.on('end', function () {
            if (self._held !== '') {
                self._line(self._held);
                self._held = '';
            }
            self.close();
        });
    }
}

Interface.prototype = Object.create(EventEmitter.prototype);
Interface.prototype.constructor = Interface;

// What has arrived so far, split at the line breaks it carries: a line is reported once its
// break has arrived, and the tail without one is held for the bytes still to come.
Interface.prototype._carry = function (text) {
    this._held = this._held + text;
    for (;;) {
        var cut = this._held.indexOf('\n');
        if (cut < 0) {
            return;
        }
        var line = this._held.slice(0, cut);
        this._held = this._held.slice(cut + 1);
        if (line.length > 0 && line.charAt(line.length - 1) === '\r') {
            line = line.slice(0, line.length - 1);
        }
        this._line(line);
    }
};

// One line, to whoever is waiting for it: the question asked first, because that is the one
// reader a line was asked for by, and the `line` event otherwise.
Interface.prototype._line = function (line) {
    this.line = line;
    if (this._questions.length > 0) {
        this._questions.shift()(line);
        return;
    }
    this._lines.push(line);
    this.emit('line', line);
};

Interface.prototype.question = function (query, callback) {
    if (this.output !== null && this.output !== undefined
        && typeof this.output.write === 'function') {
        this.output.write(String(query));
    }
    if (typeof callback === 'function') {
        this._questions.push(callback);
        return undefined;
    }
    var self = this;
    return new Promise(function (resolve) {
        self._questions.push(resolve);
    });
};

Interface.prototype.prompt = function () {
    return undefined;
};

Interface.prototype.write = function (text) {
    if (this.output !== null && this.output !== undefined
        && typeof this.output.write === 'function') {
        this.output.write(String(text));
    }
    return undefined;
};

Interface.prototype.setPrompt = function (prompt) {
    this._prompt = String(prompt);
};

Interface.prototype.getPrompt = function () {
    return this._prompt === undefined ? '' : this._prompt;
};

Interface.prototype.pause = function () { return this; };
Interface.prototype.resume = function () { return this; };

Interface.prototype.close = function () {
    if (this.closed) {
        return;
    }
    this.closed = true;
    this.emit('close');
};

// "readline.Interface … is an async iterable": every line that has arrived, and then the ones
// that arrive until the interface closes.
Interface.prototype[Symbol.asyncIterator] = function () {
    var self = this;
    var pending = [];
    var waiting = [];
    var ended = false;
    this.on('line', function (line) {
        if (waiting.length > 0) {
            waiting.shift()({ value: line, done: false });
            return;
        }
        pending.push(line);
    });
    this.on('close', function () {
        ended = true;
        while (waiting.length > 0) {
            waiting.shift()({ value: undefined, done: true });
        }
    });
    return {
        next: function () {
            if (pending.length > 0) {
                return Promise.resolve({ value: pending.shift(), done: false });
            }
            if (ended || self.closed) {
                return Promise.resolve({ value: undefined, done: true });
            }
            return new Promise(function (resolve) {
                waiting.push(resolve);
            });
        },
        [Symbol.asyncIterator]: function () { return this; }
    };
};

// The cursor calls a program writes to a terminal with. The output is whatever stream the
// caller passed, so what is written is the control sequence itself — nothing here holds a
// terminal.
function cursorWrite(stream, sequence, callback) {
    if (stream !== null && stream !== undefined && typeof stream.write === 'function') {
        stream.write(sequence);
    }
    if (typeof callback === 'function') {
        Promise.resolve().then(callback);
    }
    return true;
}

module.exports = {
    Interface: Interface,
    createInterface: function (options) { return new Interface(options); },
    clearLine: function (stream, direction, callback) {
        var sequence = direction < 0 ? '\u001b[1K' : (direction > 0 ? '\u001b[0K' : '\u001b[2K');
        return cursorWrite(stream, sequence, callback);
    },
    clearScreenDown: function (stream, callback) {
        return cursorWrite(stream, '\u001b[0J', callback);
    },
    cursorTo: function (stream, x, y, callback) {
        var row = typeof y === 'number' ? y : 0;
        var done = typeof y === 'function' ? y : callback;
        return cursorWrite(stream, '\u001b[' + (row + 1) + ';' + (Number(x) + 1) + 'H', done);
    },
    moveCursor: function (stream, dx, dy, callback) {
        var sequence = '';
        if (Number(dx) !== 0) {
            sequence = sequence + '\u001b[' + Math.abs(Number(dx)) + (Number(dx) > 0 ? 'C' : 'D');
        }
        if (Number(dy) !== 0) {
            sequence = sequence + '\u001b[' + Math.abs(Number(dy)) + (Number(dy) > 0 ? 'B' : 'A');
        }
        return cursorWrite(stream, sequence, callback);
    },
    emitKeypressEvents: function () { return undefined; },
    promises: null
};

// The promise form of the same interface: `question` answers a promise when it is called
// without a callback, which is the one difference between the two modules.
module.exports.promises = {
    Interface: Interface,
    createInterface: module.exports.createInterface
};
"##;

/// `readline/promises`: the same interface, under the name the promise form is imported by.
pub(super) const READLINE_PROMISES_SOURCE: &str = r##"
'use strict';

module.exports = require('readline').promises;
"##;
