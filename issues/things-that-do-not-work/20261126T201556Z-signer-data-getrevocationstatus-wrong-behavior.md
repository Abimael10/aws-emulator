Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: 63a5892813ef10d964868e55c8a5b01f4a53fc31

# GetRevocationStatus reads a private registry nothing writes, so the data-plane endpoint never reports a revocation the Signer control plane recorded

_Filed: 2026-11-26T20:15:56Z_
_Operation: GetRevocationStatus | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `63a5892813ef10d964868e55c8a5b01f4a53fc31`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler's own module doc states that the registry it reads is written by the Signer control plane's RevokeSignature / RevokeSigningProfile, and that this read reports what they wrote. The code reads `crate::state::service_state` — i.e. this crate's `ComAmazonawsSignerdataSignerdataplaneState`, a store that, by this crate's OWN state module doc, is only ever written through the `revoke` host seam ("the seam a host embedding this emulator (a test, say) uses") and "starts empty". Nothing outside this crate's tests writes it: the Signer control plane records revocations on its own profiles/jobs (`SigningProfile::revocation_record`) and computes its `/revocations` answer from those, and it never touches the data-plane state type.

Consequence in the assembled emulator (both services are mounted on one engine by `core/router/src/lib.rs:921`, and a request whose credential scope names `data-signer` is served by this crate — proven by the router's own test): after a successful RevokeSigningProfile / RevokeSignature, the same GetRevocationStatus request answers the revoked entity at the `signer` endpoint and `{"revokedEntities": []}` at the `data-signer` endpoint. The data-plane read therefore answers 200 "nothing is revoked" for every entity, always — a fail-open answer on a revocation check — and the only way to make its registry non-empty is a test calling the `revoke` seam directly. The two statements in this crate (handler doc: the control plane writes this registry; state doc: only a host/test seam writes it) cannot both be true, and the handler's doc is the one the code breaks.

## Evidence

Handler doc states the rule — services/service-com-amazonaws-signerdata-signerdataplane/src/ops/get_revocation_status.rs:3-7:
  "//! Reports which of the entities a signature depends on have been revoked, by looking
   //! each one up in this service's revocation registry (`crate::state`). The data plane
   //! holds no revocation of its own: the Signer CONTROL plane's `RevokeSignature` /
   //! `RevokeSigningProfile` write that registry, and this read reports it."

The same crate's state module says otherwise — services/service-com-amazonaws-signerdata-signerdataplane/src/state.rs:11-18:
  "There is no modeled write operation on the DATA plane — revocations are created by the Signer CONTROL plane (`RevokeSignature` / `RevokeSigningProfile`), which is a different service. A record therefore enters this store through [`ComAmazonawsSignerdataSignerdataplaneState::revoke`] — the seam a host embedding this emulator (a test, say) uses to record a revocation whose effect the read then reports. Nothing is seeded at construction: the store starts empty"

What the handler actually reads — src/ops/get_revocation_status.rs:252-258:
  "let revoked_entities = state.lock().revoked_entities(" ... — `state` is `crate::state::service_state(&*self.engine)` (line 205), i.e. `ComAmazonawsSignerdataSignerdataplaneState` (src/state.rs:138-145), whose only writers are `revoke`/`unrevoke` (src/state.rs:172-190).

Nobody but this crate's tests writes that store: `grep -r signerdata_signerdataplane --include=*.rs` returns only `core/router/src/lib.rs:921` (`router.register_service(service_com_amazonaws_signerdata_signerdataplane::EmulatedService::new(engine.clone()));`), `core/router/tests/all/signer_identity_authorization.rs:110`, and this crate's own files/tests (tests/all/revocation_status.rs:107-112 is the `revoke` seam call).

Where the real records live — services/service-com-amazonaws-signer-wallabyservice/src/state.rs:333-335: "/// What `RevokeSigningProfile` recorded, for a profile that has been revoked;" `pub revocation_record: Option<SigningProfileRevocationRecord>`; and that crate's own handler computes the SAME route's answer from them — services/service-com-amazonaws-signer-wallabyservice/src/ops/get_revocation_status.rs:343-348:
  "if store.profile(&region, profile_name).is_some_and(|profile| { profile.version_arn(profile_version) == input.profile_version_arn && profile.version_revoked_at(profile_version, &input.signature_timestamp) }) { revoked_entities.push(input.profile_version_arn.clone()); }"
and states at services/service-com-amazonaws-signer-wallabyservice/src/state.rs:2042-2044: "This service serves that route (it holds the revocation records that answer it)".

The data-plane endpoint is reachable and answers 200 in the assembled router — core/router/tests/all/signer_identity_authorization.rs:813-822: "// Signer's data plane publishes its own endpoint behind the same `signer` action namespace, and the router resolves a request's service from its credential scope — so a request whose scope names that endpoint is served by it, carrying the same session." followed by `assert_eq!(status, 200, ...)` — that test only checks the status/authorization, so no test pins the empty list as intended.

Test coverage: this crate's tests (tests/all/revocation_status.rs) only ever populate the registry through `Emulator::revoke` (lines 107-112), so none of them exercises a revocation created by the control plane.

## Steps to Reproduce

1. Boot the assembled emulator (core/router/src/lib.rs registers both service-com-amazonaws-signer-wallabyservice and service-com-amazonaws-signerdata-signerdataplane on one engine).
2. PutSigningProfile at the `signer` endpoint and keep the returned profileVersionArn.
3. RevokeSigningProfile (or RevokeSignature on a job) at the `signer` endpoint so the control plane records a revocation.
4. Send GET /revocations?signatureTimestamp=<an instant after the effective time>&platformId=Notation-OCI-SHA384-ECDSA&profileVersionArn=<the ARN>&jobArn=<a job ARN>&certificateHashes=<a 128-char hex entry> signed with a credential scope naming `signer`: the control plane reports the revoked entity in revokedEntities.
5. Send the identical request with the credential scope naming `data-signer` (as core/router/tests/all/signer_identity_authorization.rs:816-817 does): the data plane answers HTTP 200 with {"revokedEntities": []}.
6. Observe that no code path outside this crate's tests ever calls ComAmazonawsSignerdataSignerdataplaneState::revoke, so the data-plane answer is an empty list for every input.
