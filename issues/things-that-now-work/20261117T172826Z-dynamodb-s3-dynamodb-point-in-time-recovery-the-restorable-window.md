Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__s3.export-incremental-window-below-minimum-rejected
Issue-Key: dynamodb__s3.export-incremental-window-below-minimum-rejected|_wait_for_restorable_spread#9|Timeout|time.sleep(interval_s)
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# DynamoDB point-in-time recovery: the restorable window never widens, so the incremental-export window quota is unreachable

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExportTableToPointInTime

## Description
Cause 1 of 1: after PITR is enabled, the table's restorable range never spans the minimum incremental-export window, so the export's window validation is never reached.
AWS: once ContinuousBackups reaches PointInTimeRecoveryStatus 'ENABLED', DescribeContinuousBackups reports EarliestRestorableDateTime and LatestRestorableDateTime, and that range widens as the table lives — it is the range an incremental export window must fall inside. ExportTableToPointInTime with ExportType='INCREMENTAL_EXPORT' then accepts a 16-minute window (ExportStatus 'IN_PROGRESS' or 'COMPLETED') and refuses a 1-minute window with the modelled error (Smithy: ExportTableToPointInTime errors include InvalidExportTimeException, client, HTTP 400) code 'InvalidExportTimeException' and message 'Difference between export period from time and export period to time is less than 15 minutes' — fixed text — leaving ListExports showing only the accepted export.
Emulator: the check never reaches either export call: it waits for the restorable range reported by DescribeContinuousBackups to span more than 15 minutes and that condition is never satisfied, so the check ends with 'Check timed out after 60.0s'. Hypothesis: the emulator reports EarliestRestorableDateTime and LatestRestorableDateTime as the same instant (or a fixed-width range) rather than a window anchored at PITR enablement that extends to now, so the spread stays at zero.
Why: a client cannot discover the restorable window to pick a legal incremental-export window, every incremental export is therefore unconstrained in the emulator, and the InvalidExportTimeException path (and the ListExports bookkeeping behind it) is unreachable.
Fix: the DynamoDB continuous-backups state for a PITR-enabled table — EarliestRestorableDateTime must be pinned at enablement and LatestRestorableDateTime must track the current time — and ExportTableToPointInTime's INCREMENTAL_EXPORT window validation must then enforce the 15-minute minimum with InvalidExportTimeException.
Traces: dynamodb__s3.export-incremental-window-below-minimum-rejected

## Evidence
From the behavioral replay: dynamodb__s3.export-incremental-window-below-minimum-rejected → 'Error: Check timed out after 60.0s'.
The check's sequence is CreateTable → wait ACTIVE → UpdateContinuousBackups (PITR) → wait PointInTimeRecoveryStatus 'ENABLED' → _wait_for_restorable_spread(ddb, table, timedelta(minutes=17)), and only then the two ExportTableToPointInTime calls. The timeout occurs before the first export call, i.e. the restorable spread reported by DescribeContinuousBackups never reached 17 minutes.
Expected once the window is reported: the 16-minute window is accepted with ExportStatus in ('IN_PROGRESS','COMPLETED'); the 1-minute window is refused with ('InvalidExportTimeException', 'Difference between export period from time and export period to time is less than 15 minutes'); ListExports then holds only the accepted export's ExportArn.

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run dynamodb__s3.export-incremental-window-below-minimum-rejected.
3. Manual repro: CreateTable (PAY_PER_REQUEST, pk S), wait ACTIVE, UpdateContinuousBackups with PointInTimeRecoveryEnabled=true, wait for PointInTimeRecoveryStatus 'ENABLED'.
4. Poll DescribeContinuousBackups and print LatestRestorableDateTime - EarliestRestorableDateTime — against AWS this spread grows past 15 minutes; against the emulator it does not widen.
5. Then call ExportTableToPointInTime with ExportType='INCREMENTAL_EXPORT' and a 1-minute IncrementalExportSpecification window and compare the error code and message with AWS's InvalidExportTimeException.
