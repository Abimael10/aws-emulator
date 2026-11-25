Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 885072a57e3888fb0a37fd2986e8afb48437e63f

# A revision whose build failed reports no errors member at all — Revision.errors is initialised empty and never written by any path

_Filed: 2026-11-25T18:37:16Z_
_Operation: CreateWebFunctionRevision | Class: error-response_

> _Note: this issue was identified against the code base at commit `885072a57e3888fb0a37fd2986e8afb48437e63f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateWebFunctionRevisionResponse$errors` and `GetWebFunctionRevisionResponse$errors` carry "A list of errors encountered during revision creation. This field is absent when the revision has no errors.", and the `RevisionError` shape itself is documented as "A single error encountered while creating or reading a web function revision. The error describes the affected attribute, an error code, and a human-readable message. This structure is present only when the revision has errors."

In this crate the stored `Revision.errors` vector is initialised to `Vec::new()` in `add_revision` and is never assigned anywhere else — a repository-wide grep for `RevisionError` finds only the type definition, the field declaration, the `modeled_errors()` projection and that one `Vec::new()`. `modeled_errors()` therefore returns `None` for every revision that has ever existed, so the `errors` member is unconditionally absent from both revision readers.

That is wrong for exactly the revisions the member exists for. When the build cannot read the deployment package, the crate establishes a concrete error — `code_package` produces either `NOT_A_ZIP_ARCHIVE_REASON` ("Failed to read zip file: zip: not a valid zip file") or `refusal_reason(code, message)` ("Error occurred while GetObject. S3 Error Code: {code}. S3 Error Message: {message}") — and `Revision::materialise_due` settles the revision `Failed` (`RevisionState`: "Failed – The revision build failed.") carrying that text in `stateReason` only. The error encountered during revision creation is known to the handler, has an attribute (`buildConfig.codeConfig.s3Object`), a code and a message — the three `@required` members of `RevisionError` — and is dropped on the floor, so a client reading a failed revision is told the field is absent, i.e. that the revision has no errors.

## Evidence

Smithy (`smithy_query(query="doc", shape="GetWebFunctionRevisionResponse", member="errors")`): "<p>A list of errors encountered during revision creation. This field is absent when the revision has no errors.</p>"

Smithy (`smithy_query(query="shape", shape="RevisionError")`): "<p>A single error encountered while creating or reading a web function revision. The error describes the affected attribute, an error code, and a human-readable message. This structure is present only when the revision has errors.</p>" — members `attribute`, `errorCode`, `errorMessage`, all `required`.

Smithy (`smithy_query(query="doc", shape="RevisionState")`): "<code>Failed</code> – The revision build failed."

The field is written once, empty, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state.rs:586-592:
```
            state: RevisionState::Pending,
            state_reason: REVISION_PENDING_REASON.to_string(),
            errors: Vec::new(),
            created_at: now,
            activates_at: Some(now + PROVISIONING_DELAY),
            build_failure: new.build_failure,
```
and the projection can therefore only answer `None`, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state.rs:274-282:
```
    /// The stored build errors as the modelled `RevisionErrors` list, or `None`
    /// when there are none: the model constrains the list to 1..50 entries, so a
    /// revision with no errors reports no member rather than an empty list.
    pub fn modeled_errors(&self) -> Option<Vec<RevisionError>> {
        if self.errors.is_empty() {
            return None;
        }
        Some(self.errors.clone())
    }
```
wired into both readers at services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function_revision.rs:354 (`errors: revision.modeled_errors(),`) and services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/get_web_function_revision.rs:222 (same line).

The failure the crate DOES establish, and puts only in `stateReason`, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state.rs:297-312:
```
    fn materialise_due(&mut self, now: SystemTime) {
...
        match &self.build_failure {
            Some(reason) => {
                self.state = RevisionState::Failed;
                self.state_reason = reason.clone();
            }
```
with the reasons worded at services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/code_package.rs:52 (`pub const NOT_A_ZIP_ARCHIVE_REASON: &str = "Failed to read zip file: zip: not a valid zip file";`) and code_package.rs:153-155 (`format!("Error occurred while GetObject. S3 Error Code: {code}. S3 Error Message: {message}")`).

Test coverage: `grep errors tests/all/revision_build_outcome.rs` returns no matches — the suite that drives failed builds (`tests/all/revision_build_outcome.rs:240-363`, which asserts `state == "Failed"` and the exact `stateReason`) never looks at the `errors` member, so nothing pins its absence and no comment in the crate states a reason for it.

## Steps to Reproduce

1. Start the emulator with an object store and PutObject a non-zip body at s3://demo-bucket/broken.zip
2. PUT /2025-03-07/web-functions with {"functionName":"fn","revisionConfig":{"buildConfig":{"codeConfig":{"s3Object":{"bucket":"demo-bucket","key":"broken.zip"}},"runtimeConfig":{"runtime":"nodejs24.x"}},"serviceConfig":{"executionRoleArn":"arn:aws:iam::000000000000:role/r"}}}
3. POST /2025-03-07/web-functions/fn/revisions with the same buildConfig/serviceConfig and observe the 202 body: no "errors" member
4. Poll GET /2025-03-07/web-functions/fn/revisions/{revisionId} until state is "Failed" with stateReason "Failed to read zip file: zip: not a valid zip file"
5. Observe the response still carries no "errors" member, i.e. it reports that this revision — whose build failed — encountered no errors during revision creation
