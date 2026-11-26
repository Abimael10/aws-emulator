Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 89891cf26090dcd00165ccf096d4c55bb51500e1

# ScanSbom input refusals send the synthetic ValidationException without the model's required `reason`

_Filed: 2026-11-20T00:59:38Z_
_Operation: ScanSbom | Class: error-response_

> _Note: this issue was identified against the code base at commit `89891cf26090dcd00165ccf096d4c55bb51500e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every input-SBOM refusal (`SbomDefect::CannotParse`, `InvalidValue(..)`, `TooManyComponents(..)`, plus the defensive `outputFormat` arm) is answered through `refused()` -> `ScanSbomError::validation(..)`, i.e. the SYNTHETIC `smithy.framework#ValidationException` of `src/scaffolded.rs`, which serializes `message` and nothing else.

But this operation MODELS its own `com.amazonaws.inspectorscan#ValidationException`, whose `reason` member is `required`, and the crate already generates that shape (`ValidationException { message, reason, fields }`) and already declares the error-enum variant for it (`ScanSbomErrorModeled::ValidationException(ValidationException)`) — a variant the handler never constructs. The result is a 400 body that is missing a member the model declares mandatory, so a client (or SDK) that reads `reason` to classify the failure finds nothing, even though the handler internally knows the exact class of defect (the model's `ValidationExceptionReason` enum carries `CANNOT_PARSE`, `FIELD_VALIDATION_FAILED`, `UNSUPPORTED_SBOM_TYPE`, `OTHER`).

This is inconsistent inside the same handler: the sibling failure path `internal()` DOES construct the modeled `InternalServerException` and populates its equally-required `reason` from the model's own enum. Only the validation path bypasses the modeled shape. The crate's own error-channel documentation instructs exactly the opposite of what the handler does.

## Evidence

Model (smithy_query shape=ValidationException): `"message": {"target": "String", "required": true}, "reason": {"target": "ValidationExceptionReason", "required": true, "doc": "<p>The reason for the validation failure.</p>"}` — and `ValidationExceptionReason` values are `["UNKNOWN_OPERATION", "CANNOT_PARSE", "FIELD_VALIDATION_FAILED", "UNSUPPORTED_SBOM_TYPE", "OTHER"]`.

The generated modeled shape exists in this crate — services/service-com-amazonaws-inspectorscan-inspectorscan/src/ops/types/scan_sbom_types.rs:48-54:
```
#[smithy_macros::smithy(shape = "com.amazonaws.inspectorscan#ValidationException")]
pub struct ValidationException {
    pub message: String,
    pub reason: String,
    pub fields: Option<Vec<ValidationExceptionField>>,
}
```
The handler declares the variant and never builds it — src/ops/scan_sbom.rs:111: `    ValidationException(ValidationException),`

The refusal path instead returns the synthetic shape — src/ops/scan_sbom.rs:396-399:
```
/// A `ValidationException` (HTTP 400) carrying `message` and nothing else.
fn refused(message: impl Into<String>) -> ScanSbomError {
    ScanSbomError::validation(message)
}
```
which is src/scaffolded.rs:136-144 `smithy.framework#ValidationException` with the single member `VALIDATION_EXCEPTION_MESSAGE`.

Sibling path in the SAME handler populates the model's required `reason` — src/ops/scan_sbom.rs:418-425:
```
ScanSbomErrorModeled::InternalServerException(InternalServerException {
    message: message.into(),
    // From the model's `InternalServerExceptionReason` enum
    reason: "OTHER".to_string(),
```
The crate's own guidance contradicts the chosen path — src/scaffolded.rs:215-219: "If this service's model declares its own `ValidationException` (or another client-input error) use THAT: the modeled shape may carry members beyond `message` — a `reason`, a field list — and a real client reads them. When the operation already lists it, it is a plain variant on the generated error enum".

Test coverage: tests/all/scan_sbom_refusals.rs:367-380 currently pins the message-only body ("the refusal body must carry message alone"), i.e. the gap is pinned by a test whose only stated reason is a restatement of the behaviour, not the model.

## Steps to Reproduce

1. POST /scan/sbom with `{"sbom": {}}` (no `bomFormat`).
2. Observe HTTP 400 with body `{"__type":"ValidationException","message":"Input SBOM failed validation: Invalid value for \"bomFormat\""}`.
3. Note the body carries no `reason`, although `smithy_query(query="shape", shape="ValidationException")` marks `reason` `required: true`.
4. Compare with the 500 path (`internal(..)` in src/ops/scan_sbom.rs:418), which does populate the equally-required `reason` of `InternalServerException`.
