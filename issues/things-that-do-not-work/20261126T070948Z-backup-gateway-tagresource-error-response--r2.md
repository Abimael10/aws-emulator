Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 7fc076f9963bf06650d0eb4b8f4742b4e380f3c7

# Tagging handlers and MissingResource's own doc claim every missing-resource refusal is 400 with an ErrorCode, while the Other arm they reach answers 404 with none

_Filed: 2026-11-26T07:09:48Z_
_Operation: TagResource | Class: error-response_

> _Note: this issue was identified against the code base at commit `7fc076f9963bf06650d0eb4b8f4742b4e380f3c7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The three tagging handlers (`TagResource`, `UntagResource`, `ListTagsForResource`) are the only paths in this crate that resolve a missing resource through `crate::state::MissingResource::for_arn`, which is the only route to the `MissingResource::Other` arm. All three state, in their own module/handler docs, that such a refusal is "the modelled `ResourceNotFoundException` shape at HTTP 400, carrying the `ErrorCode` that names which of this service's resource types was not found", and `MissingResource`'s own type-level doc generalises the same rule to EVERY refusal of that value ("answered at HTTP 400 ... whatever status the error shape's `@httpError` declares — and carrying the `ErrorCode` that names WHICH kind of resource was not found").

The code under those sentences does the opposite for exactly the case only these three operations can reach. `MissingResource::error_code()` returns `None` for `Self::Other`, and `MissingResource::status_override()` returns `None` for `Self::Other`, so the handler falls into the `None => error.into()` arm and the response is the shape's declared HTTP 404 with no `ErrorCode` member at all. That ARN form reaches the handler: the modelled `ResourceArn` pattern `^arn:(aws|aws-cn|aws-us-gov):backup-gateway(:[a-zA-Z-0-9]+){3}\/[a-zA-Z-0-9]+$` admits any `[a-zA-Z-0-9]+` resource-type segment, so `arn:aws:backup-gateway:<region>:<account>:appliance/ap-1` passes the boundary validator and resolves to `Other`.

The 404/no-code answer is the behaviour this crate deliberately pins (the `Other` arm's own doc and `tests/all/missing_resource_refusal_is_one_answer.rs` both state it), so the defect is the three overbroad contract sentences plus the type-level sentence above `status_override`/`error_code`, not the wire answer. A reader who trusts the first thing they read in `tag_resource.rs`/`untag_resource.rs` — or the `MissingResource` type doc — would conclude `status_override()` and `error_code()` are missing the `Other` arm and "fix" them, which would break the pinned test and change the wire answer of all three tagging operations. The sentences should carve out the one arm the code and the test already carve out.

## Evidence

The doc, `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/tag_resource.rs:8-11`:
```
//! An ARN with no record behind it is refused as the resource TYPE it names: the modelled
//! `ResourceNotFoundException` shape at HTTP 400, carrying the `ErrorCode` that names
//! which of this service's resource types was not found
//! ([`crate::state::MissingResource`]) — but only once the tag pairs the request carries
```

The identical claim, `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/untag_resource.rs:12-15`:
```
//! An ARN with no record behind it is refused as the resource TYPE it names: the modelled
//! `ResourceNotFoundException` shape at HTTP 400, carrying the `ErrorCode` that names
//! which of this service's resource types was not found
//! ([`crate::state::MissingResource`]).
```

And again in the handler comment at `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/list_tags_for_resource.rs:165-168`:
```
        // machine against the clock exactly as `GetVirtualMachine` does — and an ARN with
        // no record behind it takes the refusal that resource type carries: the same
        // modelled `ResourceNotFoundException` shape throughout, answered 400 with the
        // `ErrorCode` naming the resource type that was not found
