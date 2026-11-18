Issue-Source: trace-sweep-divergence
Issue-Trace: secretsmanager-secret-empty-description-tags-remove-last-scheduled-deletion-restore-and-not-found
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: secretsmanager-secret-empty-description-tags-remove-last-scheduled-deletion-restore-and-not-found secretsmanager-secret-version-explicit-awscurrent-with-custom-stages-moved-and-removed
Issue-Shape-Id: com.amazonaws.secretsmanager#secretsmanager
Issue-Key: secretsmanager|DescribeSecret|2xx->2xx|$.CreatedDate
Issue-Key: secretsmanager|DescribeSecret|2xx->2xx|$.LastChangedDate
Issue-Key: secretsmanager|DescribeSecret|2xx->2xx|$.Tags
Issue-Key: secretsmanager|DeleteSecret|2xx->2xx|$.DeletionDate
Issue-Key: secretsmanager|DescribeSecret|2xx->2xx|$.DeletedDate
Issue-Key: secretsmanager|GetSecretValue|2xx->2xx|$.CreatedDate
Issue-Commit: ec2ef26d41329427f76ddd397b9730fbe7244eae

# secretsmanager: response fields are shaped wrong — timestamp members carry sub-millisecond precision and an emptied Tags list is omitted (2 causes)

