Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: d480a9c819aca039333e6ccd75fa51254e2ac722

# The recording-preferences singleton is stored globally, not per requesting Region/account, so a Get signed for another Region reports (and a Delete removes) a record written elsewhere

_Filed: 2026-11-20T21:12:18Z_
_Operation: GetConnectionRecordingPreferences | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `d480a9c819aca039333e6ccd75fa51254e2ac722`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The model scopes this resource to the requesting account and Region, and every doc comment in the crate repeats that wording ("the per-account/per-Region connection recording preferences singleton"). The implementation, however, holds exactly ONE unkeyed record for the whole process: `ComAmazonawsSsmguiconnectSsmguiconnectState.connection_recording_preferences` is a bare `Option<RecordingPrefsRecord>`, `RecordingPrefsRecord` carries no account or Region field, and `preferences()` / `put_preferences()` / `remove_preferences()` take no scope argument. No handler looks at the Region the request was signed for.

Consequences, all observable on the wire:
* `GetConnectionRecordingPreferences` signed for `eu-west-1` answers 200 with the preferences a `us-east-1`-signed `UpdateConnectionRecordingPreferences` wrote, instead of the `ResourceNotFoundException` (404) the handler's own "nothing configured" branch is for — i.e. it reports preferences that were not "specified ... in the requesting ... Region".
* `UpdateConnectionRecordingPreferences` signed for a second Region silently REPLACES the first Region's record (create-or-replace on the single global slot).
* `DeleteConnectionRecordingPreferences` signed for a second Region removes the first Region's record.

This is not a feature the crate never attempts: the same crate already derives the requesting Region from the SigV4 credential scope (`crate::state::authorization::request_region`) and scopes the IAM authorization resource by it, with a unit test pinning that a statement for one Region's resources covers that Region and no other. The record the decision is taken about is the one thing left unscoped, so the authorization layer and the storage layer of this crate disagree about whether a request's Region matters.

## Evidence

Smithy model — the operation doc states the scope of what is returned (smithy_query(query="doc", shape="GetConnectionRecordingPreferences")):
    "Returns the preferences specified for recording RDP connections in the requesting Amazon Web Services account and Amazon Web Services Region."
and GetConnectionRecordingPreferencesResponse$ConnectionRecordingPreferences:
    "The set of preferences used for recording RDP connections in the requesting Amazon Web Services account and Amazon Web Services Region. This includes details such as which S3 bucket recordings are stored in."

The crate's own doc claims the same scoping — services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/state.rs:66-69:
    `/// The one per-account/per-Region connection recording preferences record.`
    `/// \`None\` means never configured (or deleted), which every read reports as`
    `/// \`ResourceNotFoundException\`.`
    `connection_recording_preferences: Option<RecordingPrefsRecord>,`
and src/state.rs:5-9: `//! SSM GUI Connect models exactly ONE resource: the per-account/per-Region` ... `//! *connection recording preferences* singleton.`

But nothing stored or read carries a Region or an account — src/state.rs:97-110 (`RecordingPrefsRecord` members are `kms_key_arn`, `s3_buckets`, `client_token`, `last_updated` only), and the accessors take no scope:
    src/state.rs:147 `pub fn preferences(&self) -> Option<&RecordingPrefsRecord> {`
    src/state.rs:169 `pub fn put_preferences(&mut self, record: RecordingPrefsRecord) {`
    src/state.rs:184 `pub fn remove_preferences(&mut self, client_token: &str) -> bool {`

The read side, with no Region in it — src/ops/get_connection_recording_preferences.rs:179:
    `let stored = state.lock().preferences().cloned();`
The write side — src/ops/update_connection_recording_preferences.rs:254:
    `store.put_preferences(record.clone());`
The delete side — src/ops/delete_connection_recording_preferences.rs:209:
    `let removed = store.remove_preferences(&client_token);`

The other side of the repo inconsistency: THIS crate already reads the requesting Region and scopes a decision by it — src/state/authorization.rs:126-134:
    `/// The region this request was signed for, defaulting to [\`DEFAULT_REGION\`].`
    `pub fn request_region(parts: &::http::request::Parts) -> String {`
    `    ::emulator_server::sigv4_credential_scope(&parts.headers)`
    `        .map_or_else(|| DEFAULT_REGION.to_string(), |scope| scope.region)`
used at src/state/authorization.rs:163:
    `let resource = recorded_instances_resource(&request_region(ctx.parts()));`
and pinned per-Region by this crate's own unit test, src/state/authorization.rs:398-402:
    `/// RULE: the decision is taken against the account's EC2 instance space in the REGION`
    `/// the request was signed for, so a statement scoped to one Region's instances covers`
    `/// that Region and no other.`
    `fn a_statement_is_read_for_the_instance_space_of_the_requests_region() {`

Test coverage: no existing test exercises a second Region — every request helper hardcodes one credential scope, e.g. tests/all/connection_recording_preferences.rs:100 `Credential={akid}/20260101/us-east-1/ssm-guiconnect/aws4_request, ` and tests/all/jit_node_access_gate_and_error_messages.rs:72 `Credential=AKIDEXAMPLE/20260101/us-east-1/ssm-guiconnect/aws4_request, `. So the cross-Region behaviour is unpinned, and nothing in the crate states a reason for sharing one record across Regions.

## Steps to Reproduce

1. Stand the emulator up with this service registered (and enroll the account in just-in-time node access, since UpdateConnectionRecordingPreferences is gated on it — src/ops/update_connection_recording_preferences.rs:206).
2. Call UpdateConnectionRecordingPreferences signed with a SigV4 credential scope for us-east-1, body {"ConnectionRecordingPreferences":{"RecordingDestinations":{"S3Buckets":[{"BucketName":"rdp-recordings","BucketOwner":"123456789012"}]},"KMSKeyArn":"arn:aws:kms:us-east-1:123456789012:key/abc"}} -> 200.
3. Call GetConnectionRecordingPreferences signed for a DIFFERENT Region (credential scope .../eu-west-1/ssm-guiconnect/aws4_request), body {}. Observed: 200 carrying the record written for us-east-1 (src/ops/get_connection_recording_preferences.rs:179 reads the one unkeyed slot). Expected per the operation doc: the preferences "in the requesting ... Region", i.e. the handler's ResourceNotFoundException branch (:181-191).
4. Call DeleteConnectionRecordingPreferences signed for eu-west-1 -> 200, then call GetConnectionRecordingPreferences signed for us-east-1 again: 404 — the other Region's call removed this Region's record (src/state.rs:185 `self.connection_recording_preferences.take()`).
5. grep the crate for the Region the request was signed for: `request_region` exists at src/state/authorization.rs:131 and is used only for the IAM resource ARN (:163); no handler and no state accessor takes an account or Region.
