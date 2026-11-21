// SPDX-License-Identifier: CC0-1.0

//! `crypto`: the digests, the keyed digests, the symmetric ciphers and the random bytes a
//! Node program asks its runtime for.
//!
//! The arithmetic is NOT written here: a digest and a keyed digest are computed by the host
//! ([`super::super::digest`]), a symmetric transform by the host's own cipher seam
//! ([`super::super::cipher`]), and random bytes come from the host's own cryptographic
//! source. This module is the Node-shaped interface over them: it holds the bytes a program
//! feeds a hash or a cipher, names the algorithm, and hands the answer back in the encoding
//! the program asked for. Nothing here reaches a host file, socket or environment.
//!
//! The WHATWG interface over the same computations is part of that surface rather than a
//! second implementation of it: `crypto.webcrypto` — which is also the global `crypto` a
//! modern Node program reaches `crypto.subtle` through — carries the `SubtleCrypto` methods
//! for the algorithms this host computes (the digests, the keyed digest they instantiate, and
//! the AES modes), each over the same host call the node API uses. An algorithm the registry
//! here does not name is refused by name rather than answered with another one's computation.

/// `crypto`: hashes, HMACs, symmetric ciphers and random bytes.
pub(super) const CRYPTO_SOURCE: &str = r##"
'use strict';

var Buffer = require('buffer').Buffer;

// The host half of this module: the three pure computations the runtime protocol exposes.
function host() {
    return globalThis.__lambdaRuntime;
}

// The digest algorithms this runtime computes, by the name a Node program names them
// under. One per implementation the host links; an algorithm that is not here is refused
// rather than answered with a digest of another algorithm.
var ALGORITHMS = ['md5', 'sha1', 'sha256', 'sha384', 'sha512'];

// How many bytes one draw of random may ask for, which is the ceiling the HOST enforces
// ([`super::super::digest::MAX_RANDOM_BYTES`]): the draw happens in host memory, outside
// the engine's own allocation ceiling, so the bound is stated on both sides of the call.
var MAX_RANDOM_BYTES = 65536;

// The length of each digest this runtime computes, in bytes, which is what bounds a key
// derived from it: the extract-and-expand construction derives at most 255 times the
// digest's own length (RFC 5869 §2.3). The bound is stated on both sides of the host call,
// as the random draw's is above and for the same reason.
var DIGEST_BYTES = { md5: 16, sha1: 20, sha256: 32, sha384: 48, sha512: 64 };

// How many iterations one password-based derivation may ask for, and how many keyed-digest
// ROUNDS it may run in total, which are the ceilings the HOST enforces
// ([`super::super::digest::MAX_PBKDF2_ITERATIONS`], `MAX_PBKDF2_ROUNDS`): the rounds are spent
// in host code, outside the engine's own bounds, so the bounds are stated on both sides of the
// call. The derivation runs the whole iteration count once per BLOCK of key asked for, so what
// bounds the work is the product.
var MAX_PBKDF2_ITERATIONS = 10000000;
var MAX_PBKDF2_ROUNDS = 10000000;

// How many bytes one derivation may ask for, which is the ceiling the HOST enforces for both
// derivations ([`super::super::digest::MAX_DERIVED_BYTES`]): the bytes are materialised in host
// memory, outside the engine's own allocation ceiling. The extract-and-expand construction has
// a tighter ceiling of its own per digest (255 times its length, RFC 5869 §2.3), which
// `hkdfSync` states; the iterated one has none, so this host bound is what bounds it.
var MAX_DERIVED_BYTES = 16320;

function algorithmOf(name) {
    var wanted = String(name).toLowerCase().replace(/-/g, '');
    for (var index = 0; index < ALGORITHMS.length; index += 1) {
        if (ALGORITHMS[index] === wanted) {
            return ALGORITHMS[index];
        }
    }
    var error = new Error('Digest method not supported');
    error.code = 'ERR_CRYPTO_INVALID_DIGEST';
    throw error;
}

function hexOf(data, encoding) {
    if (typeof data === 'string') {
        return Buffer.from(data, encoding === undefined ? 'utf8' : encoding).toString('hex');
    }
    if (data === null || data === undefined) {
        throw new TypeError('The "data" argument must be of type string or an instance of '
            + 'Buffer, TypedArray, or DataView');
    }
    return Buffer.from(data).toString('hex');
}

function encodedDigest(hex, encoding) {
    var bytes = Buffer.from(hex, 'hex');
    return encoding === undefined || encoding === null ? bytes : bytes.toString(encoding);
}

