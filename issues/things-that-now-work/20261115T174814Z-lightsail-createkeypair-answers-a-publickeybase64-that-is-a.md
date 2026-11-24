Issue-Source: trace-sweep-divergence
Issue-Trace: lightsail-catalog-and-operations-lifecycle
Issue-Step: setup#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lightsail-catalog-and-operations-lifecycle
Issue-Shape-Id: com.amazonaws.lightsail#Lightsail_20161128
Issue-Key: lightsail|CreateKeyPair|2xx->2xx|$.publicKeyBase64
Issue-Commit: 342c23c982e836d88694781a4c8bb7d7dc4be12f

# Lightsail CreateKeyPair answers a publicKeyBase64 that is a bare SSH blob, not the ssh-rsa public key AWS returns

> _Note: this issue was identified against the code base at commit `342c23c982e836d88694781a4c8bb7d7dc4be12f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateKeyPair

## Description
One root cause (Cause 1 of 1), on the lightsail service.

AWS: Smithy com.amazonaws.lightsail#CreateKeyPair — output members keyPair (KeyPair), publicKeyBase64 (Base64), privateKeyBase64 (Base64), operation (Operation). The model documents CreateKeyPairResult$publicKeyBase64 as "A base64-encoded public key of the ssh-rsa type". Beyond that the value is recorded behavior only: the recorded 200 response carries a publicKeyBase64 whose concrete bytes are per-run, so the corpus asserts its SHAPE rather than its value (expected is the redacted marker '<shaped-not-asserted>'), and the emulator's value does not satisfy that shape. Hypothesis: real Lightsail returns the OpenSSH public-key rendering — the algorithm name "ssh-rsa", a space, then the base64 blob (the authorized_keys body) — whereas the emulator returns the blob alone, so the shape class differs by the "ssh-rsa " prefix. The requirement is the FORM of the member (an ssh-rsa public key a caller can use directly), never the recorded key bytes, which are freshly generated on every run and are not part of it.
Emulator: the replayed 200 answered publicKeyBase64 as a bare base64 blob beginning 'AAAAB3NzaC1yc2EAAAADAQAB...' with no algorithm prefix and no whitespace. This is deliberate in the crate: services/service-com-amazonaws-lightsail-lightsail-20161128/src/state/key_pairs.rs lines 231-251 (base64_rsa_public_key) builds the OpenSSH line with ssh_key::PublicKey::to_openssh() and then CUTS the body out of it via openssh_key_body(), and the doc comment on MintedKeyPairMaterial::public_key (lines 71-83) states the value is to carry "no key-type prefix and no whitespace". src/ops/create_key_pair.rs line 284 hands that string straight to the output member.
Why: this is the one call that ever reports the pair's public half, and a caller's normal use of it is to write it into an authorized_keys file, hand it to an instance provisioner, or re-import it with ImportKeyPair. A value missing the algorithm prefix is not a valid authorized_keys entry and is rejected by OpenSSH and by anything that parses an SSH public key, so a flow that works against AWS fails against the emulator; a client that merely base64-decodes the value still sees a different string than AWS returned.
Fix: the serialization of CreateKeyPair's publicKeyBase64 — i.e. the public-half rendering in src/state/key_pairs.rs (base64_rsa_public_key) that create_key_pair.rs reports, so the member carries the ssh-rsa public key in the form AWS returns. The same rendering is reused for the account default pair (HeldDefaultKeyPairMaterial::public_key) and for the reported SSH host key, so the fix must decide each of those deliberately rather than changing one caller.
Traces: lightsail-catalog-and-operations-lifecycle#setup#1 (CreateKeyPair, HTTP 200, $.publicKeyBase64).

No open issue owns this step: the replay reports owned_by empty and stub_path empty for this cluster. Open issue 20261022T050650Z-lightsail-createkeypair-stub.md is a stub-implementation issue for the same operation and is NOT this finding — the operation now answers 200 with real key material, and only the publicKeyBase64 rendering diverges. Open issue 20261024T010506Z-lightsail-importkeypair-stub.md is the related unimplemented import path.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 34 steps, 16 passed, 18 mismatched, score 47.1%.

lightsail-catalog-and-operations-lifecycle#setup#1, CreateKeyPair, verdict mismatch, expected_status 200, actual_status 200, diverging key $.publicKeyBase64:
  $.publicKeyBase64: value_mismatch (expected '<shaped-not-asserted>', got 'AAAAB3NzaC1yc2EAAAADAQABAAABAQCps1ppZt5zJJStEIpqOrFvZeVHQ2NQ+O+We90bFBDWTH08cLpr3xbi0y35+uj9050MtH7d8ObouGnvZceOjZ5diRga56yEsSZyoceWdAOGInhZ6bZ1HYDIDUsQMx3oxnPe3eRwBvC5A4sxTMZKDiS5Nx8tgO0NrEI+4xdUJlH0PM5jiIKU+cGYHbLt3zssjEeelueT8JJyabVEiLAZ1p0SFI2Upwn3Ed9bLpvHlpBs9JeE1YpC2yGuP00+nslfwLEOzi2sAIyfXW0rc5cUMm5ey/cXOB5ofOx6iDHtV4bZVHIWSCJ2GEMPgLsiezalr8utm1X/FedR+T/cAKYXLDdv')

The recorded value is redacted to the shape marker because the key bytes are per-run; the emulator's value is the one compared and it fails that shape. The actual string above is this run's freshly minted key and is NOT a required value — note only that it begins with the base64 of an ssh-rsa blob and carries no "ssh-rsa " prefix. Every other member of this response (keyPair, privateKeyBase64, operation) compared equal, so the divergence is confined to this member's rendering.

In-checkout source of the rendering: services/service-com-amazonaws-lightsail-lightsail-20161128/src/state/key_pairs.rs lines 244-251 (to_openssh() then openssh_key_body() strips the prefix) and lines 71-83 (the doc comment asserting "no key-type prefix and no whitespace"); consumed at services/service-com-amazonaws-lightsail-lightsail-20161128/src/ops/create_key_pair.rs line 284.

## Steps to Reproduce
Boot the emulator and point a boto3 lightsail client at its data listener in the emulator's region.

1. resp = ls.create_key_pair(keyPairName="probe-keypair")
2. Inspect resp["publicKeyBase64"]. Against AWS this is an ssh-rsa public key (it names its algorithm, "ssh-rsa " followed by the base64 blob) and can be appended to an authorized_keys file as-is. Observed against the emulator: the base64 blob alone, e.g. starting 'AAAAB3NzaC1yc2EAAAADAQAB...', with no algorithm prefix.
3. Confirm the practical consequence: write resp["publicKeyBase64"] to a file and run `ssh-keygen -l -f <file>` — it fails on the emulator's value and succeeds on AWS's.

Re-replay to confirm the fix: lightsail-catalog-and-operations-lifecycle#setup#1.
