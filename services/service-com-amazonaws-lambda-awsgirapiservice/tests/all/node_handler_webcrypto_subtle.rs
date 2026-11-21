// SPDX-License-Identifier: CC0-1.0

//! The WHATWG `SubtleCrypto` surface a managed Node.js handler reaches through the GLOBAL
//! `crypto.subtle`, which is the supported way a modern Node program does cryptography.
//!
//! The rule pinned here: a managed Node.js runtime's global `crypto.subtle` implements the
//! standard algorithm registry for the algorithms the runtime computes — `importKey`,
//! `generateKey`, `exportKey`, `sign`, `verify`, `digest`, `encrypt` and `decrypt` over the
//! keyed digest (`HMAC` instantiated over one of the registry's hashes) and the AES modes —
//! and each of them COMPUTES what the call names: a keyed digest is the construction's own
//! value, so one computed inside a handler equals the published vector for the same key and
//! message; an AES-GCM seal opens back to the plaintext it was given; and a name the registry
//! does not carry is refused BY NAME rather than answered with another algorithm's
//! computation.
//!
//! The rows differ along what each rule generalises over: the hash a keyed digest is
//! instantiated over (every hash of the registry), the key length (shorter than, equal to and
//! longer than the hash's block), the key size and the additional data of a seal, which part
//! of a sealed message is tampered with, and the class of each refusal. Every row RETURNS a
//! computed value, which is what proves the computation happened rather than a shape being
//! echoed.

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` this function carries: the newest Node.js runtime the model declares a value
/// for (`smithy_query(query="shape", shape="Runtime")`), whose language surface is a stock
/// Node.js of that major version.
const RUNTIME: &str = "nodejs24.x";

/// The function every row of the table runs in.
const FUNCTION: &str = "webcrypto-subtle";

/// Lambda alone: nothing here reaches another service.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names Lambda, which is what routes the
/// request. The signature is never checked here.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/lambda/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda `restJson1` call, as its status, headers and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// How long this suite waits for the function to become `Active`: the transition is owed by
/// the CLOCK, so the wait is for that CONDITION and the deadline is generous — a loaded host
/// may take far longer than an idle one.
const ACTIVE_BUDGET: Duration = Duration::from_secs(60);

/// How often the wait above looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Wait until `name` reports `State` `Active`, which is when an invocation reaches it.
async fn await_active(router: &MultiServiceRouter, name: &str) {
    let deadline = std::time::Instant::now() + ACTIVE_BUDGET;
    loop {
        let (_status, _headers, read) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
        )
        .await;
        if read["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the function {name:?} never became Active within {ACTIVE_BUDGET:?}, got {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A .zip deployment package holding `source` as `index.mjs`, and nothing else.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("index.mjs", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The handler: it reaches `crypto.subtle` through the GLOBAL `crypto` — no import at all,
/// which is how a modern Node program reaches WebCrypto — and answers what every row of the
/// event computed, so one run answers the whole table.
const HANDLER: &str = r#"
const { subtle } = globalThis.crypto;

const bytes = (hex) => Buffer.from(hex, "hex");
const hex = (buffer) => Buffer.from(buffer).toString("hex");

// One byte of `buffer` flipped: a tampered copy of it, counted from the end for a negative
// position so a row can name the tag of a sealed message.
const flipped = (buffer, at) => {
    const copy = Buffer.from(buffer);
    const index = at < 0 ? copy.length + at : at;
    copy[index] ^= 0x01;
    return copy;
};

// What a call that must be REFUSED answers with: the class of the refusal, or the fact that
// it was answered instead.
const refused = async (run) => {
    try {
        await run();
        return "answered";
    } catch (error) {
        return error.name;
    }
};

// The calls that must be refused, by what each of them gets wrong.
const REFUSALS = {
    "a hash the registry does not carry": () => subtle.digest("MD5", bytes("00")),
    "an algorithm this runtime does not implement": () => subtle.importKey(
        "raw", bytes("00".repeat(32)), { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false,
        ["sign"]
    ),
    "a keyed digest over a hash the registry does not carry": () => subtle.importKey(
        "raw", bytes("00".repeat(32)), { name: "HMAC", hash: "SHA3-256" }, false, ["sign"]
    ),
    "a cipher key of a size the cipher is not defined for": () => subtle.importKey(
        "raw", bytes("00".repeat(20)), "AES-GCM", false, ["encrypt"]
    ),
    "a key used for an operation it was not imported for": async () => {
        const key = await subtle.importKey(
            "raw", bytes("00".repeat(32)), { name: "HMAC", hash: "SHA-256" }, false, ["verify"]
        );
        return subtle.sign("HMAC", key, bytes("00"));
    },
    "a signing algorithm that is not the keyed digest": async () => {
        const key = await subtle.importKey(
            "raw", bytes("00".repeat(32)), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]
        );
        return subtle.sign("AES-GCM", key, bytes("00"));
    },
    "a key held for another algorithm than the call names": async () => {
        const key = await subtle.importKey(
            "raw", bytes("00".repeat(32)), "AES-GCM", false, ["sign"]
        );
        return subtle.sign({ name: "HMAC", hash: "SHA-256" }, key, bytes("00"));
    },
    "a sealed message shorter than the tag it must carry": async () => {
        const key = await subtle.importKey(
            "raw", bytes("00".repeat(32)), "AES-GCM", false, ["decrypt"]
        );
        return subtle.decrypt({ name: "AES-GCM", iv: bytes("00".repeat(12)) }, key, bytes("0011"));
    },
};

export const handler = async (event) => {
    // The methods the interface carries, so a missing one is reported as that rather than as
    // a failure inside a row.
    const surface = {};
    for (const method of event.surface) {
        surface[method] = typeof subtle[method];
    }

    const macs = {};
    for (const row of event.macs) {
        const key = await subtle.importKey(
            "raw", bytes(row.key), { name: "HMAC", hash: row.hash }, false, ["sign", "verify"]
        );
        const signature = await subtle.sign("HMAC", key, bytes(row.data));
        macs[row.case] = {
            mac: hex(signature),
            held: key.algorithm.name + "/" + key.algorithm.hash.name,
            verified: await subtle.verify("HMAC", key, signature, bytes(row.data)),
            // A signature one byte of which was flipped, and one of another length, are both
            // "not this signature" rather than errors.
            flipped: await subtle.verify("HMAC", key, flipped(signature, 0), bytes(row.data)),
            shortened: await subtle.verify(
                "HMAC", key, Buffer.from(signature).subarray(1), bytes(row.data)
            ),
            // The MAC covers the MESSAGE, so the signature of one message does not verify
            // another.
            otherMessage: await subtle.verify(
                "HMAC", key, signature, Buffer.concat([bytes(row.data), bytes("00")])
            ),
        };
    }

    const seals = {};
    for (const row of event.seals) {
        const key = await subtle.importKey(
            "raw", bytes(row.key), { name: "AES-GCM" }, true, ["encrypt", "decrypt"]
        );
        const other = await subtle.importKey(
            "raw", bytes(row.other), "AES-GCM", false, ["decrypt"]
        );
        const parameters = { name: "AES-GCM", iv: bytes(row.iv) };
        if (row.aad !== undefined) {
            parameters.additionalData = bytes(row.aad);
        }
        const sealed = await subtle.encrypt(parameters, key, bytes(row.plain));
        seals[row.case] = {
            opened: hex(await subtle.decrypt(parameters, key, sealed)),
            sealedBytes: sealed.byteLength,
            exported: hex(await subtle.exportKey("raw", key)),
            // The message and its tag are both authenticated, so a ciphertext tampered with
            // in either, or opened under another key, is REFUSED rather than answered.
            tamperedBytes: await refused(
                () => subtle.decrypt(parameters, key, flipped(sealed, 0))
            ),
            tamperedTag: await refused(
                () => subtle.decrypt(parameters, key, flipped(sealed, -1))
            ),
            otherKey: await refused(() => subtle.decrypt(parameters, other, sealed)),
        };
    }

    const digests = {};
    for (const row of event.digests) {
        digests[row.case] = hex(await subtle.digest(row.hash, bytes(row.data)));
    }

    const refusals = {};
    for (const probe of event.refusals) {
        refusals[probe] = await refused(REFUSALS[probe]);
    }

    // A key the interface GENERATES is a key of the length the algorithm asks for, drawn
    // rather than fixed, and usable for the computation it was generated for.
    const keyed = await subtle.generateKey({ name: "HMAC", hash: "SHA-512" }, true, ["sign", "verify"]);
    const cipherKey = await subtle.generateKey({ name: "AES-GCM", length: 192 }, true, ["encrypt", "decrypt"]);
    const again = await subtle.generateKey({ name: "AES-GCM", length: 192 }, true, ["encrypt"]);
    const signed = await subtle.sign("HMAC", keyed, bytes("00"));
    const generated = {
        keyedBytes: (await subtle.exportKey("raw", keyed)).byteLength,
        cipherBytes: (await subtle.exportKey("raw", cipherKey)).byteLength,
        drawn: hex(await subtle.exportKey("raw", cipherKey))
            !== hex(await subtle.exportKey("raw", again)),
        usable: await subtle.verify("HMAC", keyed, signed, bytes("00")),
        sized: (await subtle.generateKey({ name: "AES-GCM", length: 10 }, true, ["encrypt"])
            .then(() => "answered", (error) => error.name)),
    };

    return { surface, macs, seals, digests, refusals, generated };
};
"#;

/// The hex of `text`, which is how every byte string reaches the handler.
fn hex_of(text: &str) -> String {
    hex::encode(text.as_bytes())
}

/// One keyed-digest row: the case, the hash, the key and the message, and the MAC published
/// for them.
struct Mac {
    case: &'static str,
    hash: &'static str,
    key: String,
    data: String,
    mac: &'static str,
}

/// The published keyed-digest vectors every row below is asserted against.
///
/// Source: RFC 4231 §4 (the SHA-2 test cases of HMAC) and RFC 2202 §3 (the SHA-1 cases of the
/// same keys and messages) — the values every implementation of the construction agrees on,
/// so a MAC computed inside the handler is checked against something outside the emulator
/// rather than against the emulator itself. The rows span the hash (all four of the
/// registry's), and the key length either side of the hash's block: 20 bytes, 4 bytes, and
/// 131 bytes, which is longer than the block of any of them.
fn macs() -> Vec<Mac> {
    let one_key = "0b".repeat(20);
    let one_data = hex_of("Hi There");
    let two_key = hex_of("Jefe");
    let two_data = hex_of("what do ya want for nothing?");
    let long_key = "aa".repeat(131);
    let long_data = hex_of("Test Using Larger Than Block-Size Key - Hash Key First");
    vec![
        Mac {
            case: "RFC 2202 case 1 over SHA-1",
            hash: "SHA-1",
            key: one_key.clone(),
            data: one_data.clone(),
            mac: "b617318655057264e28bc0b6fb378c8ef146be00",
        },
        Mac {
            case: "RFC 4231 case 1 over SHA-256",
            hash: "SHA-256",
            key: one_key.clone(),
            data: one_data.clone(),
            mac: "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        },
        Mac {
            case: "RFC 4231 case 1 over SHA-384",
            hash: "SHA-384",
            key: one_key.clone(),
            data: one_data.clone(),
            mac: "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4\
                  af152e8b2fa9cb6",
        },
        Mac {
            case: "RFC 4231 case 1 over SHA-512",
            hash: "SHA-512",
            key: one_key,
            data: one_data,
            mac: "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702\
                  038b274eaea3f4e4be9d914eeb61f1702e696c203a126854",
        },
        Mac {
            case: "RFC 2202 case 2 over SHA-1, with a key shorter than the block",
            hash: "SHA-1",
            key: two_key.clone(),
            data: two_data.clone(),
            mac: "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79",
        },
        Mac {
            case: "RFC 4231 case 2 over SHA-256, with a key shorter than the block",
            hash: "SHA-256",
            key: two_key,
            data: two_data,
            mac: "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
        },
        Mac {
            case: "RFC 4231 case 6 over SHA-256, with a key longer than the block",
            hash: "SHA-256",
            key: long_key,
            data: long_data,
            mac: "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        },
    ]
}

/// One seal row: the case, the key and the key a tampering row opens with instead, the nonce,
/// the additional data (none for `None`), and the plaintext.
struct Seal {
    case: &'static str,
    key: String,
    other: String,
    iv: String,
    aad: Option<String>,
    plain: String,
}

/// The seal rows, spanning what the transform generalises over: each key size AES is defined
/// for (128, 192 and 256 bits), additional data present and absent, and a message that
/// carries bytes as well as one that carries none.
fn seals() -> Vec<Seal> {
    vec![
        Seal {
            case: "a 128-bit key and no additional data",
            key: "00112233445566778899aabbccddeeff".to_string(),
            other: "ff112233445566778899aabbccddeeff".to_string(),
            iv: "102030405060708090a0b0c0".to_string(),
            aad: None,
            plain: hex_of("the payload of one invocation"),
        },
        Seal {
            case: "a 192-bit key with additional data",
            key: "00".repeat(24),
            other: "ff".to_string() + &"00".repeat(23),
            iv: "0102030405060708090a0b0c".to_string(),
            aad: Some(hex_of("bound to this request")),
            plain: hex_of("another payload"),
        },
        Seal {
            case: "a 256-bit key with additional data",
            key: "0f".repeat(32),
            other: "f0".repeat(32),
            iv: "000000000000000000000001".to_string(),
            aad: Some(hex_of("bound")),
            plain: hex_of("851274"),
        },
        Seal {
            case: "a 256-bit key over a message of no bytes at all",
            key: "0f".repeat(32),
            other: "f0".repeat(32),
            iv: "000000000000000000000002".to_string(),
            aad: None,
            plain: String::new(),
        },
    ]
}

/// RULE: the global `crypto.subtle` implements the registry for the algorithms this runtime
/// computes, and every one of its calls computes what the call names — the published keyed
/// digest, the digest, the seal that opens back, and the refusal a name outside the registry
/// or a message that does not authenticate gets.
#[tokio::test]
async fn a_node_handlers_global_subtle_crypto_computes_what_it_names() {
    let router = router();
    let (status, _headers, created) = lambda(
        &router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": 60,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(HANDLER)),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    await_active(&router, FUNCTION).await;

    let macs = macs();
    let seals = seals();
    // Each digest row: the case, the hash, the message and the digest published for it.
    // Sources: the digest of the empty input each algorithm's own specification fixes (the
    // rows this crate's own host seam is pinned against) and the SHA-256 of "abc".
    let digests: &[(&str, &str, String, &str)] = &[
        (
            "SHA-1 over no bytes",
            "SHA-1",
            String::new(),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        ),
        (
            "SHA-256 over \"abc\"",
            "SHA-256",
            hex_of("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            "SHA-384 over no bytes",
            "SHA-384",
            String::new(),
            "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd5\
             1ad2f14898b95b",
        ),
        (
            "SHA-512 over no bytes",
            "SHA-512",
            String::new(),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff\
             8318d2877eec2f63b931bd47417a81a538327af927da3e",
        ),
    ];
    // Each refusal row: the probe the handler runs, and the class of refusal it must get.
    let refusals: &[(&str, &str)] = &[
        ("a hash the registry does not carry", "NotSupportedError"),
        (
            "an algorithm this runtime does not implement",
            "NotSupportedError",
        ),
        (
            "a keyed digest over a hash the registry does not carry",
            "NotSupportedError",
        ),
        (
            "a cipher key of a size the cipher is not defined for",
            "DataError",
        ),
        (
            "a key used for an operation it was not imported for",
            "InvalidAccessError",
        ),
        (
            "a signing algorithm that is not the keyed digest",
            "NotSupportedError",
        ),
        (
            "a key held for another algorithm than the call names",
            "InvalidAccessError",
        ),
        (
            "a sealed message shorter than the tag it must carry",
            "OperationError",
        ),
    ];
    // The methods of the interface this rule is about.
    let surface: &[&str] = &[
        "importKey",
        "exportKey",
        "generateKey",
        "sign",
        "verify",
        "digest",
        "encrypt",
        "decrypt",
    ];

    let event = json!({
        "surface": surface,
        "macs": macs
            .iter()
            .map(|row| json!({
                "case": row.case,
                "hash": row.hash,
                "key": row.key,
                "data": row.data,
            }))
            .collect::<Vec<Value>>(),
        "seals": seals
            .iter()
            .map(|row| {
                let mut stated = json!({
                    "case": row.case,
                    "key": row.key,
                    "other": row.other,
                    "iv": row.iv,
                    "plain": row.plain,
                });
                if let Some(aad) = row.aad.as_deref() {
                    stated["aad"] = json!(aad);
                }
                stated
            })
            .collect::<Vec<Value>>(),
        "digests": digests
            .iter()
            .map(|(case, hash, data, _)| json!({ "case": case, "hash": hash, "data": data }))
            .collect::<Vec<Value>>(),
        "refusals": refusals.iter().map(|(probe, _)| *probe).collect::<Vec<&str>>(),
    });

    let (status, headers, answers) = lambda(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from Invoke, got {status} ({answers})"
    );
    assert!(
        headers.get("x-amz-function-error").is_none(),
        "expected the run to answer, got the function error {:?} with {answers}",
        headers.get("x-amz-function-error")
    );

    for method in surface {
        let held = answers["surface"][*method].as_str();
        assert_eq!(
            held,
            Some("function"),
            "case {method:?}: expected crypto.subtle to carry the method, got {held:?}"
        );
    }
    for row in &macs {
        let held = &answers["macs"][row.case];
        let expected = json!({
            "mac": row.mac,
            "held": format!("HMAC/{}", row.hash),
            "verified": true,
            "flipped": false,
            "shortened": false,
            "otherMessage": false,
        });
        assert_eq!(
            held, &expected,
            "case {:?}: expected {expected}, got {held}",
            row.case
        );
    }
    for row in &seals {
        let held = &answers["seals"][row.case];
        // "The ciphertext carries the tag appended": a sealed message is the plaintext's own
        // bytes plus the mode's 128-bit tag.
        let sealed_bytes = row.plain.len() / 2 + 16;
        let expected = json!({
            "opened": row.plain,
            "sealedBytes": sealed_bytes,
            "exported": row.key,
            "tamperedBytes": "OperationError",
            "tamperedTag": "OperationError",
            "otherKey": "OperationError",
        });
        assert_eq!(
            held, &expected,
            "case {:?}: expected {expected}, got {held}",
            row.case
        );
    }
    for (case, _hash, _data, digest) in digests {
        let held = answers["digests"][*case].as_str();
        assert_eq!(
            held,
            Some(*digest),
            "case {case:?}: expected the published digest {digest:?}, got {held:?}"
        );
    }
    for (probe, expected) in refusals {
        let held = answers["refusals"][*probe].as_str();
        assert_eq!(
            held,
            Some(*expected),
            "case {probe:?}: expected the refusal {expected:?}, got {held:?}"
        );
    }
    let generated = json!({
        // A keyed-digest key a call states no length for is the hash's own block: 128 bytes
        // for SHA-512.
        "keyedBytes": 128,
        "cipherBytes": 24,
        "drawn": true,
        "usable": true,
        // A GENERATED key's length is a parameter of the call, so a length no key can be
        // drawn at is the operation's own failure rather than a fault of key data.
        "sized": "OperationError",
    });
    assert_eq!(
        answers["generated"], generated,
        "expected a generated key to be drawn at the length its algorithm asks for, got {}",
        answers["generated"]
    );
}
