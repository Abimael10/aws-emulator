Issue-Severity: HIGH
Issue-Gap-Class: event-propagation
Issue-Commit: 0fd0c8a38859f6782ef39e69b19293525b727c9f

# Wire the just-in-time node access gate to the enrollment state the emulator already holds — today no request can ever enroll an account, so every Update is 403 forever

_Filed: 2026-11-20T17:42:51Z_
_Operation: UpdateConnectionRecordingPreferences | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `0fd0c8a38859f6782ef39e69b19293525b727c9f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateConnectionRecordingPreferences` is gated on `store.just_in_time_node_access_enabled()`, a private `bool` on this crate's own state whose ONLY writer is `set_just_in_time_node_access_enabled`, a `pub fn` that no handler, no operation and no other crate in the tree ever calls. The default is `false`. Consequence: for ANY caller reaching the emulator over HTTP, `UpdateConnectionRecordingPreferences` answers `AccessDeniedException` (403) unconditionally, no matter what the request holds and no matter what else the caller does first — the singleton can never be written, so `GetConnectionRecordingPreferences` can only ever answer `ResourceNotFoundException` (404) and `DeleteConnectionRecordingPreferences` can only ever take its "nothing configured" branch. The whole create -> read -> delete lifecycle of this service's one resource is reachable only from in-process Rust test code that mutates the flag directly (which is exactly what all three test suites do).

The crate's own comment admits this is an unfinished path: it says enrollment is "a different API, which this service does not model and this emulator does not implement" and that the flag is set by "an enrollment path this emulator grows later, or a test standing the enrolled account up". That first claim is not true of this repository: the SSM service crate DOES implement just-in-time node access enrollment, persists it, and exposes it as a per-Region predicate (`has_access_request_approval_policy`) that is already consumed by two SSM handlers (`start_access_request.rs:177`, `get_access_token.rs:179`). An account becomes enrolled over the wire by creating an SSM document of type `AutoApprovalPolicy`/`ManualApprovalPolicy`.

Reading another service crate's store from this crate is also already this crate's established pattern: `src/state/authorization.rs` imports and reads the IAM and STS service crates' state modules to take its authorization decision. So the gate has a real, persisted, wire-reachable source of truth available to it and reads an orphaned local flag instead.

This is not a request to remove or weaken the gate (the gate itself is required): it is that the gate's input is never fed, so the denial is unconditional rather than conditional on account state.

## Evidence

The gate, and the flag it reads — services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/ops/update_connection_recording_preferences.rs:206-215:
    `if !store.just_in_time_node_access_enabled() {`
    `    drop(store);`
    `    audit(Self::NAME, ctx, AuditOutcome::AccessDenied);`
    `    return Err(UpdateConnectionRecordingPreferencesErrorModeled::AccessDeniedException(`
    `        AccessDeniedException {`
    `            message: crate::state::JUST_IN_TIME_NODE_ACCESS_DISABLED.to_string(),`

The flag's only writer, and the comment admitting no caller exists — services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/state.rs:157-164:
    `/// Enroll (or un-enroll) this account/Region in just-in-time node access.`
    `///`
    `/// The ONE writer of the gate's flag. No modelled operation of this service calls`
    `/// it — enrollment belongs to SSM Quick Setup — so it exists for whoever stands an`
    `/// enrolled account up.`
    `pub fn set_just_in_time_node_access_enabled(&mut self, enabled: bool) {`

The comment admitting the gap, and the claim the repo contradicts — services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/state.rs:73-81:
    `/// \`false\` is the state of an account nobody has enrolled, which is what the`
    `/// default is: enrollment is performed by SSM Quick Setup's just-in-time node`
    `/// access configuration — a different API, which this service does not model and`
    `/// this emulator does not implement — so no operation of THIS service turns it on.`
    `/// ... whoever enrolls the account (an enrollment path this emulator grows later,`
    `/// or a test standing the enrolled account up) sets it`
    `/// through [\`...::set_just_in_time_node_access_enabled\`]`
    `/// and every later Update reads it back.`