> _Note: this issue was identified against the code base at commit `ec2ef26d41329427f76ddd397b9730fbe7244eae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSecret

## Description
Two net-new causes, both in how this service RENDERS a successful response (every affected step answered HTTP 200 with the right members otherwise). Cause count: 2.

=== Cause 1 of 2: every timestamp member is serialized at sub-millisecond precision ===
AWS: Smithy (com.amazonaws.secretsmanager#secretsmanager, awsJson1_1): DescribeSecretResponse$CreatedDate (TimestampType), $LastChangedDate (LastChangedDateType), $DeletedDate (DeletedDateType), DeleteSecretResponse$DeletionDate (DeletionDateType) and GetSecretValueResponse$CreatedDate (CreatedDateType) are all plain `timestamp` members with no @timestampFormat override, so the awsJson1_1 default — epoch-seconds as a JSON number — applies. Recorded behavior: in every recorded response each of those members is an epoch-seconds number at AWS's date resolution, i.e. at most three fractional digits (millisecond resolution); the corpus asserts the SHAPE of these members, not their instants, because the instant of a run is a per-run value and is not a requirement.
Emulator: emits full f64 precision for all of them, so the shaped assertion rejects every timestamp member on every step it appears on: DescribeSecret CreatedDate/LastChangedDate = 1794970788.041994, DescribeSecret DeletedDate = 1794970788.088883, DeleteSecret DeletionDate = 1797562788.088883, GetSecretValue CreatedDate = 1794970798.1360521 (six and even seven fractional digits). The single helper every handler routes a stored instant through is crate::state::timestamp, which builds the wire value with DateTime::from_secs_f64(duration_since(UNIX_EPOCH).as_secs_f64()) and therefore keeps every sub-millisecond digit of the host clock plus binary-float noise. Hypothesis: the sub-millisecond digits are exactly what the shaped assertion rejects — the magnitudes are plausible and ordered correctly (CreatedDate <= LastChangedDate, DeletionDate in the future), and the failure is uniform across all five members and all three operations rather than specific to any one value.
Why: a client that round-trips, hashes or formats these fields does not see AWS's shape — a date AWS would hand back as 1794970788.042 comes back as 1794970788.041994, so a response digest, a recorded-fixture comparison, or a strict date parser that accepts millisecond precision disagrees with the emulator even though the underlying instant is fine. It also makes the emulator distinguishable from AWS on every secret-metadata read, which is what breaks replay-based tooling.
Fix: crate::state::timestamp in the secretsmanager crate's state module — the one place a stored SystemTime becomes a wire DateTime for this service — must truncate the instant to millisecond resolution before constructing the DateTime, so every operation's timestamp members serialize the way AWS's do. No per-handler change is needed; DescribeSecret, DeleteSecret and GetSecretValue all call this helper.
Traces: secretsmanager-secret-empty-description-tags-remove-last-scheduled-deletion-restore-and-not-found#task#3 (CreatedDate, LastChangedDate; also task#5, task#7), #task#9 (also task#14), #task#10 (DeletionDate from DeleteSecret; also task#12, task#15), #task#11 (CreatedDate, DeletedDate, LastChangedDate), and secretsmanager-secret-version-explicit-awscurrent-with-custom-stages-moved-and-removed#task#3 (GetSecretValue CreatedDate; also task#5, task#7).

=== Cause 2 of 2: DescribeSecret omits Tags instead of reporting the empty list once the secret's last tag is removed ===
AWS: Smithy: DescribeSecretResponse$Tags targets TagListType (a list of Tag). The operation's doc says only "Secrets Manager only returns fields that have a value in the response", so this is recorded behavior: after the secret's last tag is removed (UntagResource), DescribeSecret still reports Tags as a PRESENT but EMPTY list — "Tags": [] — and keeps reporting it as [] while the secret is scheduled for deletion. The same corpus shows the member legitimately ABSENT earlier in the same fixture, on the freshly created, never-tagged secret (that step passed with no Tags member on either side), so the recorded rule is that an emptied tag list is reported empty rather than dropped.
Emulator: drops the member whenever the list is empty. StoredSecret::tags_out in the secretsmanager state module is `(!self.tags.is_empty()).then(|| ...)`, i.e. it answers None for an empty list, so DescribeSecret's `tags: content.tags_out()` emits nothing. Replay: "$.Tags: missing (expected [], got None)" at task#9 (after the last tag was removed) and task#11 (same secret, scheduled for deletion).
Why: a client cannot confirm a tag removal from the response — the member it must read to see "this secret now has no tags" is simply not there. Code that indexes the field unconditionally (response["Tags"] in botocore, a non-optional field in a typed SDK) raises against the emulator where AWS hands back an empty list, and a remove-then-verify loop can never observe the post-removal state.
Fix: the projection of the stored tag list for this operation — tags_out in the secretsmanager crate's state module, as used by the DescribeSecret handler — must report the empty list for a secret whose tags were removed rather than omitting the member.
Traces: secretsmanager-secret-empty-description-tags-remove-last-scheduled-deletion-restore-and-not-found#task#9, #task#11.

## Evidence
Wire corpus simparity-wire-aws-v1, replayed against the emulator built from this checkout (32 fixtures, 260 steps, 16 mismatched). The five clusters bundled here, verbatim from the replay:

1) DescribeSecret, fixture secretsmanager-secret-empty-description-tags-remove-last-scheduled-deletion-restore-and-not-found, step task#3 (also task#5, task#7), expected 200 / actual 200:
   $.CreatedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794970788.041994)
   $.LastChangedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794970788.041994)

2) DescribeSecret, same fixture, step task#9 (also task#14), expected 200 / actual 200:
   $.CreatedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794970788.041994)
   $.LastChangedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794970788.080958)
   $.Tags: missing (expected [], got None)

3) DeleteSecret, same fixture, step task#10 (also task#12, task#15), expected 200 / actual 200:
   $.DeletionDate: value_mismatch (expected '<shaped-not-asserted>', got 1797562788.088883)

4) DescribeSecret, same fixture, step task#11, expected 200 / actual 200:
   $.CreatedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794970788.041994)
   $.DeletedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794970788.088883)
   $.LastChangedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794970788.088883)
   $.Tags: missing (expected [], got None)

5) GetSecretValue, fixture secretsmanager-secret-version-explicit-awscurrent-with-custom-stages-moved-and-removed, step task#3 (also task#5, task#7), expected 200 / actual 200:
   $.CreatedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794970798.1360521)

Expected (AWS): each timestamp member an epoch-seconds JSON number at AWS's millisecond date resolution (at most three fractional digits); Tags present as [] on a DescribeSecret for a secret whose last tag was removed.
Actual (emulator): timestamp members with six or seven fractional digits; no Tags member at all once the tag list is empty.

The literal epoch values above are per-run instants and are NOT the requirement — only the shape of the encoding is. The code behind both causes: crate::state::timestamp (DateTime::from_secs_f64(as_secs_f64())) and StoredSecret::tags_out ((!self.tags.is_empty()).then(...)), both in services/service-com-amazonaws-secretsmanager-secretsmanager/src/state.rs; consumed by ops/describe_secret.rs (tags/created_date/last_changed_date/deleted_date) and the DeleteSecret and GetSecretValue handlers.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a secretsmanager client at it.
2. create_secret(Name=N, SecretString='...'), then describe_secret(SecretId=N) and read the raw JSON body (not the SDK-parsed datetime): CreatedDate and LastChangedDate come back with six fractional digits (e.g. 1794970788.041994). AWS renders these epoch-seconds numbers at millisecond resolution (at most three fractional digits). Same for get_secret_value(SecretId=N) -> CreatedDate.
3. tag_resource(SecretId=N, Tags=[{'Key':'k','Value':'v'}]), then untag_resource(SecretId=N, TagKeys=['k']).
4. describe_secret(SecretId=N): the response has NO Tags member. Expected: Tags present and equal to [].
5. delete_secret(SecretId=N, RecoveryWindowInDays=30): DeletionDate again carries sub-millisecond digits. describe_secret(SecretId=N) on the scheduled secret: DeletedDate/CreatedDate/LastChangedDate likewise, and Tags is still absent instead of [].
6. Re-replay the listed fixture steps of simparity-wire-aws-v1 to confirm both causes are closed.
