Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 9d6dbefbeeb79c5752051e10ea3d5ba278a8ab35

# Handler doc contradicts itself and the code about which error a missing pod identity association gets

_Filed: 2026-11-24T16:24:01Z_
_Operation: AssumeRoleForPodIdentity | Class: error-response_

> _Note: this issue was identified against the code base at commit `9d6dbefbeeb79c5752051e10ea3d5ba278a8ab35`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The `AssumeRoleForPodIdentity` handler's module doc states, in the paragraph about which failure the file's own `AccessDeniedException` covers, that the only `AccessDeniedException` this handler constructs is the missing pod identity association. Fifteen lines earlier, step 5 of the same docblock says that exact failure is answered with `ResourceNotFoundException` (404), which is what the code does: `no_association()` builds a `ResourceNotFoundException`, and the only `AccessDeniedException` the file constructs is `target_role_not_trusted()`, for an association whose TARGET role may not be assumed — a failure the stale sentence does not mention at all.

So the file carries two mutually exclusive statements of its own error contract, and the one in the AccessDenied paragraph is false for the code under it: a maintainer who edits the error mapping from that paragraph would turn a tested 404 into a 400 and would also lose the documented home of the target-role refusal. The defect is the stale sentence; the response the operation serves today (404 `ResourceNotFoundException`, message "The token included in the request has no service account role association for it.") is what step 5, the code and the test all agree on and must not change.

## Evidence

The stale claim — services/service-com-amazonaws-eksauth-eksauthfrontend/src/ops/assume_role_for_pod_identity.rs:56-58:
```
//! modelled `AccessDeniedException` (400) itself. So the `AccessDeniedException` this file
//! constructs is only the other thing that shape says — the cluster holds no pod identity
//! association for the service account the token names (step 5).
```
Step 5 of the SAME docblock — src/ops/assume_role_for_pod_identity.rs:42-44:
```
//! 5. the cluster must hold a pod identity association for the (namespace, service
//!    account) pair the token names — otherwise `ResourceNotFoundException` (404), the
//!    answer a token whose association was deleted, or never created, gets;
```
The code the paragraph sits above — src/ops/assume_role_for_pod_identity.rs:584-589 (`no_association`):
```
fn no_association() -> AssumeRoleForPodIdentityError {
    AssumeRoleForPodIdentityErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
        message: Some(
            "The token included in the request has no service account role association for it."
```
and the one `AccessDeniedException` the file really constructs — src/ops/assume_role_for_pod_identity.rs:594-603:
```
/// The association names a TARGET role its own role may not assume, so there is no
/// credential to vend: the modelled `AccessDeniedException` (400) ...
fn target_role_not_trusted() -> AssumeRoleForPodIdentityError {
    AssumeRoleForPodIdentityErrorModeled::AccessDeniedException(AccessDeniedException {
```
Test coverage pinning the code's side (so the fix is the doc, not the response) — tests/all/assume_role_for_pod_identity.rs:540-544:
```
        assert_eq!(
            error_code(status, &body),
            "404 ResourceNotFoundException",
            "case {case:?}: expected the modeled 404, got {status} {body}"
        );
```

## Steps to Reproduce

1. Read services/service-com-amazonaws-eksauth-eksauthfrontend/src/ops/assume_role_for_pod_identity.rs:56-58: the doc says the `AccessDeniedException` this file constructs is the missing-association case.
2. Read lines 42-44 of the same docblock: step 5 says the missing-association case is `ResourceNotFoundException` (404).
3. Read `no_association()` at line 584: it constructs `ResourceNotFoundException`; read `target_role_not_trusted()` at line 602: it is the file's only `AccessDeniedException`, for an untrusted target role.
4. Run the suite: tests/all/assume_role_for_pod_identity.rs::a_service_account_without_an_association_is_denied asserts "404 ResourceNotFoundException" for a token whose (namespace, service account) pair has no association, confirming the doc sentence — not the response — is wrong.