function Hash(algorithm) {
    this.algorithm = algorithmOf(algorithm);
    this.parts = '';
    this.done = false;
}

Hash.prototype.update = function (data, encoding) {
    if (this.done) {
        throw new Error('Digest already called');
    }
    this.parts += hexOf(data, encoding);
    return this;
};

Hash.prototype.digest = function (encoding) {
    if (this.done) {
        throw new Error('Digest already called');
    }
    this.done = true;
    return encodedDigest(host().digest(this.algorithm, this.parts), encoding);
};

Hash.prototype.copy = function () {
    var copied = new Hash(this.algorithm);
    copied.parts = this.parts;
    return copied;
};

function Hmac(algorithm, key) {
    this.algorithm = algorithmOf(algorithm);
    this.key = hexOf(key);
    this.parts = '';
    this.done = false;
}

Hmac.prototype.update = function (data, encoding) {
    if (this.done) {
        throw new Error('Digest already called');
    }
    this.parts += hexOf(data, encoding);
    return this;
};

Hmac.prototype.digest = function (encoding) {
    if (this.done) {
        throw new Error('Digest already called');
    }
    this.done = true;
    return encodedDigest(host().hmac(this.algorithm, this.key, this.parts), encoding);
};

function randomBytes(size, callback) {
    var count = Math.trunc(Number(size));
    // The size one draw may ask for. Node's own ceiling is `buffer.kMaxLength`, and a
    // request past it is out of range rather than an allocation: the bytes are materialised
    // outside the engine's own heap, so the request is refused before anything is drawn.
    if (!Number.isSafeInteger(count) || count < 0 || count > MAX_RANDOM_BYTES) {
        var error = new RangeError('The value of "size" is out of range. It must be >= 0 && <= '
            + MAX_RANDOM_BYTES + '. Received ' + String(size));
        error.code = 'ERR_OUT_OF_RANGE';
        throw error;
    }
    var bytes = Buffer.from(host().randomHex(count), 'hex');
    if (typeof callback === 'function') {
        Promise.resolve().then(function () { callback(null, bytes); });
        return undefined;
    }
    return bytes;
}

function randomFillSync(target, offset, size) {
    var at = offset === undefined ? 0 : Math.trunc(Number(offset));
    var count = size === undefined ? target.length - at : Math.trunc(Number(size));
    var bytes = randomBytes(count);
    for (var index = 0; index < count; index += 1) {
        target[at + index] = bytes[index];
    }
    return target;
}

// A v4 UUID: 122 random bits with the version and variant bits the format fixes.
function randomUUID() {
    var bytes = randomBytes(16);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    var hex = bytes.toString('hex');
    return hex.slice(0, 8) + '-' + hex.slice(8, 12) + '-' + hex.slice(12, 16) + '-'
        + hex.slice(16, 20) + '-' + hex.slice(20);
}

// An integer of [min, max), drawn by rejection so every value of the range is equally
// likely however wide the range is.
function randomInt(min, max, callback) {
    var low = max === undefined || typeof max === 'function' ? 0 : Math.trunc(Number(min));
    var high = max === undefined || typeof max === 'function'
        ? Math.trunc(Number(min))
        : Math.trunc(Number(max));
    var done = typeof max === 'function' ? max : callback;
    if (!(high > low)) {
        throw new RangeError('The value of "max" is out of range.');
    }
    var span = high - low;
    var bytes = 0;
    var ceiling = 1;
    while (ceiling < span) {
        ceiling *= 256;
        bytes += 1;
    }
    var drawn = span;
    while (drawn >= ceiling - (ceiling % span)) {
        drawn = 0;
        var held = randomBytes(bytes);
        for (var index = 0; index < bytes; index += 1) {
            drawn = (drawn * 256) + held[index];
        }
    }
    var value = low + (drawn % span);
    if (typeof done === 'function') {
        Promise.resolve().then(function () { done(null, value); });
        return undefined;
    }
    return value;
}

function timingSafeEqual(first, second) {
    var left = Buffer.from(first);
    var right = Buffer.from(second);
    if (left.length !== right.length) {
        throw new RangeError('Input buffers must have the same byte length');
    }
    var difference = 0;
    for (var index = 0; index < left.length; index += 1) {
        difference |= left[index] ^ right[index];
    }
    return difference === 0;
}

function getRandomValues(target) {
    return randomFillSync(target, 0, target.length);
}

