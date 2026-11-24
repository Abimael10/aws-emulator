Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__cognito-identity.identity-pool-data-events-are-delivered-only-once-a-selector-names-them
Issue-Key: cloudtrail__cognito-identity.identity-pool-data-events-are-delivered-only-once-a-selector-names-them|run#65|Timeout|time.sleep(_DELIVERY_POLL_S)
Issue-Commit: e597f2d78fa346ea123ebf70f6b121a2210ad722

# cloudtrail__cognito-identity: a logging trail delivers no log files at all, so Cognito identity-pool data events selected by an advanced event selector are never observable

> _Note: this issue was identified against the code base at commit `e597f2d78fa346ea123ebf70f6b121a2210ad722`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEventSelectors

## Description
Cause 1 of 1: a trail that reports IsLogging=true and whose advanced event selector naming Cognito identity pools round-trips through GetEventSelectors still writes nothing to its S3 log bucket, for either management or data events.

AWS: recorded behavior only (Cognito's CloudTrail page; the CloudTrail record-contents and PutEventSelectors references). A trail created with default selectors captures management events only and reports `EventSelectors[0].DataResources == []` with no `AdvancedEventSelectors` member. cognito-identity GetId and GetOpenIdToken are DATA events: withheld by that default trail, and captured once PutEventSelectors is given AdvancedEventSelectors of {Field: eventCategory, Equals: ["Data"]} + {Field: resources.type, Equals: ["AWS::Cognito::IdentityPool"]}. Delivered records arrive as gzip objects (first bytes 1f 8b) under `AWSLogs/<account>/CloudTrail/<region>/<yyyy>/<mm>/<dd>/` whose inflated body is a JSON object with a `Records` list; a data-event record carries eventSource "cognito-identity.amazonaws.com", eventCategory "Data", managementEvent false, a `resources` entry of type `AWS::Cognito::IdentityPool` whose ARN is the pool addressed and whose accountId is the caller's account, and `requestParameters.logins.<provider>` redacted to the exact fixed string `HIDDEN_DUE_TO_SECURITY_REASONS`. The remaining identity-pool operations (e.g. TagResource) are MANAGEMENT events: eventCategory "Management", managementEvent true. Delivery is documented at an average of about 5 minutes on real AWS; an emulator is expected to deliver within the replay budget. Data events never appear in LookupEvents.

Emulator: the full setup succeeds — CreateUserPool/CreateUserPoolClient/InitiateAuth yield an ID token, two identical identity pools are created, the log bucket and its CloudTrail bucket policy are accepted, CreateTrail reports the documented management-only default, StartLogging is accepted and GetTrailStatus reports IsLogging=true — and then no object is ever written under `AWSLogs/<account>/CloudTrail/<region>/`. The check's own positive control (a management cognito-identity:TagResource marker) is never delivered either, so neither the warm-up barrier nor the data-event half can be reached, and the check exhausts its budget: `Error: Check timed out after 60.0s`.

Why: a delivered log file is the only place a data event is observable (data events are excluded from LookupEvents by design). With nothing delivered, a consumer cannot distinguish "my advanced event selector was not applied" from "the delivery pipeline is dead", the eventCategory/managementEvent classification of Cognito operations is unverifiable, and the `logins` token-redaction contract — the reason a security reviewer enables these data events — can never be confirmed. Any cost control, Athena query or SIEM ingest keyed on the trail's S3 prefix sees an empty bucket.

Fix: the cloudtrail service crate's trail delivery pipeline — the component that evaluates basic and advanced event selectors against recorded events and the S3 log-file writer that gzips and PUTs them under the documented `AWSLogs/<account>/CloudTrail/<region>/<date>/` prefix — plus the cognito-identity-side event classification that must mark GetId/GetOpenIdToken as eventCategory "Data"/managementEvent false with an `AWS::Cognito::IdentityPool` resources entry and redact `requestParameters.logins.*` to `HIDDEN_DUE_TO_SECURITY_REASONS`.

Relation to an existing open issue: 20261029T173328Z-cloudtrail-records-and-delivers-no-audit-record-for.md lists this check id under its Cause 2, but the replay report did NOT mark this failure OWNED — its recorded replay key no longer matches the failure observed now, so that issue does not carry this check and the failure would otherwise go unclosed. Please reconcile the two: fixing the delivery pipeline described there should also close this, and this issue names the exact check to re-replay.

Traces: cloudtrail__cognito-identity.identity-pool-data-events-are-delivered-only-once-a-selector-names-them.

## Evidence
Expected (AWS): with the trail logging, (a) the management cognito-identity:TagResource marker is delivered as a gzip object under `AWSLogs/<account>/CloudTrail/<region>/<yyyy>/<mm>/<dd>/` whose record has eventCategory "Management" and managementEvent true; (b) after PutEventSelectors(AdvancedEventSelectors=[{eventCategory Equals ["Data"], resources.type Equals ["AWS::Cognito::IdentityPool"]}, {eventCategory Equals ["Management"]}]) is accepted and read back verbatim by GetEventSelectors (with `EventSelectors` now absent), GetId and GetOpenIdToken against the selected pool are delivered as records with eventSource "cognito-identity.amazonaws.com", eventCategory "Data", managementEvent false, a resources entry {type: "AWS::Cognito::IdentityPool", ARN: <the pool's arn>, accountId: <caller account>}, and requestParameters.logins.<provider> == "HIDDEN_DUE_TO_SECURITY_REASONS"; (c) the identical calls made against the other pool BEFORE the selector existed are never delivered.

Actual (emulator): zero objects exist under the trail's `AWSLogs/<account>/CloudTrail/<region>/` prefix for the whole run, including for the management-event positive control, so phase (a) never completes and the check is killed:
  [mismatch] cloudtrail__cognito-identity.identity-pool-data-events-are-delivered-only-once-a-selector-names-them — Error: Check timed out after 60.0s

Every call the check makes before polling — CreateUserPool, CreateUserPoolClient, AdminCreateUser, AdminSetUserPassword, InitiateAuth, CreateIdentityPool x2, CreateBucket, PutBucketPolicy, PutBucketTagging, CreateTrail, GetEventSelectors, StartLogging, GetTrailStatus, TagResource, GetId, GetOpenIdToken — succeeds, so the divergence is isolated to recording and delivery rather than to the audited operations. Account id, region, bucket/trail names, pool ids and the generated marker tokens are per-run values; the requirement violated is that a logging trail delivers log files at all and that the Cognito data events selected by the advanced selector appear in them with the members above.

## Steps to Reproduce
1. cognito-idp: CreateUserPool + CreateUserPoolClient(ALLOW_USER_PASSWORD_AUTH), AdminCreateUser, AdminSetUserPassword(Permanent=True), InitiateAuth(USER_PASSWORD_AUTH) -> IdToken; provider name is `cognito-idp.<region>.<domain>/<UserPoolId>`.
2. cognito-identity: CreateIdentityPool twice (pool A and pool B), both with AllowUnauthenticatedIdentities=False and that provider as a CognitoIdentityProvider.
3. s3: CreateBucket + PutBucketPolicy granting cloudtrail.amazonaws.com s3:GetBucketAcl on the bucket and s3:PutObject on `AWSLogs/<account>/*`.
4. cloudtrail: CreateTrail(S3BucketName=<bucket>, IsMultiRegionTrail=False, IncludeGlobalServiceEvents=False); confirm GetEventSelectors reports DataResources == [] and no AdvancedEventSelectors; StartLogging; confirm GetTrailStatus IsLogging is true.
5. Warm-up / positive control: call cognito-identity TagResource on pool B, then list and gunzip objects under `AWSLogs/<account>/CloudTrail/<region>/<yyyy>/<mm>/<dd>/`.
   Expected: an object arrives whose Records contain the TagResource event with eventCategory "Management" and managementEvent true. Actual: no objects are ever listed.
6. Withheld leg: GetId + GetOpenIdToken against pool A while no data selector exists.
7. PutEventSelectors(TrailName=<trail>, AdvancedEventSelectors=[{Name:..., FieldSelectors:[{Field:"eventCategory",Equals:["Data"]},{Field:"resources.type",Equals:["AWS::Cognito::IdentityPool"]}]},{Name:..., FieldSelectors:[{Field:"eventCategory",Equals:["Management"]}]}]); GetEventSelectors must echo it verbatim and no longer report EventSelectors.
8. Delivered leg: GetId + GetOpenIdToken against pool B, then re-read the log prefix.
   Expected: records for both event names with eventCategory "Data", managementEvent false, the AWS::Cognito::IdentityPool resources entry naming pool B's ARN and the caller's accountId, and requestParameters.logins.<provider> == "HIDDEN_DUE_TO_SECURITY_REASONS"; pool A's earlier calls absent. Actual: the bucket is still empty, so neither the delivery nor the withholding can be observed.
