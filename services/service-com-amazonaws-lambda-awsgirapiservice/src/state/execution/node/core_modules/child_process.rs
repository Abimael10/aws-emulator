// SPDX-License-Identifier: CC0-1.0

//! `child_process`: the child-process interface of this runtime.
//!
//! A managed `nodejs*` runtime is a promise about an interpreter AND about the standard
//! library that comes with it, so a handler — or a bundle it ships — whose module body says
//! `require('child_process')` loads on the runtime it was written for, and so it has to
//! here. A module of that standard library this emulator registers no capability for is
//! SUPPLIED on the resolver path instead of being absent from it: what is refused is the
//! CALL that needs the capability, never the import, because an import that fails points the
//! developer at a deployment package that is not at fault. The Python half of the same
//! decision is the `subprocess` module this crate already supplies
//! ([`crate::state::execution::guest_process`]).
//!
//! What is this module's own, and what it refuses:
//!
//! * the NAMES a program reads off `child_process` — the four asynchronous entry points, the
//!   three synchronous ones and the `ChildProcess` class — are all here, so a module that
//!   imports one of them to call it later, or to name it at all, imports and runs;
//! * the pieces that are COMPUTATION are performed rather than refused: the arguments of
//!   each entry point are read as node reads them (the optional `args`, the optional
//!   `options` and the trailing callback), a command line is run by the shell the caller
//!   named or by the default one, and the child object reports the file and the arguments it
//!   was built with;
//! * STARTING a child is not, and no call here pretends otherwise: an invocation of this
//!   emulator runs in this process, with no process of its own to fork and no host program
//!   to execute, so every call that would start one reports the failure node reports for a
//!   spawn that did not start — an `Error` carrying `ENOSYS`, the number a system call that
//!   is not there answers with — through that entry point's own channel: thrown by a
//!   synchronous call, answered in `spawnSync`'s result, and reported to the callback and as
//!   the child's `error` event by an asynchronous one.
//!
//! Nothing here invents a child: no process id, no exit status, no signal and no output is
//! reported for work nothing ran.

/// `child_process`: the entry points a node program starts a child with, on a runtime that
/// starts none.
pub(super) const CHILD_PROCESS_SOURCE: &str = r##"
'use strict';

var EventEmitter = require('events');

// The number a system call that is not there answers with, which is what a spawn nothing
// carried reports. node carries it negated in `errno`, as the result it came from.
var ENOSYS = 38;

// The program a COMMAND LINE is run by when the caller names no other shell, and the
// argument that hands it the line: node's own default on a POSIX host, which is what the
// execution environment a function runs in is.
var SHELL = '/bin/sh';
var SHELL_ARGUMENT = '-c';

function inherit(child, parent) {
    child.prototype = Object.create(parent.prototype);
    child.prototype.constructor = child;
}

// The failure of a spawn, as node builds one: the message is the syscall that failed and
// the code it failed with, and the properties beside it are what a caller branches on.
function notStarted(syscall, file, args) {
    var failing = syscall + ' ' + file;
    var error = new Error(failing + ' ENOSYS');
    error.errno = -ENOSYS;
    error.code = 'ENOSYS';
    error.syscall = failing;
    error.path = file;
    error.spawnargs = args.slice();
    return error;
}

// ------------------------------------------------------------------ reading arguments

// Whether `value` is an options object, which is what tells the optional `options` of a
// call from the optional `args` and the trailing callback.
function isOptions(value) {
    return value !== null && typeof value === 'object' && !Array.isArray(value);
}

// The arguments of a call, read the way node reads them: the file (or the command line),
// then an optional argument list, an optional options object and a trailing callback in
// whatever combination the caller passed them.
function read(file, first, second, third) {
    var args = Array.isArray(first) ? first.map(String) : [];
    var rest = Array.isArray(first) ? [second, third] : [first, second, third];
    var settings = {};
    var callback = null;
    for (var index = 0; index < rest.length; index += 1) {
        var value = rest[index];
        if (typeof value === 'function') {
            callback = value;
        } else if (isOptions(value)) {
            settings = value;
        }
    }
    return { file: String(file), args: args, settings: settings, callback: callback };
}

// The shell a command line is run by: the one `options.shell` names, or the default one —
// the option may also be `true`, which asks for that same default.
function shellOf(settings) {
    var named = settings.shell;
    return typeof named === 'string' && named !== '' ? named : SHELL;
}

// What `options.shell` asks of `spawn` and `execFile`: the file and its arguments become
// ONE command line run by the shell, so the shell is the file that would be executed.
function shelled(taken) {
    if (!taken.settings.shell) {
        return { file: taken.file, args: taken.args };
    }
    return {
        file: shellOf(taken.settings),
        args: [SHELL_ARGUMENT, [taken.file].concat(taken.args).join(' ')],
    };
}

