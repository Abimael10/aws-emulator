// SPDX-License-Identifier: CC0-1.0

//! `zlib`: the compression a Node program asks its runtime for.
//!
//! The arithmetic is NOT written here. One deflate stream, under each of the three wrappers a
//! program asks for it by, is computed by the host ([`super::super::compress`]); this module is
//! the Node-shaped interface over it — it holds the bytes a program hands a call, names the
//! wrapper and the level, hands the answer back as a `Buffer`, and reports a stream it could
//! not read as the data error node reports for one. Nothing here reaches a host file, socket or
//! environment, and nothing is answered that was not computed: a call the host refused is a
//! failure, never empty bytes.
//!
//! Each of the three wrappers is here in all four of the forms node states it in — the
//! synchronous call, the callback call, the stream, and the `util.promisify`-able callback form
//! — because a program picks the form and not the algorithm. The bound on how many bytes one
//! call may read or write is the HOST's ([`super::super::compress::MAX_COMPRESSED_BYTES`]) and
//! is stated on both sides of the call, so a breach is a refusal rather than this process's
//! memory.

/// `zlib`: the deflate family, in each of the forms a node program calls it in.
pub(super) const ZLIB_SOURCE: &str = r##"
'use strict';

var Buffer = require('buffer').Buffer;
var Transform = require('stream').Transform;

// The host half of this module: the two directions of the stream the runtime protocol
// exposes.
function host() {
    return globalThis.__lambdaRuntime;
}

// How many bytes one call may read or write, which is the ceiling the HOST enforces
// ([`super::super::compress::MAX_COMPRESSED_BYTES`]): the bytes are materialised in host
// memory, outside the engine's own allocation ceiling, so the bound is stated on both sides
// of the call.
var MAX_COMPRESSED_BYTES = 8388608;

// The wrapper each entry point asks the one stream for, and the direction it asks in.
var GZIP = 'gzip';
var DEFLATE = 'deflate';
var DEFLATE_RAW = 'deflate-raw';
var BROTLI = 'brotli';
var AUTO = 'auto';

// The bytes a call was given, whatever of the forms node accepts they were given in.
function bytesOf(data, options) {
    if (typeof data === 'string') {
        var encoding = options && options.encoding ? options.encoding : 'utf8';
        return Buffer.from(data, encoding);
    }
    if (data === null || data === undefined) {
        var error = new TypeError('The "buffer" argument must be of type string or an instance '
            + 'of Buffer, TypedArray, DataView, or ArrayBuffer. Received ' + String(data));
        error.code = 'ERR_INVALID_ARG_TYPE';
        throw error;
    }
    return Buffer.from(data);
}

// The level a call asked for: the option node reads it from, and the library's own default
// where the call named none. A brotli call states its own quality under the parameter map that
// format's options carry.
function levelOf(options) {
    if (options === null || options === undefined) {
        return -1;
    }
    if (options.params !== null && options.params !== undefined
        && options.params[constants.BROTLI_PARAM_QUALITY] !== undefined) {
        var quality = Number(options.params[constants.BROTLI_PARAM_QUALITY]);
        return Number.isFinite(quality) ? quality : -1;
    }
    if (options.level === undefined) {
        return -1;
    }
    var level = Number(options.level);
    return Number.isFinite(level) ? level : -1;
}

// The failure a call the host refused reports, per DIRECTION. On the way IN, a stream that
// cannot be read is the data error zlib itself raises for one. On the way OUT, the stream is
// this module's own — every wrapper it names is one the host computes — so the only refusal
// left is an answer past the ceiling above, which is the range error a buffer that cannot be
// made is reported as. An input past that ceiling is told apart from both HERE, before the
// call.
function dataError() {
    var error = new Error('incorrect header check');
    error.errno = -3;
    error.code = 'Z_DATA_ERROR';
    return error;
}

