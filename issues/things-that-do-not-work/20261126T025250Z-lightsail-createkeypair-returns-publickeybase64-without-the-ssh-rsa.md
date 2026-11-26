Issue-Source: trace-sweep-divergence
Issue-Trace: lightsail-catalog-and-operations-lifecycle
Issue-Step: setup#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lightsail-catalog-and-operations-lifecycle
Issue-Shape-Id: com.amazonaws.lightsail#Lightsail_20161128
Issue-Key: lightsail|CreateKeyPair|2xx->2xx|$.publicKeyBase64
Issue-Commit: dc1b37cb70c2cd36b8cfbe0cb58cd4064d1ca6fc

# lightsail CreateKeyPair returns publicKeyBase64 without the ssh-rsa OpenSSH label

> _Note: this issue was identified against the code base at commit `dc1b37cb70c2cd36b8cfbe0cb58cd4064d1ca6fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateKeyPair

## Description
Cause 1 of 1: CreateKeyPair reports the minted public half in the wrong rendering, so the response's `publicKeyBase64` is not the value AWS returns for that member.

AWS: Smithy: CreateKeyPair output member `publicKeyBase64` targets `com.amazonaws.lightsail#Base64`, a plain string with no `@pattern` and no `@length` (smithy_query shape=Base64), documented as "A base64-encoded public key of the ssh-rsa type" (smithy_query doc shape=CreateKeyPairResult member=publicKeyBase64). The model therefore states the key TYPE but constrains the rendering not at all, so for the rendering this is recorded behavior only: the recorded 200 response carries `publicKeyBase64` in the OpenSSH public-key rendering for an `ssh-rsa` key — the algorithm label, a separating space, then the base64 of the RFC 4253 `ssh-rsa` key blob. Because the key is minted per run, the fixture does not assert the literal text: the recorded literal is masked (`<shaped-not-asserted>`) and only the value's SHAPE is asserted, and it is that shape the emulator's value fails. The fixed part of the value is the algorithm label and the separator; the variable part is the base64 key blob, which comes from the key pair this call minted. Hypothesis: the asserted shape is exactly that label-plus-body form, since a bare base64 token is what the emulator produced and it is what the comparison rejected.

Emulator: answers HTTP 200 with `$.publicKeyBase64` set to the base64 body ALONE — e.g. a value starting `AAAAB3NzaC1yc2EAAAADAQAB...` with no algorithm label and no separating space. This is deliberate in the code: `state/key_pairs.rs` renders the public half via `ssh_rsa_public_key()` and strips the OpenSSH line's label with `openssh_key_body(..)`, documenting the choice as "one base64 token, with no algorithm label, no separating space". The replay reports `$.publicKeyBase64: value_mismatch (expected '<shaped-not-asserted>', got 'AAAAB3NzaC1yc2E...')` on an otherwise matching 200.

Why: a client relying on AWS's behavior treats this member as the key material it can write straight into a `.pub` file or an `authorized_keys` entry, or hand to another API that takes an OpenSSH public key (EC2 ImportKeyPair, Lightsail ImportKeyPair). Against the emulator that value has no algorithm label, so every such consumer rejects it as not an SSH public key: an SSH key read of the file fails, and automation that compares the created pair's public half against a key it already holds in OpenSSH form sees two different strings for the same key. The failure is silent at the call site, because the call itself still succeeds with 200.

Fix: the rendering of the public half that this operation's state layer hands to the serializer — the function that builds `MintedKeyPairMaterial::public_key`, which `CreateKeyPair`'s handler moves into the output member. The same member is reported by `DownloadDefaultKeyPair` out of `HeldDefaultKeyPairMaterial::public_key` and is documented to be identical, so both reporting paths must change together; the import path already accepts both renderings, so a pair minted here still imports after the change.

Note: the open issue `20261022T050650Z-lightsail-createkeypair-stub.md` ("Implement lightsail CreateKeyPair: unimplemented scaffold stub") does NOT cover this: CreateKeyPair is now implemented and answers 200, the replay reports no stub for it, and that issue is about the 501 refusal, not the rendering of this member.

Traces: lightsail-catalog-and-operations-lifecycle#setup#1 (CreateKeyPair, expected 200 / actual 200, `$.publicKeyBase64` shape mismatch).

## Evidence
Replay of fixture `lightsail-catalog-and-operations-lifecycle`, step `setup#1`, operation CreateKeyPair, verdict mismatch (divergence_key `service=lightsail|CreateKeyPair|status=200->200|keys=$.publicKeyBase64`):

Expected (recorded, shape-asserted because the key is per-run): `$.publicKeyBase64` is an OpenSSH `ssh-rsa` public key — algorithm label + space + base64 key blob. Literal masked as `<shaped-not-asserted>`.

Actual (emulator): `$.publicKeyBase64` = `AAAAB3NzaC1yc2EAAAADAQABAAABAQCPBUPRDXgiEi+MhO75zbEr+dneElTVKy/cpuzQMB7uSScOUcfsPVephp0DWAyICsH0taKIRd8DAxBKG6lX7duLwUrLYDmKs2eYMyhhvillwlX/LKWk3MeHEgoNaRy3zdRzz0uub33iazRLw4LN0meiZASZiPbQAGcTI41XzmcFvtfiDwXsZtAL3LdO4hXXrff46RNsHsV+9y7rdkho8wmbJK8cNJsefWKZf/KObAR19RcuebiMIXHANQOYQTRjbP+8lxBdiCnExz9oHx2wiurBHlNzQ8el7XhT0bg4ebqOxoP4z5cb55OCApPTEgA0ZaA7ftXgBTRvv6opAjY9umSP` — the base64 blob only, with no `ssh-rsa` label and no separator. (The blob itself is per-run key material and is not the requirement; its missing label is.)

HTTP status matched (200) and no error code was involved, so the divergence is confined to the rendering of this one output member.

Supporting code evidence: services/service-com-amazonaws-lightsail-lightsail-20161128/src/state/key_pairs.rs lines 71-87 and 343-350 (`MintedKeyPairMaterial::public_key` / `ssh_rsa_public_key` strip the OpenSSH label deliberately); services/service-com-amazonaws-lightsail-lightsail-20161128/src/ops/create_key_pair.rs line 284 (`public_key_base64: Some(minted.public_key)`).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture `lightsail-catalog-and-operations-lifecycle`, step `setup#1`.
2. Equivalently, call lightsail CreateKeyPair with a fresh `keyPairName` matching `^\w[\w\-]*\w$` (POST /ls/api/2016-11-28/CreateKeyPair).
3. Read `publicKeyBase64` from the 200 response: it is a bare base64 token, with no `ssh-rsa ` prefix.
4. Write that value to a file and read it with any OpenSSH tool (e.g. `ssh-keygen -l -f key.pub`), or submit it unchanged as an OpenSSH public key: it is rejected, whereas the recorded AWS value is accepted.