// What an asynchronous entry point hands its callback for each stream of a child that
// produced nothing: the empty text, or empty bytes where the caller asked for bytes.
function noOutput(settings) {
    if (settings.encoding === 'buffer' || settings.encoding === null) {
        return require('buffer').Buffer.alloc(0);
    }
    return '';
}

// ------------------------------------------------------------------ the child

// One child process as a node program holds it. Nothing was started for it, so every
// stream is absent and no process id is reported — which is what node hands back for a
// spawn that failed, and this is one.
function ChildProcess() {
    EventEmitter.call(this);
    this.stdin = null;
    this.stdout = null;
    this.stderr = null;
    this.stdio = [null, null, null];
    this.pid = undefined;
    this.exitCode = null;
    this.signalCode = null;
    this.killed = false;
    this.connected = false;
    this.spawnfile = undefined;
    this.spawnargs = [];
}

inherit(ChildProcess, EventEmitter);

// "Returns true if the signal was delivered": nothing was started, so nothing is signalled.
ChildProcess.prototype.kill = function () {
    return false;
};

ChildProcess.prototype.send = function () {
    return false;
};

ChildProcess.prototype.disconnect = function () {
    return undefined;
};

ChildProcess.prototype.ref = function () {
    return this;
};

ChildProcess.prototype.unref = function () {
    return this;
};

function newChild(file, args) {
    var child = new ChildProcess();
    child.spawnfile = file;
    child.spawnargs = [file].concat(args);
    return child;
}

// Report a spawn nothing carried through the channels an ASYNCHRONOUS entry point reports
// one through — deferred, because a node program attaches its handlers to the child it has
// just been handed, and those are the handlers the failure is reported to.
function reported(child, error, callback, settings) {
    Promise.resolve().then(function () {
        if (callback !== null) {
            callback(error, noOutput(settings), noOutput(settings));
        }
        if (child.listenerCount('error') > 0) {
            child.emit('error', error);
        } else if (callback === null) {
            // An `error` nothing listened for is the program's own failure, the way node
            // reports an unhandled one.
            Promise.reject(error);
        }
        child.emit('close', null, null);
    });
    return child;
}

// ------------------------------------------------------------------ the entry points

function spawn(file, first, second) {
    var taken = read(file, first, second);
    var started = shelled(taken);
    return reported(
        newChild(started.file, started.args),
        notStarted('spawn', started.file, started.args),
        null,
        taken.settings
    );
}

// "Will not return until the child process has fully closed": it never started, so what is
// answered is what node answers for a spawn that failed — the failure in `error`, with no
// status, no signal and no output. It does not throw: the caller reads the result.
function spawnSync(file, first, second) {
    var taken = read(file, first, second);
    var started = shelled(taken);
    return {
        pid: undefined,
        output: null,
        stdout: null,
        stderr: null,
        status: null,
        signal: null,
        error: notStarted('spawnSync', started.file, started.args),
    };
}

function exec(command, first, second) {
    var taken = read(command, first, second);
    var shell = shellOf(taken.settings);
    var args = [SHELL_ARGUMENT, taken.file];
    return reported(
        newChild(shell, args),
        notStarted('spawn', shell, args),
        taken.callback,
        taken.settings
    );
}

// The synchronous forms THROW: "if the process times out or has a non-zero exit code, this
// method will throw" — and a spawn that never started is reported the same way.
function execSync(command, first) {
    var taken = read(command, first);
    throw notStarted('spawnSync', shellOf(taken.settings), [SHELL_ARGUMENT, taken.file]);
}

function execFile(file, first, second, third) {
    var taken = read(file, first, second, third);
    var started = shelled(taken);
    return reported(
        newChild(started.file, started.args),
        notStarted('spawn', started.file, started.args),
        taken.callback,
        taken.settings
    );
}

function execFileSync(file, first, second) {
    var taken = read(file, first, second);
    var started = shelled(taken);
    throw notStarted('spawnSync', started.file, started.args);
}

// `fork` runs the node executable itself on the module it names, so the file a failed one
// reports is the interpreter this runtime runs.
function fork(modulePath, first, second) {
    var taken = read(modulePath, first, second);
    var interpreter = globalThis.process && globalThis.process.execPath
        ? globalThis.process.execPath
        : 'node';
    var args = [taken.file].concat(taken.args);
    return reported(
        newChild(interpreter, args),
        notStarted('spawn', interpreter, args),
        null,
        taken.settings
    );
}

module.exports = {
    ChildProcess: ChildProcess,
    spawn: spawn,
    spawnSync: spawnSync,
    exec: exec,
    execSync: execSync,
    execFile: execFile,
    execFileSync: execFileSync,
    fork: fork,
};
"##;