// The ciphers this runtime computes, by the name a Node program names them under. The list
// is the HOST's own ([`super::super::cipher::CIPHERS`]) rather than a second copy of it, so
// what `getCiphers()` reports and what a call can compute cannot drift apart; it is read on
// first use because a module of this runtime is evaluated before any of it is called.
var HELD_CIPHERS = null;

function ciphers() {
    if (HELD_CIPHERS === null) {
        HELD_CIPHERS = host().ciphers();
    }
    return HELD_CIPHERS;
}

function cipherNameOf(name) {
    var wanted = String(name).toLowerCase();
    var named = ciphers();
    for (var index = 0; index < named.length; index += 1) {
        if (named[index] === wanted) {
            return named[index];
        }
    }
    var error = new TypeError('Unknown cipher');
    error.code = 'ERR_CRYPTO_UNKNOWN_CIPHER';
    throw error;
}

// The error one of the host's refusals is reported as, worded the way Node reports that
// failure: the message a caller matches on is the failure's, not this runtime's.
function cipherFailure(outcome, algorithm) {
    var error;
    if (outcome === 'keylen') {
        error = new RangeError('Invalid key length');
        error.code = 'ERR_CRYPTO_INVALID_KEYLEN';
        return error;
    }
    if (outcome === 'iv') {
        error = new TypeError('Invalid initialization vector');
        error.code = 'ERR_CRYPTO_INVALID_IV';
        return error;
    }
    if (outcome === 'unopened') {
        if (algorithm.slice(-4) === '-gcm') {
            return new Error('Unsupported state or unable to authenticate data');
        }
        error = new Error('error:1C800064:Provider routines::bad decrypt');
        error.code = 'ERR_OSSL_BAD_DECRYPT';
        return error;
    }
    if (outcome === 'toolong') {
        error = new RangeError('The value of "data" is out of range. It must be at most '
            + MAX_CIPHER_BYTES + ' bytes');
        error.code = 'ERR_OUT_OF_RANGE';
        return error;
    }
    if (outcome === 'malformed') {
        error = new TypeError('The "data" argument must be of type string or an instance of '
            + 'Buffer, TypedArray, or DataView');
        error.code = 'ERR_INVALID_ARG_TYPE';
        return error;
    }
    error = new TypeError('Unknown cipher');
    error.code = 'ERR_CRYPTO_UNKNOWN_CIPHER';
    return error;
}

// How many bytes one transform may carry, which is the ceiling the HOST enforces
// ([`super::super::cipher::MAX_CIPHER_BYTES`]): the bytes are transformed in host memory,
// outside the engine's own allocation ceiling, so the bound is named on both sides of the
// call as the random draw's is.
var MAX_CIPHER_BYTES = 8388608;

// One cipher in one direction: the key, the IV and the bytes a program feeds it.
//
// A COUNTER mode is a stream, so the transform of what has arrived already is settled and
// `update` hands those bytes over, as a node cipher does. The other two modes settle when the
// message does: a padded mode's last block carries the padding, and an authenticated mode's
// message is not known to open until its tag is verified — so their bytes are handed over at
// `final`, and the concatenation of what `update` and `final` answer is the whole transform
// either way.
function Cipher(algorithm, key, iv, decrypting) {
    this.algorithm = cipherNameOf(algorithm);
    this.keyHex = hexOf(key);
    this.ivHex = iv === null || iv === undefined ? '' : hexOf(iv);
    this.decrypting = decrypting === true;
    this.streamed = this.algorithm.slice(-4) === '-ctr';
    this.aadHex = '';
    this.partsHex = '';
    this.tagHex = '';
    this.authTag = null;
    this.handedOver = 0;
    this.done = false;
}

Cipher.prototype.setAAD = function (data, options) {
    // Additional data is part of an AUTHENTICATED mode's own computation; a mode that
    // authenticates nothing has nowhere to carry it, so being given some is a state this
    // cipher cannot be in rather than something to ignore.
    if (this.done || this.algorithm.slice(-4) !== '-gcm') {
        var error = new Error('Invalid state for operation setAAD');
        error.code = 'ERR_CRYPTO_INVALID_STATE';
        throw error;
    }
    this.aadHex += hexOf(data, options === undefined || options === null
        ? undefined
        : options.encoding);
    return this;
};

