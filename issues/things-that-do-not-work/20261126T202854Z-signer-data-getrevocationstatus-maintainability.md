Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: b79c1c9976cd91e4a93f1f05fb039eb86d3c85f3

# ENDPOINT_PREFIX's doc states can_handle claims the shared `signer` signing name, which can_handle's own code and comment deliberately refuse to claim

_Filed: 2026-11-26T20:28:54Z_
_Operation: GetRevocationStatus | Class: maintainability_

> _Note: this issue was identified against the code base at commit `b79c1c9976cd91e4a93f1f05fb039eb86d3c85f3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The routing rule of this service — which route keys it claims — is stated twice in `src/service.rs`, and the two statements contradict each other.

`can_handle` matches the resolved route key against ONE value, `ENDPOINT_PREFIX` ("data-signer"), and the comment directly above that code spells out why the shared SigV4 signing name `signer` is deliberately NOT claimed: claiming it would give every `signer`-signed request two claimants and the router would answer `500 AmbiguousService`.

The doc comment on the `ENDPOINT_PREFIX` constant itself, 110 lines further down the same file, states the opposite rule: "`can_handle` claims a request that names either" (i.e. the endpoint prefix OR the signing name). Nothing in the crate implements that; `can_handle` has exactly one arm and it compares to `ENDPOINT_PREFIX` only.

This is the drift the two copies make possible: an editor who follows the constant's doc and adds the signing name to the match arm reintroduces exactly the ambiguous-claim failure the in-function comment exists to prevent, breaking every `signer`-signed request in the assembled router. Correcting the stale statement changes no response and no test — `can_handle`'s code and the claimed key stay exactly as they are.

## Evidence

The rule as the code states it — services/service-com-amazonaws-signerdata-signerdataplane/src/service.rs:152-172:
  "// The signing name itself is SHARED with AWS Signer
   // (`services/service-com-amazonaws-signer-wallabyservice`, endpoint `signer`),
   // and it is NOT claimed here: ... Claiming the shared name here as well would
   // give every `signer`-signed request two claimants and the router would answer
   // `500 AmbiguousService` for all of it."
followed by the only match arm:
  "match smithy_server_runtime::resolve_route_key(parts) {
       Some(key) if key == ENDPOINT_PREFIX => {
           claims_modeled_path(parts, crate::ROUTE_PATH_SEGMENTS)
       }
       _ => false,
   }"

The same rule as the constant's doc states it — services/service-com-amazonaws-signerdata-signerdataplane/src/service.rs:280-283:
  "/// This service's endpoint prefix, as the model gives it (`data-signer`) — distinct
   /// from its SigV4 signing name (`signer`), which the generated identity carries.
   /// `can_handle` claims a request that names either.
   pub const ENDPOINT_PREFIX: &str = \"data-signer\";"

"claims a request that names either" cannot be true of a match with a single arm testing `key == ENDPOINT_PREFIX`, and the in-function comment says claiming the second name would be a defect (500 AmbiguousService).

The crate's own state module repeats the code's version of the rule, not the constant's — services/service-com-amazonaws-signerdata-signerdataplane/src/state.rs:21-25:
  "the control plane ... serves `/revocations` for the shared `signer` signing name — every request an SDK signs, whichever endpoint host it sends it to — and this service answers the requests addressed at its OWN endpoint key, which only a request carrying no credential scope resolves to."

Test coverage of the behaviour that must stay unchanged: every request in services/service-com-amazonaws-signerdata-signerdataplane/tests/all/revocation_status.rs is driven through `MultiServiceRouter` with only this service registered (tests/all/revocation_status.rs:96-102), and the assembled-router case is covered by core/router/tests/all/signer_identity_authorization.rs (cited in the open issue 20261126T201556Z-signer-data-getrevocationstatus-wrong-behavior as asserting a 200 for a scope naming `data-signer`).

## Steps to Reproduce

1. Open services/service-com-amazonaws-signerdata-signerdataplane/src/service.rs and read `can_handle` (lines 144-173): the resolved route key is compared against `ENDPOINT_PREFIX` only, and the comment at lines 152-161 states that claiming the shared `signer` signing name as well would make every signer-signed request ambiguous (500 AmbiguousService).
2. Read the doc comment on `ENDPOINT_PREFIX` at lines 280-283 of the same file: "`can_handle` claims a request that names either."
3. Observe the two statements of one routing rule disagree: no code path in the crate claims the `signer` signing name, and the in-function comment says doing so would be a defect.