function tooLarge(count) {
    var error = new RangeError('Cannot create a Buffer larger than ' + MAX_COMPRESSED_BYTES
        + ' bytes. Received ' + count);
    error.code = 'ERR_BUFFER_TOO_LARGE';
    return error;
}

// One call in either direction: the bytes go over as hex and come back as hex, and a call the
// host refused is reported as the failure for its direction.
function transform(format, direction, data, options) {
    var bytes = bytesOf(data, options);
    if (bytes.length > MAX_COMPRESSED_BYTES) {
        throw tooLarge(bytes.length);
    }
    var answered;
    if (direction === 'deflate') {
        try {
            answered = host().deflate(format, levelOf(options), bytes.toString('hex'));
        } catch (error) {
            throw tooLarge(MAX_COMPRESSED_BYTES);
        }
    } else {
        try {
            answered = host().inflate(format, bytes.toString('hex'));
        } catch (error) {
            throw dataError();
        }
    }
    return Buffer.from(answered, 'hex');
}

// The arguments of a callback call, read the way node reads them: an optional options object
// before the trailing callback.
function read(first, second) {
    if (typeof first === 'function') {
        return { options: {}, callback: first };
    }
    return {
        options: first === null || first === undefined ? {} : first,
        callback: typeof second === 'function' ? second : null
    };
}

// A callback call: the same computation, reported to the callback rather than returned —
// deferred, because a node program attaches nothing of its own to the call and reads the
// answer only in that callback.
function deferred(format, direction) {
    return function (data, first, second) {
        var taken = read(first, second);
        var done = taken.callback === null ? function () { return undefined; } : taken.callback;
        var answered = null;
        try {
            answered = transform(format, direction, data, taken.options);
        } catch (error) {
            Promise.resolve().then(function () { done(error); });
            return undefined;
        }
        Promise.resolve().then(function () { done(null, answered); });
        return undefined;
    };
}

// The STREAM form of the same computation: a transform stream that holds what it was written
// and computes the whole answer when the writing ends, which is what one deflate stream over a
// whole input is.
function streamOf(format, direction) {
    return function (options) {
        var held = options === null || options === undefined ? {} : options;
        var stream = new Transform(held);
        var chunks = [];
        stream._transform = function (chunk, encoding, next) {
            chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk, encoding));
            next();
        };
        stream._flush = function (next) {
            var answered;
            try {
                answered = transform(format, direction, Buffer.concat(chunks), held);
            } catch (error) {
                next(error);
                return;
            }
            stream.push(answered);
            next();
        };
        return stream;
    };
}