Cipher.prototype.setAutoPadding = function (auto) {
    // The padded mode is the only one padding decides anything for, and the transform this
    // runtime computes is the padded one: turning it off is refused rather than ignored.
    if (auto === false && this.algorithm.slice(-4) === '-cbc') {
        var error = new Error('Unsupported state or unable to authenticate data');
        error.code = 'ERR_CRYPTO_UNSUPPORTED_OPERATION';
        throw error;
    }
    return this;
};

Cipher.prototype.setAuthTag = function (tag) {
    if (this.done) {
        throw new Error('Unsupported state');
    }
    this.tagHex = hexOf(tag);
    return this;
};

Cipher.prototype.getAuthTag = function () {
    if (this.authTag === null) {
        throw new Error('Unsupported state');
    }
    return this.authTag;
};

// The whole transform of the bytes fed so far, as the host computes it; a refusal is thrown
// as the error that failure is. An encryption's tag is held as it becomes known.
Cipher.prototype.transformed = function () {
    var answered = host().cipher(
        [this.algorithm, this.keyHex, this.ivHex, this.aadHex, this.partsHex, this.tagHex],
        this.decrypting
    );
    if (answered[0] !== 'ok') {
        throw cipherFailure(answered[0], this.algorithm);
    }
    if (!this.decrypting) {
        this.authTag = Buffer.from(answered[2], 'hex');
    }
    return Buffer.from(answered[1], 'hex');
};

// The bytes of `whole` this cipher has not handed over yet.
Cipher.prototype.notYetHandedOver = function (whole) {
    var from = this.handedOver < whole.length ? this.handedOver : whole.length;
    this.handedOver = whole.length;
    return Buffer.from(whole.subarray(from));
};

// `bytes` in the form the caller asked for: the bytes themselves, or their text under the
// encoding named.
function cipherOutput(bytes, outputEncoding) {
    return outputEncoding === undefined || outputEncoding === null
        ? bytes
        : bytes.toString(outputEncoding);
}

Cipher.prototype.update = function (data, inputEncoding, outputEncoding) {
    if (this.done) {
        throw new Error('Trying to add data in unsupported state');
    }
    this.partsHex += hexOf(data, inputEncoding === null ? undefined : inputEncoding);
    var bytes = this.streamed
        ? this.notYetHandedOver(this.transformed())
        : Buffer.alloc(0);
    return cipherOutput(bytes, outputEncoding);
};

Cipher.prototype.final = function (outputEncoding) {
    if (this.done) {
        throw new Error('Unsupported state');
    }
    this.done = true;
    return cipherOutput(this.notYetHandedOver(this.transformed()), outputEncoding);
};

// The WHATWG interface over the same computations: the random source, the identifier, and the
// `SubtleCrypto` surface. Its methods are installed BELOW rather than written inline here,
// because each of them is written over the helpers that follow.
var webcrypto = {
    getRandomValues: getRandomValues,
    randomUUID: randomUUID,
    subtle: {}
};

// The bytes `value` holds, as the ArrayBuffer a WebCrypto call answers with.
function arrayBufferOf(bytes) {
    return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.length);
}

// The refusal a call naming an algorithm this runtime does not implement gets, worded and
// named the way the interface reports that failure. An algorithm outside the registry below
// is refused rather than answered with the computation of another one.
function unrecognizedAlgorithm() {
    var error = new Error('Unrecognized algorithm name');
    error.name = 'NotSupportedError';
    return error;
}

// The refusal a computation that did not complete gets: the interface reports every failure
// of the operation itself as this ONE class, whatever the node API's own name for it is — a
// message whose tag does not verify included.
function operationFailed() {
    var error = new Error('The operation failed for an operation-specific reason');
    error.name = 'OperationError';
    return error;
}

// The name an algorithm is identified by, which a call states either as a string or as the
// `name` of an object. The registry matches a name without regard to its case, so the name is
// resolved in one case and compared in it.
function webAlgorithmName(algorithm) {
    var named = algorithm === null || algorithm === undefined
        ? undefined
        : (typeof algorithm === 'string' ? algorithm : algorithm.name);
    if (named === null || named === undefined) {
        throw unrecognizedAlgorithm();
    }
    return String(named).toUpperCase();
}

// The hashes the WebCrypto registry defines, by the name it spells each under, mapped to the
// name the HOST computes that digest under ([`super::super::digest`]). The keyed digest and
// the plain one are both instantiated over one of these and over nothing else.
var WEB_DIGESTS = {
    'SHA-1': 'sha1',
    'SHA-256': 'sha256',
    'SHA-384': 'sha384',
    'SHA-512': 'sha512'
};

