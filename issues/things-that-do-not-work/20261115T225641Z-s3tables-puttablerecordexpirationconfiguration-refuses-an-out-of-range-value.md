Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3tables.record-expiration-invalid-days-rejected
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables.record-expiration-invalid-days-rejected|run#14|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code}"
Issue-Commit: a8b5efd4a7d15f36db8efaf56cc150cd0dc8890c

# s3tables PutTableRecordExpirationConfiguration refuses an out-of-range value.settings.days with BadRequestException instead of ValidationException

> _Note: this issue was identified against the code base at commit `a8b5efd4a7d15f36db8efaf56cc150cd0dc8890c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutTableRecordExpirationConfiguration

## Description
REOPENS 20261102T055316Z-s3tables-constraint-violation-refusals-are-now-blanket-400.md: its fix left the call refused. That issue's cause 1 named this exact path (PutTableRecordExpirationConfiguration with value.settings.days below the modeled minimum answering BadRequestException where AWS answers ValidationException) and is marked fixed, but replaying the check that carries its replay key still shows the framework-level member-constraint refusal re-labelled as the service's own BadRequestException. This is reported as that reopening, not as a newly discovered defect.

Cause 1 of 1: the framework member-constraint refusal on PutTableRecordExpirationConfiguration carries the wrong error code.
AWS: a PutTableRecordExpirationConfiguration whose value.settings.days is below the member's modeled minimum is refused by request validation BEFORE any table-eligibility check, with HTTP 400 and error code `ValidationException`, message "1 validation error detected: Value at '<member path>' failed to satisfy constraint: Member must have value greater than or equal to <min>". Fixed text: "1 validation error detected: Value at '", "' failed to satisfy constraint: Member must have value greater than or equal to "; variable parts: the member path derived from the request's shape path (here value.settings.days) and the constraint's own limit taken from the model (1) — the offending value itself is NOT quoted. Smithy (services/service-com-amazonaws-s3tables-s3tablebuckets/smithy-model/com-amazonaws-s3tables-s3tablebuckets.json): PutTableRecordExpirationConfiguration.value targets TableRecordExpirationConfigurationValue (required), http PUT /table-record-expiration code 204, errors BadRequestException(400)/ForbiddenException(403)/MethodNotAllowedException(405)/NotFoundException(404)/TooManyRequestsException(429)/InternalServerErrorException(500); TableRecordExpirationSettings.days targets PositiveInteger @range(min 1, max 2147483647). No ValidationException shape exists anywhere in com.amazonaws.s3tables, so this code is the framework-level (untyped) request-validation refusal rather than a modeled error: recorded behavior only for the code on this path.
Emulator: HTTP 400 with error code `BadRequestException` — the generic member-constraint validator's refusal is classified as the service's business-rule error.
Why: `ValidationException` is the only code a client can branch on to separate a malformed-parameter refusal from s3tables' own semantic refusals, which also use BadRequestException (invalid open table format, non-empty namespace, invalid SSE algorithm). A client whose `catch ValidationException` arm reports a field-level parameter error, and whose BadRequestException arm reports a service rejection, now takes the service-rejection branch for a plain out-of-range parameter and never surfaces the offending member. The modeled-error set of this operation does not even contain ValidationException, so the SDK maps the correct code to an unknown error — meaning the emulator cannot be relied on to tell the two refusal classes apart at all.
Fix: the s3tables request-validation layer's refusal classification for framework-validated member constraints (the generic constraint validator feeding PutTableRecordExpirationConfiguration's input validation) — keep answering `ValidationException` for member-constraint violations and reserve BadRequestException for the per-operation, service-authored refusals. The accompanying message template is the subject of open issue 20261101T233620Z-s3tables-refusals-diverge-enum-pattern-violations-answer-validationexception.md (its cause 5), which owns the message assertion of this same check and does NOT cover the error code reported here.
Traces: s3tables.record-expiration-invalid-days-rejected.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout. Check `s3tables.record-expiration-invalid-days-rejected` is a `mismatch`: the emulator answered with HTTP 400, so only the error code diverges.

- Request: create a table bucket, a namespace and an ICEBERG table, then with a botocore client built as Config(parameter_validation=False) call PutTableRecordExpirationConfiguration(tableArn=<the customer table's ARN>, value={"settings": {"days": 0}, "status": "enabled"}).
- Failure: `check_s3tables_record_expiration_invalid_days_rejected.py` line 53 — AssertionError at `assert code == "ValidationException", f"expected ValidationException, got {code}"` → "expected ValidationException, got BadRequestException".
- Expected code/message: 400 `ValidationException`, $.message "1 validation error detected: Value at 'value.settings.days' failed to satisfy constraint: Member must have value greater than or equal to 1" (fixed text plus the member path and the model's minimum; no quoted value).
- Observed: 400 `BadRequestException`.
- The check's later assertions (the Get still answering 405 MethodNotAllowedException "Table record expiration configuration API calls to this table are not allowed", i.e. no configuration was created) are not reached because the code assertion fails first.
- Model check (smithy_query on services/service-com-amazonaws-s3tables-s3tablebuckets/smithy-model/com-amazonaws-s3tables-s3tablebuckets.json): TableRecordExpirationSettings.days → PositiveInteger @range(1, 2147483647); PutTableRecordExpirationConfiguration errors = BadRequestException(400), ForbiddenException(403), MethodNotAllowedException(405), NotFoundException(404), TooManyRequestsException(429), InternalServerErrorException(500) — no ValidationException shape in the service model.
- Prior filing: 20261102T055316Z-s3tables-constraint-violation-refusals-are-now-blanket-400.md (status fixed) listed this check in Issue-Trace-Ids with Issue-Key `s3tables.record-expiration-invalid-days-rejected|run#14|AssertionError|assert code == "ValidationException", f"expected ValidationException, got {code}"` — the identical assertion still fails.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay simparity-behavioral-aws-v1 (or run the single check s3tables.record-expiration-invalid-days-rejected).
2. CreateTableBucket, then CreateNamespace in it, then CreateTable(..., format="ICEBERG") in that namespace; confirm GetTable returns the table (positive control).
3. Build an s3tables client with botocore Config(parameter_validation=False) so the out-of-range value reaches the service.
4. Call PutTableRecordExpirationConfiguration(tableArn=<the table ARN from step 2>, value={"settings": {"days": 0}, "status": "enabled"}).
5. Read $.Error.Code on the 400 — expect `ValidationException`, observe `BadRequestException`. Expected $.message: "1 validation error detected: Value at 'value.settings.days' failed to satisfy constraint: Member must have value greater than or equal to 1".
6. Then call GetTableRecordExpirationConfiguration(tableArn=<same ARN>) — it must still answer 405 MethodNotAllowedException "Table record expiration configuration API calls to this table are not allowed", showing the refused Put created nothing.