// "zlib.constants": the names a program states a level, a strategy or a flush by. The values
// are zlib's own, which is what a program comparing against one reads.
var constants = {
    Z_NO_FLUSH: 0,
    Z_PARTIAL_FLUSH: 1,
    Z_SYNC_FLUSH: 2,
    Z_FULL_FLUSH: 3,
    Z_FINISH: 4,
    Z_BLOCK: 5,
    Z_TREES: 6,
    Z_OK: 0,
    Z_STREAM_END: 1,
    Z_NEED_DICT: 2,
    Z_ERRNO: -1,
    Z_STREAM_ERROR: -2,
    Z_DATA_ERROR: -3,
    Z_MEM_ERROR: -4,
    Z_BUF_ERROR: -5,
    Z_VERSION_ERROR: -6,
    Z_NO_COMPRESSION: 0,
    Z_BEST_SPEED: 1,
    Z_BEST_COMPRESSION: 9,
    Z_DEFAULT_COMPRESSION: -1,
    Z_FILTERED: 1,
    Z_HUFFMAN_ONLY: 2,
    Z_RLE: 3,
    Z_FIXED: 4,
    Z_DEFAULT_STRATEGY: 0,
    Z_DEFAULT_WINDOWBITS: 15,
    Z_MIN_WINDOWBITS: 8,
    Z_MAX_WINDOWBITS: 15,
    Z_MIN_CHUNK: 64,
    Z_MAX_CHUNK: Infinity,
    Z_DEFAULT_CHUNK: 16384,
    Z_MIN_MEMLEVEL: 1,
    Z_MAX_MEMLEVEL: 9,
    Z_DEFAULT_MEMLEVEL: 8,
    Z_MIN_LEVEL: -1,
    Z_MAX_LEVEL: 9,
    Z_DEFAULT_LEVEL: -1,
    DEFLATE: 1,
    INFLATE: 2,
    GZIP: 3,
    GUNZIP: 4,
    DEFLATERAW: 5,
    INFLATERAW: 6,
    UNZIP: 7,
    BROTLI_DECODE: 8,
    BROTLI_ENCODE: 9,
    BROTLI_OPERATION_PROCESS: 0,
    BROTLI_OPERATION_FLUSH: 1,
    BROTLI_OPERATION_FINISH: 2,
    BROTLI_PARAM_MODE: 0,
    BROTLI_MODE_GENERIC: 0,
    BROTLI_MODE_TEXT: 1,
    BROTLI_MODE_FONT: 2,
    BROTLI_DEFAULT_MODE: 0,
    BROTLI_PARAM_QUALITY: 1,
    BROTLI_MIN_QUALITY: 0,
    BROTLI_MAX_QUALITY: 11,
    BROTLI_DEFAULT_QUALITY: 11,
    BROTLI_PARAM_LGWIN: 2,
    BROTLI_MIN_WINDOW_BITS: 10,
    BROTLI_MAX_WINDOW_BITS: 24,
    BROTLI_DEFAULT_WINDOW: 22,
    BROTLI_PARAM_SIZE_HINT: 3
};

// Each wrapper, in each of the forms node states it: the synchronous call, the callback call
// and the stream.
var FORMS = [
    ['gzip', GZIP, 'deflate', 'Gzip'],
    ['gunzip', GZIP, 'inflate', 'Gunzip'],
    ['deflate', DEFLATE, 'deflate', 'Deflate'],
    ['inflate', DEFLATE, 'inflate', 'Inflate'],
    ['deflateRaw', DEFLATE_RAW, 'deflate', 'DeflateRaw'],
    ['inflateRaw', DEFLATE_RAW, 'inflate', 'InflateRaw'],
    ['unzip', AUTO, 'inflate', 'Unzip'],
    ['brotliCompress', BROTLI, 'deflate', 'BrotliCompress'],
    ['brotliDecompress', BROTLI, 'inflate', 'BrotliDecompress']
];

for (var index = 0; index < FORMS.length; index += 1) {
    (function (named, format, direction, classed) {
        exports[named + 'Sync'] = function (data, options) {
            return transform(format, direction, data, options);
        };
        exports[named] = deferred(format, direction);
        exports['create' + classed] = streamOf(format, direction);
        // The stream under the name of its CLASS, which is the other way a program makes one:
        // the constructor answers the stream it built, as the factory above does.
        exports[classed] = exports['create' + classed];
    }(FORMS[index][0], FORMS[index][1], FORMS[index][2], FORMS[index][3]));
}

// "Computes a 32-bit Cyclic Redundancy Check checksum of data": the checksum itself, continued
// from the value a program checksumming a stream a chunk at a time hands back in.
exports.crc32 = function (data, value) {
    var bytes = bytesOf(data);
    if (bytes.length > MAX_COMPRESSED_BYTES) {
        throw tooLarge(bytes.length);
    }
    var seed = value === undefined ? 0 : Number(value);
    var checksum = host().crc32(
        bytes.toString('hex'),
        Number.isFinite(seed) ? seed : 0
    );
    return checksum;
};

exports.constants = constants;
// The same names on the module itself, which is where a program written before `constants`
// existed reads them.
var named = Object.keys(constants);
for (var at = 0; at < named.length; at += 1) {
    exports[named[at]] = constants[named[at]];
}
"##;