// The block length of each of those hashes, in bytes, which is the length of a keyed-digest
// key a generating call states none for: the construction's own block.
var WEB_DIGEST_BLOCK_BYTES = { sha1: 64, sha256: 64, sha384: 128, sha512: 128 };

// The host's name for the hash a WebCrypto call names, which is the one computation behind
// both `digest` and the keyed digest below.
function webDigestName(hash) {
    var named = WEB_DIGESTS[webAlgorithmName(hash)];
    if (named === undefined) {
        throw unrecognizedAlgorithm();
    }
    return named;
}

// The key sizes AES is defined for, in bytes — the ones the host's own ciphers are named for
// ([`super::super::cipher::CIPHERS`]).
var AES_KEY_BYTES = [16, 24, 32];

// The refusal key DATA whose length its algorithm is not defined for gets: the length of an
// IMPORTED key is a property of the data handed over, so a size no transform takes is a fault
// of that data.
function invalidKeyLength() {
    var error = new Error('Invalid key length');
    error.name = 'DataError';
    return error;
}

// The cipher one WebCrypto algorithm names, for a key of that many bytes: the two names
// speak of the same transforms, so one table maps between them. A key that is not one of the
// sizes the cipher is defined for names no transform; whose fault that is depends on where
// the length came from, so the caller states it — `sizeFailure` for a length the CALL chose
// (a generated key), and the key-data refusal by default.
function webCipherName(algorithm, keyBytes, sizeFailure) {
    var named = webAlgorithmName(algorithm);
    if (named !== 'AES-GCM' && named !== 'AES-CBC' && named !== 'AES-CTR') {
        throw unrecognizedAlgorithm();
    }
    if (AES_KEY_BYTES.indexOf(keyBytes) < 0) {
        throw typeof sizeFailure === 'function' ? sizeFailure() : invalidKeyLength();
    }
    return 'aes-' + (keyBytes * 8) + '-' + named.slice('AES-'.length).toLowerCase();
}

// A key a WebCrypto call holds: the raw bytes, with the algorithm they belong to and what the
// key may be used for. The bytes are held on the key and nowhere else, which is what
// `exportKey` reads back.
function SubtleKey(algorithm, extractable, usages, bytes) {
    this.type = 'secret';
    this.algorithm = algorithm;
    this.extractable = extractable !== false;
    this.usages = usages === undefined || usages === null ? [] : usages;
    this.keyBytes = bytes;
}

// The algorithm a secret key REPORTS, as the interface normalises it: the name, the key's own
// length in bits, and — for a keyed digest, which is instantiated over a hash — the hash the
// key is held for, so every later call on that key computes the same construction.
function webKeyAlgorithm(algorithm, keyBytes) {
    var named = webAlgorithmName(algorithm);
    if (named === 'HMAC') {
        return {
            name: 'HMAC',
            hash: { name: webAlgorithmName(algorithm.hash) },
            length: keyBytes * 8
        };
    }
    return { name: named, length: keyBytes * 8 };
}

// What a key may be used FOR: a key is held for ONE algorithm and for one set of operations,
// so a call is refused when the key it was handed belongs to another algorithm or does not
// carry the usage that call needs — the one access failure the interface names for either.
// `named` is the algorithm the key must be held for, where the call has one to state.
function usableFor(key, usage, named) {
    var refused = new Error('Unable to use this key to ' + usage);
    refused.name = 'InvalidAccessError';
    var held = key === null || key === undefined || key.algorithm === null
        || key.algorithm === undefined
        ? null
        : key.algorithm;
    if (named !== undefined && (held === null || webAlgorithmName(held) !== named)) {
        throw refused;
    }
    var usages = key === null || key === undefined || key.usages === null
        || key.usages === undefined
        ? []
        : key.usages;
    for (var index = 0; index < usages.length; index += 1) {
        if (String(usages[index]) === usage) {
            return;
        }
    }
    throw refused;
}

// The nonce or counter a WebCrypto AES call carries, by the member that mode names it in.
function webCipherIv(algorithm, cipher) {
    if (cipher.slice(-4) === '-ctr') {
        return Buffer.from(algorithm.counter);
    }
    return Buffer.from(algorithm.iv);
}

// How long the tag of an authenticated WebCrypto call is, in bytes: the mode's full tag,
// which is the only length this runtime computes.
var WEB_TAG_BYTES = 16;