The other side of the repo inconsistency — the emulator DOES implement just-in-time node access enrollment, wire-reachable and persisted. services/service-com-amazonaws-ssm-amazonssm/src/state/just_in_time_node_access.rs:15-25:
    `//! The workflow is OFF for an account until that account holds an access-request approval`
    `//! policy — the document that decides who may approve a request, held as an SSM document of`
    `//! type \`AutoApprovalPolicy\` or \`ManualApprovalPolicy\``
    `//! ... one that creates a policy has it in the`
    `//! Region the policy was created in. A policy is a REGIONAL document like every other, so`
    `//! the decision is taken per Region ([\`...::has_access_request_approval_policy\`]).`
and services/service-com-amazonaws-ssm-amazonssm/src/state/just_in_time_node_access.rs:277-281:
    `/// Whether \`region\` has just-in-time node access at all: the account holds an`
    `/// access-request approval policy there.`
    `pub fn has_access_request_approval_policy(&self, region: &str) -> bool {`
already consumed by SSM handlers: services/service-com-amazonaws-ssm-amazonssm/src/ops/start_access_request.rs:177 `if !store.has_access_request_approval_policy(&region) {` and services/service-com-amazonaws-ssm-amazonssm/src/ops/get_access_token.rs:179 `if !store.has_access_request_approval_policy(&region) {`.

This crate already reads other service crates' stores, so the cross-crate read is in-pattern — services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/state/authorization.rs:92-93:
    `use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;`
    `use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;`

Test coverage proving no request can enroll an account (all three suites bypass the wire to set the flag) — services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/tests/all/connection_recording_preferences.rs:75-85:
    `/// Enroll (or un-enroll) the account in just-in-time node access.`
    `///`
    `/// Set on the account's own state rather than through an API because enrollment`
    `/// belongs to SSM Quick Setup: no operation of THIS service performs it, so there is`
    `/// no request a test could send to stand an enrolled account up.`
    `fn enroll(engine: &engine::InMemoryEngine, enrolled: bool) {`
    `    engine.state::<ComAmazonawsSsmguiconnectSsmguiconnectState>()`
    `        .lock().set_just_in_time_node_access_enabled(enrolled);`
same helper duplicated at tests/all/jit_node_access_gate_and_error_messages.rs:52-61 and tests/all/identity_authorization.rs:123-131. Every "configured resource" case in connection_recording_preferences.rs runs through `enrolled_router()` (:64), i.e. none of them is reachable by an HTTP client.

## Steps to Reproduce

1. Boot the emulator and point any SSM GuiConnect client at it, signing as a principal with `ssm-guiconnect:*` allowed (so the IAM gate in src/state/authorization.rs passes).
2. Call UpdateConnectionRecordingPreferences with a well-formed body: ConnectionRecordingPreferences={'RecordingDestinations':{'S3Buckets':[{'BucketName':'my-rdp-recordings','BucketOwner':'123456789012'}]},'KMSKeyArn':'arn:aws:kms:us-east-1:123456789012:key/abc'}. Observe 403 AccessDeniedException with "Just-in-time node access is not enabled. ..." (src/ops/update_connection_recording_preferences.rs:206-215).
3. Try to enroll the account through ANY request the emulator serves, including SSM `CreateDocument` with DocumentType='AutoApprovalPolicy' (which flips the SSM crate's own `has_access_request_approval_policy`, services/service-com-amazonaws-ssm-amazonssm/src/state/just_in_time_node_access.rs:279). Re-issue the Update: still 403 — the ssm-guiconnect gate reads its own private bool, which nothing wrote.
4. grep the whole tree for `set_just_in_time_node_access_enabled`: the only callers are the three `fn enroll(...)` test helpers (tests/all/connection_recording_preferences.rs:80, tests/all/jit_node_access_gate_and_error_messages.rs:56, tests/all/identity_authorization.rs:123). No handler and no other crate calls it.
5. Call GetConnectionRecordingPreferences: 404 ResourceNotFoundException, and it can never be anything else over the wire, because the singleton is only ever written by the Update that is permanently refused (src/ops/get_connection_recording_preferences.rs:181-191).
