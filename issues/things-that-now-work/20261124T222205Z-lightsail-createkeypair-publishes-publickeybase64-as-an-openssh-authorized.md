Issue-Source: trace-sweep-divergence
Issue-Trace: lightsail-catalog-and-operations-lifecycle
Issue-Step: setup#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lightsail-catalog-and-operations-lifecycle
Issue-Shape-Id: com.amazonaws.lightsail#Lightsail_20161128
Issue-Key: lightsail|CreateKeyPair|2xx->2xx|$.publicKeyBase64
Issue-Commit: 455b877557af92974b1e1b0549e81b06a05b022f

# Lightsail CreateKeyPair publishes publicKeyBase64 as an OpenSSH authorized_keys line instead of the base64-encoded key

> _Note: this issue was identified against the code base at commit `455b877557af92974b1e1b0549e81b06a05b022f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateKeyPair

## Description
Cause 1 of 1: `CreateKeyPair` answers 200 but the `publicKeyBase64` member of the result carries an OpenSSH public-key LINE rather than a base64-encoded key, so the value fails the shape the recorded AWS response's value has.

AWS: Smithy model `com.amazonaws.lightsail#CreateKeyPair` -> `CreateKeyPairResult$publicKeyBase64`, target `com.amazonaws.lightsail#Base64` (plain string, no pattern), documented as "A base64-encoded public key of the ssh-rsa type." Recorded behavior only for the value's exact bytes: the fixture does not assert the literal key material (it is per-run), it asserts the value's SHAPE, and the recorded AWS response's `publicKeyBase64` is the base64 encoding of the key alone — a single base64 token with no algorithm label, no space and no trailing whitespace. The response status is 200 in both the recording and the emulator; no error code or error message is involved in this divergence.

Emulator: it returns the whole `authorized_keys` rendering of the key: the algorithm name `ssh-rsa`, a space, then the base64 body (replay: `$.publicKeyBase64: value_mismatch (expected '<shaped-not-asserted>', got 'ssh-rsa AAAAB3NzaC1yc2E…')`). That is a deliberate choice in the service's key-pair state module, which documents publishing "its algorithm name, a space, and the base64 of the key's own ssh-rsa encoding" — i.e. a two-field line, not a base64 value. The same rendering is held for the account's default pair, so `DownloadDefaultKeyPair` reports the public half the same non-conforming way.

Why: a client that treats the member as what it is named and documented to be — base64 — and decodes it (base64-decode to the RFC 4253 `ssh-rsa` key blob, to compute a fingerprint, to re-encode the key, or to store it in a field validated as base64) gets a decode failure on the leading `ssh-rsa ` label and the embedded space, so key handling after a create fails against the emulator while it succeeds against AWS. Conversely, a client that writes the value straight into an `authorized_keys` file works against the emulator and produces a malformed line against AWS. The value is also the one a caller feeds back to `ImportKeyPair`, so the two sides of a round trip disagree about the form.

Fix: the serialization of the public half in the service's key-pair state, i.e. the helper that renders a minted key's public half for `CreateKeyPairResult$publicKeyBase64` (and the identically documented `DownloadDefaultKeyPairResult$publicKeyBase64`), not the `CreateKeyPair` handler's control flow — the handler simply moves the minted value into the output. The emitted value must be the base64 of the key's own `ssh-rsa` encoding with no algorithm label, no separating space and no surrounding whitespace. `ImportKeyPair` already accepts both renderings, so narrowing what is EMITTED does not narrow what is accepted.

Note: open issue `20261022T050650Z-lightsail-createkeypair-stub.md` ("Implement lightsail CreateKeyPair: unimplemented scaffold stub") is about the operation answering 501 as an unimplemented scaffold. It does not cover this: the operation is now implemented and answers 200, and that issue carries no replay key for this fixture step.

Traces: lightsail-catalog-and-operations-lifecycle#setup#1 (CreateKeyPair, expected status 200, actual 200, key `$.publicKeyBase64`).

## Evidence
Replay of fixture `lightsail-catalog-and-operations-lifecycle`, step `setup#1`, operation CreateKeyPair, verdict mismatch, expected_status 200 / actual_status 200.

Divergence detail, verbatim:
  $.publicKeyBase64: value_mismatch (expected '<shaped-not-asserted>', got 'ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQCmo06U39Kq3AZRrmtkRq7yLs1chZR01K2f21iTFiPeHAbhX8J/OtU8ZSnEIXcImCnS8ndDAmMp/jkfU3duLAB5vt3qNzqVoAv8jUtSC1FvM9QZFV9INfE+iQsEv3zBV8VsvOd417zNOoruYamc7JRyv7tdvF8dv+45xpPkosMd1EPoTEL6EqGVjsvQyq/atuMqbbfbFU6+hfFUfKCv2W7lZJbMAlpWTulDbIPk4vX1+AGMKcBsyB/nYY0H9YH+cM9x1WVGjiWhEP+6n99pcT7ibM6PsY6CGW7PjftAk9FGYx1Luy/kcUJE4yx3uWd0EaGafshm0xeNxKuWjVaQr2p9')

Expected (AWS, recorded): a single base64 token — the base64 of the key's `ssh-rsa` encoding, no algorithm label, no space, no trailing whitespace. The key bytes themselves are per-run material and are NOT the requirement; the form is.
Actual (emulator): `<algorithm> <base64-body>` — an OpenSSH public-key line.

Model citation: smithy_query(query="doc", shape="CreateKeyPairResult", member="publicKeyBase64") -> "A base64-encoded public key of the ssh-rsa type."; smithy_query(query="shape", shape="Base64") -> plain string, no pattern.
Source citations: services/service-com-amazonaws-lightsail-lightsail-20161128/src/state/key_pairs.rs lines 350-360 (`ssh_rsa_public_key` formats `"{SSH_RSA_ALGORITHM} {body}"`), lines 71-94 (`MintedKeyPairMaterial::public_key` documents the line form), lines 263-272 (the default pair holds the same form); services/service-com-amazonaws-lightsail-lightsail-20161128/src/ops/create_key_pair.rs line 284 (`public_key_base64: Some(minted.public_key)`).

## Steps to Reproduce
1. Boot the emulator built from this checkout.
2. Replay fixture `lightsail-catalog-and-operations-lifecycle`, step `setup#1` — a Lightsail `CreateKeyPair` call with a fresh `keyPairName`.
3. Read `publicKeyBase64` from the 200 response and base64-decode it: against AWS it decodes to the RFC 4253 `ssh-rsa` public key blob; against the emulator the value begins `ssh-rsa ` and does not decode.
4. Equivalently: assert the value matches the base64 alphabet only (no whitespace, no `-`). The emulator's value fails that assertion, which is the shape the recorded response satisfies.