function webTransform(algorithm, key, data, decrypting) {
    var cipher = webCipherName(algorithm, key.keyBytes.length);
    var authenticated = cipher.slice(-4) === '-gcm';
    if (authenticated
        && algorithm.tagLength !== undefined
        && Number(algorithm.tagLength) !== WEB_TAG_BYTES * 8) {
        var unsupported = new Error('Unsupported tag length');
        unsupported.name = 'NotSupportedError';
        return Promise.reject(unsupported);
    }
    var bytes = Buffer.from(data);
    var tag = Buffer.alloc(0);
    // A WebCrypto ciphertext carries the tag of an authenticated mode APPENDED to it, where
    // the node cipher API hands it over separately.
    if (authenticated && decrypting) {
        if (bytes.length < WEB_TAG_BYTES) {
            return Promise.reject(operationFailed());
        }
        tag = Buffer.from(bytes.subarray(bytes.length - WEB_TAG_BYTES));
        bytes = Buffer.from(bytes.subarray(0, bytes.length - WEB_TAG_BYTES));
    }
    var aad = authenticated && algorithm.additionalData !== undefined
        && algorithm.additionalData !== null
        ? Buffer.from(algorithm.additionalData)
        : Buffer.alloc(0);
    var answered = host().cipher(
        [
            cipher,
            Buffer.from(key.keyBytes).toString('hex'),
            webCipherIv(algorithm, cipher).toString('hex'),
            aad.toString('hex'),
            bytes.toString('hex'),
            tag.toString('hex')
        ],
        decrypting
    );
    if (answered[0] !== 'ok') {
        // A message that did not open is the transform's own failure, which the interface
        // reports as its one operation-failure class ([`operationFailed`]).
        var failed = answered[0] === 'unopened'
            ? operationFailed()
            : cipherFailure(answered[0], cipher);
        return Promise.reject(failed);
    }
    var out = Buffer.from(answered[1], 'hex');
    if (authenticated && !decrypting) {
        out = Buffer.concat([out, Buffer.from(answered[2], 'hex')]);
    }
    return Promise.resolve(arrayBufferOf(out));
}

// The keyed digest a signing call computes, as the bytes of the MAC: the construction is the
// host's ([`super::super::digest`]), instantiated over the hash the KEY is held for, so a
// signature this interface produces is the one `createHmac` computes for the same key and
// message — and the one any other implementation of the construction computes.
//
// The key is already known to be held for this algorithm ([`usableFor`]), so the hash is the
// key's own: a key held for one hash is not signed over with another.
function webSignature(algorithm, key, data) {
    if (webAlgorithmName(algorithm) !== 'HMAC') {
        throw unrecognizedAlgorithm();
    }
    var hashed = webDigestName(key.algorithm.hash);
    return Buffer.from(
        host().hmac(
            hashed,
            Buffer.from(key.keyBytes).toString('hex'),
            Buffer.from(data).toString('hex')
        ),
        'hex'
    );
}

webcrypto.subtle.digest = function (algorithm, data) {
    try {
        var hashed = host().digest(
            webDigestName(algorithm),
            Buffer.from(data).toString('hex')
        );
        return Promise.resolve(arrayBufferOf(Buffer.from(hashed, 'hex')));
    } catch (error) {
        return Promise.reject(error);
    }
};

webcrypto.subtle.importKey = function (format, keyData, algorithm, extractable, usages) {
    if (String(format) !== 'raw') {
        var unsupported = new Error('Unsupported key format');
        unsupported.name = 'NotSupportedError';
        return Promise.reject(unsupported);
    }
    var bytes = Buffer.from(keyData);
    try {
        // A key is held for ONE algorithm, so the algorithm is resolved as the key is
        // imported: a keyed digest is instantiated over its hash, and a cipher takes a key
        // of one of the sizes it is defined for.
        if (webAlgorithmName(algorithm) === 'HMAC') {
            webDigestName(algorithm.hash);
        } else {
            webCipherName(algorithm, bytes.length);
        }
        return Promise.resolve(new SubtleKey(
            webKeyAlgorithm(algorithm, bytes.length),
            extractable,
            usages,
            bytes
        ));
    } catch (error) {
        return Promise.reject(error);
    }
};

