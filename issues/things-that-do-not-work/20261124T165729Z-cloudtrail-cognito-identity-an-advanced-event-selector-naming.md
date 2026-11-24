Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__cognito-identity.identity-pool-data-events-are-delivered-only-once-a-selector-names-them
Issue-Key: cloudtrail__cognito-identity.identity-pool-data-events-are-delivered-only-once-a-selector-names-them|run#65|Timeout|time.sleep(_DELIVERY_POLL_S)
Issue-Commit: 061fcc8ecfb1ea3767cdd9cc8542bb3501498b07

# cloudtrail__cognito-identity: an advanced event selector naming AWS::Cognito::IdentityPool round-trips but delivers no data-event log file, so identity-pool GetId/GetOpenIdToken are never audited and the logins redaction is unobservable

> _Note: this issue was identified against the code base at commit `061fcc8ecfb1ea3767cdd9cc8542bb3501498b07`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEventSelectors

## Description
One net-new root cause, grounded in the single surviving failing check of this service heading.

Cause 1 of 1: a trail whose advanced event selector names eventCategory=Data + resources.type=AWS::Cognito::IdentityPool delivers no log file at all, so Cognito identity-pool data events are never recorded.

AWS: Smithy model (services/service-com-amazonaws-cloudtrail-cloudtrail-20131101) — PutEventSelectors takes TrailName (required) plus EventSelectors / AdvancedEventSelectors and echoes the stored selectors; GetTrailStatus returns IsLogging, LatestDeliveryTime, LatestDeliveryError. The classification and delivery contract itself is recorded behavior only (AWS's Cognito-identity CloudTrail page and the CloudTrail log-file reference): cognito-identity GetId and GetOpenIdToken are DATA events — withheld by a default management-only trail, and captured once an advanced event selector of {Field: eventCategory, Equals: ["Data"]} + {Field: resources.type, Equals: ["AWS::Cognito::IdentityPool"]} is accepted. When captured, CloudTrail writes gzipped log files (first bytes 1f 8b) under AWSLogs/<account-id>/CloudTrail/<region>/<yyyy>/<mm>/<dd>/ whose inflated body is a JSON object with a top-level "Records" list, and each identity-pool record carries eventSource "cognito-identity.amazonaws.com", eventCategory "Data", managementEvent false, a resources entry of type "AWS::Cognito::IdentityPool" whose ARN is the pool addressed (arn:aws:cognito-identity:<region>:<account-id>:identitypool/<pool-id>) and whose accountId is the caller's account, and requestParameters.logins kept as a MAP whose provider-name KEY is preserved while its token VALUE is replaced by the exact literal string "HIDDEN_DUE_TO_SECURITY_REASONS" (the fixed text is that literal; the key is the request's provider name cognito-idp.<region>.amazonaws.com/<user-pool-id>). The presented ID token must appear nowhere in the delivered bytes. The same data events must NOT appear in LookupEvents, which returns management events only, and GetTrailStatus on a trail that has delivered must report LatestDeliveryTime at or after the instant of the last delivered record with no LatestDeliveryError.

Emulator: every setup call succeeds — the user pool, app client and two identity pools are created, CreateTrail/StartLogging report IsLogging=true, PutEventSelectors echoes the advanced selectors verbatim and GetEventSelectors reads them back — but ZERO objects are ever written under AWSLogs/<account>/CloudTrail/<region>/. The check's warm-up leg, which only needs ONE management record (cognito-identity:TagResource) to be delivered before it proceeds, never sees a delivered object and spins on its delivery poll until the harness kills the check (reported as "Check timed out after 60.0s"). Nothing downstream — the Data classification, managementEvent=false, the resources entry, the logins redaction, the LookupEvents exclusion, LatestDeliveryTime — is reachable because no log file exists.

Why: a delivered log file is the ONLY place a data event is observable, so a client cannot distinguish "my selector did not match" from "the delivery pipeline is dead". Security tooling that reads identity-pool data events out of the trail bucket sees an account with no federated activity, the token-redaction guarantee is unverifiable (a consumer cannot confirm credentials are not being written to an audit log), and the default-off/selector-on partition that controls data-event billing has no observable effect whatsoever.

Fix: the CloudTrail trail delivery pipeline in the cloudtrail service crate — advanced-event-selector evaluation (eventCategory=Data matching on resources.type) and the S3 log-file writer that must gzip a Records batch to the trail's bucket prefix and advance GetTrailStatus.LatestDeliveryTime — plus the cognito-identity side emitting GetId/GetOpenIdToken as data events with the identity-pool resources entry and the logins-value redaction.

Related: open issue 20261029T173328Z-cloudtrail-records-and-delivers-no-audit-record-for.md describes this surface as its Cause 2, but the replay report does NOT mark this check as OWNED by it (its recorded replay key for this check no longer matches the failing step), so this check is currently carried by no issue and is filed here.

Traces: cloudtrail__cognito-identity.identity-pool-data-events-are-delivered-only-once-a-selector-names-them

## Evidence
Replay report, suite simparity-behavioral-aws-v1, service heading `### cloudtrail__cognito-identity`:
  [mismatch] cloudtrail__cognito-identity.identity-pool-data-events-are-delivered-only-once-a-selector-names-them — Error: Check timed out after 60.0s
Not marked ALREADY FILED and not marked OWNED by any open issue.

Expected (AWS): after PutEventSelectors with AdvancedEventSelectors [{eventCategory Equals [Data]}, {resources.type Equals [AWS::Cognito::IdentityPool]}] is accepted and read back, a gzipped object appears under AWSLogs/<account>/CloudTrail/<region>/<yyyy>/<mm>/<dd>/ whose inflated Records list contains a cognito-identity.amazonaws.com record per GetId / GetOpenIdToken with eventCategory "Data", managementEvent false, resources[].type "AWS::Cognito::IdentityPool" / ARN the pool / accountId the caller's account, and requestParameters.logins[<provider-name>] == "HIDDEN_DUE_TO_SECURITY_REASONS"; the ID token appears nowhere in the file; LookupEvents returns the management marker but none of those data events; GetTrailStatus reports LatestDeliveryTime advanced past the marker with no LatestDeliveryError.

Actual (emulator): the trail is created, StartLogging succeeds, GetTrailStatus reports IsLogging=true, PutEventSelectors echoes the submitted selectors and GetEventSelectors returns them, and the bucket prefix stays EMPTY. The check cannot even clear its warm-up barrier, which requires a single delivered management record for cognito-identity:TagResource, so it exhausts the harness timeout while polling. All operations that are asked of the emulator (CreateUserPool, CreateUserPoolClient, AdminCreateUser, AdminSetUserPassword, InitiateAuth, CreateIdentityPool, CreateBucket, PutBucketPolicy, CreateTrail, StartLogging, GetTrailStatus, PutEventSelectors, GetEventSelectors, TagResource, GetId, GetOpenIdToken) return success, so the divergence is isolated to recording and delivery.

## Steps to Reproduce
Against the booted emulator:
1. Create a Cognito user pool, an app client with ALLOW_USER_PASSWORD_AUTH, a user with a permanent password, and InitiateAuth to obtain an IdToken.
2. Create two identity pools with that user pool as a CognitoIdentityProvider.
3. Create an S3 bucket with the AWSCloudTrailAclCheck + AWSCloudTrailWrite bucket policy, then CreateTrail(Name, S3BucketName=<bucket>) and StartLogging; confirm GetTrailStatus reports IsLogging=true.
4. Call cognito-identity TagResource on a pool and poll ListObjectsV2 under AWSLogs/<account>/CloudTrail/<region>/ — no object is ever written (management delivery already fails here).
5. PutEventSelectors(TrailName, AdvancedEventSelectors=[{Name, FieldSelectors=[{Field: eventCategory, Equals: [Data]}, {Field: resources.type, Equals: [AWS::Cognito::IdentityPool]}]}, {Name, FieldSelectors=[{Field: eventCategory, Equals: [Management]}]}]) — it is accepted and GetEventSelectors reads it back.
6. Call GetId and GetOpenIdToken against the second pool with Logins={<provider-name>: <IdToken>}, then list and gunzip every object under AWSLogs/<account>/CloudTrail/<region>/. Zero objects exist, so no record, no eventCategory "Data", and no "HIDDEN_DUE_TO_SECURITY_REASONS" redaction can be observed.
