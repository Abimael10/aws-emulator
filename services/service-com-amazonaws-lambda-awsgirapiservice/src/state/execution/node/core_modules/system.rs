// SPDX-License-Identifier: CC0-1.0

//! `os` and `assert`: the two remaining core modules of this group a handler reaches for
//! that are pure computation over what the run already holds.
//!
//! `os` reports the SANDBOX a function runs in — the task root's writable directory, the
//! memory the invocation was given — rather than anything of the host, and `assert` is
//! algebra over values. The two TEXT CODECS this group used to hold — `querystring` and
//! `url` — are the same for every service that runs Node.js code, so they are compiled from
//! the one shared source ([`text_codecs`]).

/// The query codec and the URL object, from the source this emulator keeps once.
mod text_codecs;
pub use text_codecs::*;

/// `os`: what the execution environment reports about itself.
pub(super) const OS_SOURCE: &str = r##"
'use strict';

// The one writable directory a function has, which is what `os.tmpdir()` names.
var TMP = '/tmp';

function memoryBytes() {
    var configured = Number(globalThis.process.env.AWS_LAMBDA_FUNCTION_MEMORY_SIZE);
    return isFinite(configured) && configured > 0 ? configured * 1024 * 1024 : 0;
}

exports.EOL = '\n';
exports.platform = function () { return globalThis.process.platform; };
exports.arch = function () { return globalThis.process.arch; };
exports.type = function () { return 'Linux'; };
exports.release = function () { return globalThis.process.release.name; };
exports.endianness = function () { return 'LE'; };
exports.tmpdir = function () { return TMP; };
exports.homedir = function () { return globalThis.process.env.HOME || TMP; };
exports.hostname = function () { return globalThis.process.env.AWS_LAMBDA_LOG_STREAM_NAME || ''; };
exports.totalmem = memoryBytes;
// What is left of the invocation's own memory allowance: the engine's accounting of what
// this run holds, subtracted from what it was given.
exports.freemem = function () {
    return Math.max(0, memoryBytes() - globalThis.process.memoryUsage().heapUsed);
};
exports.uptime = function () { return globalThis.process.uptime(); };
exports.loadavg = function () { return [0, 0, 0]; };
exports.cpus = function () { return []; };
exports.networkInterfaces = function () { return {}; };
exports.userInfo = function () {
    return {
        uid: 993,
        gid: 990,
        username: 'sbx_user1051',
        homedir: exports.homedir(),
        shell: null
    };
};
exports.constants = { signals: {}, errno: {} };
exports.devNull = '/dev/null';
"##;

/// `assert`: the checks a Node program states its own expectations with.
pub(super) const ASSERT_SOURCE: &str = r##"
'use strict';

var util = require('util');

function AssertionError(options) {
    var error = Error.call(this, options.message);
    this.message = options.message;
    this.actual = options.actual;
    this.expected = options.expected;
    this.operator = options.operator;
    this.generatedMessage = options.generatedMessage === true;
    this.code = 'ERR_ASSERTION';
    this.name = 'AssertionError';
    if (error !== undefined && typeof error.stack === 'string') {
        this.stack = error.stack;
    }
}

AssertionError.prototype = Object.create(Error.prototype);
AssertionError.prototype.constructor = AssertionError;
AssertionError.prototype.name = 'AssertionError';

function failure(actual, expected, message, operator) {
    if (message instanceof Error) {
        throw message;
    }
    throw new AssertionError({
        message: message === undefined || message === null
            ? util.inspect(actual) + ' ' + operator + ' ' + util.inspect(expected)
            : String(message),
        actual: actual,
        expected: expected,
        operator: operator,
        generatedMessage: message === undefined || message === null
    });
}