webcrypto.subtle.generateKey = function (algorithm, extractable, usages) {
    try {
        // The length a call STATES, in bits; `null` when it states none, which each
        // algorithm answers for itself below.
        var stated = algorithm !== null && algorithm !== undefined
            && typeof algorithm !== 'string' && algorithm.length !== undefined
            && algorithm.length !== null
            ? Math.trunc(Number(algorithm.length))
            : null;
        if (webAlgorithmName(algorithm) === 'HMAC') {
            var hashed = webDigestName(algorithm.hash);
            // A keyed-digest key a call states no length for is the hash's own BLOCK, which
            // is the length the construction is defined over.
            var keyBits = stated === null ? WEB_DIGEST_BLOCK_BYTES[hashed] * 8 : stated;
            if (!Number.isSafeInteger(keyBits) || keyBits <= 0 || keyBits % 8 !== 0) {
                // A GENERATED key's length is a parameter of the CALL rather than a
                // property of key data handed over, so a length no key can be drawn at is
                // the operation's own failure.
                throw operationFailed();
            }
            return Promise.resolve(new SubtleKey(
                webKeyAlgorithm({ name: 'HMAC', hash: algorithm.hash }, keyBits / 8),
                extractable,
                usages,
                randomBytes(keyBits / 8)
            ));
        }
        // A cipher key is DRAWN at the size the call asks for, so a call that states none —
        // or one that is not whole bytes — names no key to draw. That length is the CALL's,
        // so a size the cipher is not defined for is the operation's own failure rather than
        // a fault of key data, while the ALGORITHM is resolved first and refused by name.
        var keyBytes = stated === null ? -1 : stated / 8;
        webCipherName(algorithm, keyBytes, operationFailed);
        return Promise.resolve(new SubtleKey(
            webKeyAlgorithm(algorithm, keyBytes),
            extractable,
            usages,
            randomBytes(keyBytes)
        ));
    } catch (error) {
        return Promise.reject(error);
    }
};

webcrypto.subtle.sign = function (algorithm, key, data) {
    try {
        usableFor(key, 'sign', 'HMAC');
        return Promise.resolve(arrayBufferOf(webSignature(algorithm, key, data)));
    } catch (error) {
        return Promise.reject(error);
    }
};

webcrypto.subtle.verify = function (algorithm, key, signature, data) {
    try {
        usableFor(key, 'verify', 'HMAC');
        var computed = webSignature(algorithm, key, data);
        var given = Buffer.from(signature);
        // A signature of another length is simply not this one; one of the same length is
        // compared without leaking where it first differs.
        return Promise.resolve(
            given.length === computed.length && timingSafeEqual(given, computed)
        );
    } catch (error) {
        return Promise.reject(error);
    }
};

webcrypto.subtle.exportKey = function (format, key) {
    if (String(format) !== 'raw') {
        var unsupported = new Error('Unsupported key format');
        unsupported.name = 'NotSupportedError';
        return Promise.reject(unsupported);
    }
    if (key.extractable === false) {
        var refused = new Error('key is not extractable');
        refused.name = 'InvalidAccessError';
        return Promise.reject(refused);
    }
    return Promise.resolve(arrayBufferOf(Buffer.from(key.keyBytes)));
};

webcrypto.subtle.encrypt = function (algorithm, key, data) {
    try {
        usableFor(key, 'encrypt');
        return webTransform(algorithm, key, data, false);
    } catch (error) {
        return Promise.reject(error);
    }
};

webcrypto.subtle.decrypt = function (algorithm, key, data) {
    try {
        usableFor(key, 'decrypt');
        return webTransform(algorithm, key, data, true);
    } catch (error) {
        return Promise.reject(error);
    }
};