```

The same rule generalised over the whole value, `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:3302-3305`:
```
/// Every such refusal is the modelled `ResourceNotFoundException`, answered at HTTP 400 —
/// the status AWS is recorded giving every client refusal of this service, whatever status
/// the error shape's `@httpError` declares — and carrying the `ErrorCode` that names WHICH
/// kind of resource was not found, paired with the fixed sentence AWS states for that kind.
```

The code that breaks both halves of it, `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:3364-3372`:
```
    pub fn error_code(self) -> Option<&'static str> {
        match self {
            Self::Hypervisor => Some(HYPERVISOR_NOT_FOUND),
            Self::PropertyMappings => Some(MAPPINGS_NOT_FOUND),
            Self::Gateway => Some(GATEWAY_NOT_FOUND),
            Self::VirtualMachine => Some(VIRTUAL_MACHINE_NOT_FOUND),
            Self::Other => None,
        }
    }
```
and `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:3393-3401`:
```
    pub fn status_override(self) -> Option<u16> {
        match self {
            Self::Hypervisor
            | Self::PropertyMappings
            | Self::Gateway
            | Self::VirtualMachine => Some(400),
            Self::Other => None,
        }
    }
```

The same file's arm-level doc says the opposite of its own type-level doc, `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/state.rs:3345-3346`:
```
    /// is the one arm that keeps the status its error shape declares and words the
    /// condition around the ARN, because there is no kind of resource to name instead.
```

The branch the three handlers take for it, e.g. `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/tag_resource.rs:205-215`:
```
            let missing = crate::state::MissingResource::for_arn(&input.resource_arn);
            let error = TagResourceErrorModeled::ResourceNotFoundException(
                ResourceNotFoundException {
                    error_code: missing.error_code().map(str::to_string),
                    message: Some(missing.message(&input.resource_arn)),
                },
            );
            return Err(match missing.status_override() {
                Some(status) => crate::scaffolded::Scaffolded::modeled_with_status(error, status),
                None => error.into(),
            });
```

Test coverage that pins the real (404, no `ErrorCode`) answer for the tagging operations — `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/tests/all/missing_resource_refusal_is_one_answer.rs:291-292`:
```
/// all did not flatten refusals that differ on the wire. A type this service does not name at
/// all has no `ErrorCode` of its own and keeps the status the modelled shape declares.
```
and the row that drives it, same file `:336-345`:
```
        (
            "a resource type this service does not name",
            arn_of("appliance", "ap-1111111111111111"),
            Refusal {
                status: 404,
                shape: Some("ResourceNotFoundException".to_string()),
                error_code: None,
```
(contrast `fn not_found` at `:70-77`, which is `status: 400` plus a `Some(error_code)`, used by every other row).

## Steps to Reproduce

1. Read `services/service-com-amazonaws-backupgateway-backuponpremises-v20210101/src/ops/tag_resource.rs:8-11` (and the identical sentence at `src/ops/untag_resource.rs:12-15`, and the handler comment at `src/ops/list_tags_for_resource.rs:165-168`): the documented contract for an ARN with no record behind it is HTTP 400 carrying an `ErrorCode`.
2. Read `src/state.rs:3302-3305`: `MissingResource`'s type-level doc states the same rule for EVERY refusal built from that value.
3. Follow the handler branch at `src/ops/tag_resource.rs:205-215` into `src/state.rs:3364-3372` and `:3393-3401`: for `MissingResource::Other` both `error_code()` and `status_override()` are `None`, so the answer is the shape's declared 404 with no `ErrorCode` member.
4. Confirm `Other` is reachable over the wire: the modelled `ResourceArn` pattern `^arn:(aws|aws-cn|aws-us-gov):backup-gateway(:[a-zA-Z-0-9]+){3}\/[a-zA-Z-0-9]+$` admits any resource-type segment, so call `TagResource` (or `UntagResource`/`ListTagsForResource`) with `arn:aws:backup-gateway:us-east-1:<account>:appliance/ap-1111111111111111` and observe HTTP 404 with no `ErrorCode`, as `tests/all/missing_resource_refusal_is_one_answer.rs:336-345` asserts.
5. Note that `src/state.rs:3345-3346` (the `Other` arm's own doc) already states the carve-out that the type-level doc and the three handler docs omit, so the three contract sentences are the stale half.