// Structural equality: the same keys with equal values, compared strictly when `strict`.
function equivalent(first, second, strict) {
    if (strict ? first === second : first == second) {
        return true;
    }
    if (typeof first === 'number' && typeof second === 'number') {
        return Number.isNaN(first) && Number.isNaN(second);
    }
    if (first === null || second === null || typeof first !== 'object' || typeof second !== 'object') {
        return false;
    }
    if (strict && Object.getPrototypeOf(first) !== Object.getPrototypeOf(second)) {
        return false;
    }
    if (Array.isArray(first) !== Array.isArray(second)) {
        return false;
    }
    if (first instanceof Date && second instanceof Date) {
        return first.getTime() === second.getTime();
    }
    var left = Object.keys(first);
    var right = Object.keys(second);
    if (left.length !== right.length) {
        return false;
    }
    for (var index = 0; index < left.length; index += 1) {
        if (!Object.prototype.hasOwnProperty.call(second, left[index])) {
            return false;
        }
        if (!equivalent(first[left[index]], second[left[index]], strict)) {
            return false;
        }
    }
    return true;
}

function ok(value, message) {
    if (!value) {
        failure(value, true, message, '==');
    }
}

var assert = function (value, message) { ok(value, message); };

assert.AssertionError = AssertionError;
assert.ok = ok;
assert.equal = function (actual, expected, message) {
    if (!equivalent(actual, expected, false)) {
        failure(actual, expected, message, '==');
    }
};
assert.notEqual = function (actual, expected, message) {
    if (equivalent(actual, expected, false)) {
        failure(actual, expected, message, '!=');
    }
};
assert.strictEqual = function (actual, expected, message) {
    if (actual !== expected && !(Number.isNaN(actual) && Number.isNaN(expected))) {
        failure(actual, expected, message, 'strictEqual');
    }
};
assert.notStrictEqual = function (actual, expected, message) {
    if (actual === expected) {
        failure(actual, expected, message, 'notStrictEqual');
    }
};
assert.deepEqual = function (actual, expected, message) {
    if (!equivalent(actual, expected, false)) {
        failure(actual, expected, message, 'deepEqual');
    }
};
assert.notDeepEqual = function (actual, expected, message) {
    if (equivalent(actual, expected, false)) {
        failure(actual, expected, message, 'notDeepEqual');
    }
};
assert.deepStrictEqual = function (actual, expected, message) {
    if (!equivalent(actual, expected, true)) {
        failure(actual, expected, message, 'deepStrictEqual');
    }
};
assert.notDeepStrictEqual = function (actual, expected, message) {
    if (equivalent(actual, expected, true)) {
        failure(actual, expected, message, 'notDeepStrictEqual');
    }
};
assert.match = function (text, pattern, message) {
    if (!pattern.test(text)) {
        failure(text, pattern, message, 'match');
    }
};
assert.doesNotMatch = function (text, pattern, message) {
    if (pattern.test(text)) {
        failure(text, pattern, message, 'doesNotMatch');
    }
};
assert.throws = function (run, expected, message) {
    try {
        run();
    } catch (error) {
        return;
    }
    failure(undefined, expected, message === undefined ? 'Missing expected exception.' : message,
        'throws');
};
assert.doesNotThrow = function (run, message) {
    run();
    return undefined;
};
assert.rejects = function (run, expected, message) {
    var started = typeof run === 'function' ? run() : run;
    return Promise.resolve(started).then(
        function () {
            failure(undefined, expected,
                message === undefined ? 'Missing expected rejection.' : message, 'rejects');
        },
        function () { return undefined; }
    );
};
assert.doesNotReject = function (run) {
    var started = typeof run === 'function' ? run() : run;
    return Promise.resolve(started).then(function () { return undefined; });
};
assert.ifError = function (value) {
    if (value !== null && value !== undefined) {
        failure(value, null, 'ifError got unwanted exception: ' + util.inspect(value), 'ifError');
    }
};
assert.fail = function (message) {
    failure(undefined, undefined,
        message === undefined ? 'Failed' : message, 'fail');
};
assert.strict = assert;

module.exports = assert;
"##;