exports.createHash = function (algorithm) { return new Hash(algorithm); };
exports.createHmac = function (algorithm, key) { return new Hmac(algorithm, key); };
exports.createCipheriv = function (algorithm, key, iv) {
    return new Cipher(algorithm, key, iv, false);
};
exports.createDecipheriv = function (algorithm, key, iv) {
    return new Cipher(algorithm, key, iv, true);
};
exports.getCiphers = function () { return ciphers().slice(); };
exports.hkdfSync = function (digest, ikm, salt, info, keylen) {
    var algorithm = algorithmOf(digest);
    var ceiling = 255 * DIGEST_BYTES[algorithm];
    var length = Math.trunc(Number(keylen));
    if (!Number.isSafeInteger(length) || length < 0 || length > ceiling) {
        var error = new RangeError('The value of "length" is out of range. It must be >= 0 && <= '
            + ceiling + '. Received ' + String(keylen));
        error.code = 'ERR_OUT_OF_RANGE';
        throw error;
    }
    var derived = host().hkdf(
        [algorithm, hexOf(ikm), hexOf(salt), hexOf(info)],
        length
    );
    // The construction answers bytes, and this call answers the ArrayBuffer Node's own
    // `hkdfSync` hands back.
    return arrayBufferOf(Buffer.from(derived, 'hex'));
};
exports.hkdf = function (digest, ikm, salt, info, keylen, callback) {
    var done = typeof callback === 'function' ? callback : function () { return undefined; };
    var derived = null;
    try {
        derived = exports.hkdfSync(digest, ikm, salt, info, keylen);
    } catch (error) {
        Promise.resolve().then(function () { done(error); });
        return undefined;
    }
    Promise.resolve().then(function () { done(null, derived); });
    return undefined;
};
// "Provides a synchronous Password-Based Key Derivation Function 2 (PBKDF2)
// implementation": the iterated keyed digest of PKCS #5, computed by the host over the same
// digests as the keyed digest above ([`super::super::digest::pbkdf2`]). The iteration count
// and the key length are bounded on BOTH sides of the host call, as the derivation above is
// and for the same reason: the work and the bytes are the host's.
exports.pbkdf2Sync = function (password, salt, iterations, keylen, digest) {
    var algorithm = algorithmOf(digest === undefined ? 'sha1' : digest);
    var rounds = Math.trunc(Number(iterations));
    var length = Math.trunc(Number(keylen));
    if (!Number.isSafeInteger(rounds) || rounds < 1 || rounds > MAX_PBKDF2_ITERATIONS) {
        var counted = new RangeError('The value of "iterations" is out of range. It must be '
            + '>= 1 && <= ' + MAX_PBKDF2_ITERATIONS + '. Received ' + String(iterations));
        counted.code = 'ERR_OUT_OF_RANGE';
        throw counted;
    }
    var ceiling = MAX_DERIVED_BYTES;
    if (!Number.isSafeInteger(length) || length < 0 || length > ceiling) {
        var sized = new RangeError('The value of "keylen" is out of range. It must be >= 0 '
            + '&& <= ' + ceiling + '. Received ' + String(keylen));
        sized.code = 'ERR_OUT_OF_RANGE';
        throw sized;
    }
    // One block of key per digest length, each of them the whole iteration count: what the
    // host bounds is that product, so it is checked here too.
    var blocks = Math.max(1, Math.ceil(length / DIGEST_BYTES[algorithm]));
    if (blocks * rounds > MAX_PBKDF2_ROUNDS) {
        var work = new RangeError('The value of "iterations" is out of range. It must be '
            + '>= 1 && <= ' + Math.floor(MAX_PBKDF2_ROUNDS / blocks) + ' for a key of '
            + length + ' bytes. Received ' + String(iterations));
        work.code = 'ERR_OUT_OF_RANGE';
        throw work;
    }
    var derived = host().pbkdf2(
        [algorithm, hexOf(password), hexOf(salt)],
        rounds,
        length
    );
    return Buffer.from(derived, 'hex');
};
exports.pbkdf2 = function (password, salt, iterations, keylen, digest, callback) {
    var done = typeof callback === 'function' ? callback : function () { return undefined; };
    var derived = null;
    try {
        derived = exports.pbkdf2Sync(password, salt, iterations, keylen, digest);
    } catch (error) {
        Promise.resolve().then(function () { done(error); });
        return undefined;
    }
    Promise.resolve().then(function () { done(null, derived); });
    return undefined;
};
exports.Cipheriv = Cipher;
exports.Decipheriv = Cipher;
exports.Hash = Hash;
exports.Hmac = Hmac;
exports.getHashes = function () { return ALGORITHMS.slice(); };
exports.randomBytes = randomBytes;
exports.pseudoRandomBytes = randomBytes;
exports.randomFillSync = randomFillSync;
exports.randomFill = function (target, offset, size, callback) {
    var done = typeof offset === 'function' ? offset
        : (typeof size === 'function' ? size : callback);
    var at = typeof offset === 'function' ? 0 : offset;
    var count = typeof size === 'function' || size === undefined ? undefined : size;
    var filled = randomFillSync(target, at, count);
    Promise.resolve().then(function () { done(null, filled); });
};
exports.randomUUID = randomUUID;
exports.randomInt = randomInt;
exports.timingSafeEqual = timingSafeEqual;
exports.getRandomValues = getRandomValues;
exports.webcrypto = webcrypto;
exports.subtle = webcrypto.subtle;
exports.constants = {};
"##;
