Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.associate-dataset-kms-key-nonexistent-key-rejected cloudwatch.associate-dataset-kms-key-cross-account-key-denied cloudwatch.associate-dataset-kms-key-missing-permission-denied cloudwatch.associate-dataset-kms-key-reassociation-conflict-rejected cloudwatch.duplicate-kms-association-rejected cloudwatch.kms-key-access-errors cloudwatch.associate-dataset-kms-key-disabled-key-rejected cloudwatch.dataset-kms-key-disabled-key-rejected cloudwatch.get-metric-widget-image-malformed-spec-rejected
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.associate-dataset-kms-key-nonexistent-key-rejected|run#18|AssertionError|assert code == "AccessDenied", (
Issue-Key: cloudwatch.associate-dataset-kms-key-cross-account-key-denied|run#9|AssertionError|assert code == "ValidationError", f"expected ValidationError, got {code!r} ({message!r})"
Issue-Key: cloudwatch.associate-dataset-kms-key-missing-permission-denied|run#15|AssertionError|assert message == (
Issue-Key: cloudwatch.associate-dataset-kms-key-reassociation-conflict-rejected|run#20|AssertionError|assert message == (
Issue-Key: cloudwatch.duplicate-kms-association-rejected|run#20|AssertionError|assert message == (
Issue-Key: cloudwatch.kms-key-access-errors|run#12|AssertionError|assert message == (
Issue-Key: cloudwatch.associate-dataset-kms-key-disabled-key-rejected|run#16|AssertionError|assert message == f"KMS key {key_arn} is disabled.", (
Issue-Key: cloudwatch.dataset-kms-key-disabled-key-rejected|run#16|AssertionError|assert message == f"KMS key {target_arn} is disabled.", (
Issue-Key: cloudwatch.get-metric-widget-image-malformed-spec-rejected|run#11|AssertionError|assert message_empty == _EMPTY_MESSAGE, (
Issue-Commit: 4bf99a6da981b1c03b4c2a4da03064374e7befb5

# CloudWatch dataset-KMS refusals answer the wrong error class and emulator-authored messages, and GetMetricWidgetImage's missing-metrics refusal is re-worded (5 causes)

> _Note: this issue was identified against the code base at commit `4bf99a6da981b1c03b4c2a4da03064374e7befb5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateDatasetKmsKey

## Description
This issue bundles 5 independent net-new root causes in the CloudWatch control plane. Every cause is independent; all 5 must be closed before submitting.

NOTE on prior work: the OPEN issue 20261031T235853Z-cloudwatch-7-net-new-root-causes-associatedatasetkmskey-validates.md describes AssociateDatasetKmsKey as performing no KMS validation at all (every key ARN accepted with HTTP 200). That is no longer what the emulator does — the operation now DOES refuse, so these checks fail on a different divergence: the error CLASS and the message TEXT of the refusal. Three of that issue's check ids still carry its replay key and are owned by it; the eight listed here do not and are net-new. The OPEN issue 20261105T062419Z-cloudwatch-4-net-new-root-causes-in-getmetricwidgetimage.md covers other GetMetricWidgetImage divergences but does not name the missing-metrics refusal message of Cause 5.

Cause 1 of 5: AssociateDatasetKmsKey refuses a well-formed but unresolvable key ARN with the modelled KmsKeyNotFoundException instead of the generic AccessDenied the service actually answers.
AWS: Smithy: AssociateDatasetKmsKey takes DatasetIdentifier (@required, @length 1..2048, @pattern ^(default|arn:[a-zA-Z0-9-]+:cloudwatch:[a-zA-Z0-9-]*:\d{12}:dataset/default)$) and KmsKeyArn (@required, @length 20..2048, @pattern ^arn:[a-zA-Z0-9-]+:kms:[a-zA-Z0-9-]+:\d{12}:key/[a-f0-9-]+$); it models ConflictException (409), KmsAccessDeniedException (400), KmsKeyDisabledException (400), KmsKeyNotFoundException (400) and ResourceNotFoundException (404). Recorded behavior: a key ARN in the caller's own account and Region that resolves to no key is refused with HTTP 400, Error.Code "AccessDenied" and the message "Unable to validate KMS key <kms-key-arn>. Verify the key exists and the key policy grants kms:DescribeKey to the CloudWatch service principal." — fixed text "Unable to validate KMS key ", ". Verify the key exists and the key policy grants kms:DescribeKey to the CloudWatch service principal."; the only variable part is the KmsKeyArn taken verbatim from the request. The service does NOT distinguish "key absent" from "key unusable" on the wire: the modelled Kms* shapes are not what it returns here. The dataset's existing association is left untouched and the positive control (a key whose policy grants the CloudWatch service principal kms:DescribeKey/GenerateDataKey/Encrypt/Decrypt/ReEncrypt*) associates.
Emulator: answers Error.Code "KmsKeyNotFoundException" with the model's documentation sentence "The specified Amazon Web Services Key Management Service (Amazon Web Services KMS) key could not be found. Verify that the key Amazon Resource Name (ARN) is correct, that the key exists, and that it is in the same Amazon Web Services Region as the resource."
Why: a client branching on Error.Code sees a class CloudWatch never returns for this input, so retry/remediation logic keyed on AccessDenied never fires, and the message does not name the key the caller must repair.
Fix: the AssociateDatasetKmsKey handler's key-resolution refusal path — an ARN that resolves to no key must be refused under the generic AccessDenied code with the "Unable to validate KMS key …" sentence, not under the modelled not-found shape.
Traces: cloudwatch.associate-dataset-kms-key-nonexistent-key-rejected.

Cause 2 of 5: a KmsKeyArn whose account segment is not the caller's account is not validated as an account mismatch at all.
AWS: Smithy: KmsKeyArn's @pattern pins a 12-digit account segment, and the operation's documentation states the key "must be in the same Amazon Web Services Region as the dataset" and that the caller must hold kms:Decrypt on it. Recorded behavior: a syntactically valid key ARN whose account segment names another account is refused with HTTP 400, Error.Code "ValidationError" and the message "KMS key account <key-account-id> does not match caller account <caller-account-id>" — fixed text "KMS key account " and " does not match caller account "; the variable parts are the account segment of the KmsKeyArn in the request and the calling identity's own account id. The dataset's association is unchanged.
Emulator: the foreign-account ARN is sent down the same key-resolution path as any other ARN and refused with Error.Code "KmsKeyNotFoundException" and the not-found documentation sentence; no caller-account comparison is made anywhere in the handler (it only compares the ARN's Region segment to the request Region).
Why: a cross-account configuration mistake is reported as a missing key rather than as the validation error that names both accounts, so the caller is told to check that the key exists instead of that it is in the wrong account.
Fix: AssociateDatasetKmsKey's input validation — compare the KmsKeyArn's account segment with the caller's account before resolving the key, and refuse a mismatch as ValidationError with that sentence.
Traces: cloudwatch.associate-dataset-kms-key-cross-account-key-denied.

Cause 3 of 5: the under-permissioned-key refusal carries the Smithy documentation paragraph instead of AWS's one-sentence AccessDenied message.
AWS: Smithy: KmsAccessDeniedException's documentation is the long paragraph beginning "The operation was denied because either the calling principal lacks the required …"; that is DOCUMENTATION, not the wire message. Recorded behavior: a key whose policy grants the CloudWatch service principal nothing (and the same case presented as a re-association over an already-working key) is refused with HTTP 400, Error.Code "AccessDenied" and the message "Unable to validate KMS key <kms-key-arn>. Verify the key exists and the key policy grants kms:DescribeKey to the CloudWatch service principal." — same fixed text as Cause 1, with the request's KmsKeyArn as the only variable part. The prior association is untouched, and granting that same key the documented permissions makes the identical call succeed.
Emulator: Error.Code is correct ("AccessDenied") but the message is the KmsAccessDeniedException documentation paragraph: "The operation was denied because either the calling principal lacks the required Amazon Web Services Key Management Service (Amazon Web Services KMS) permission on the key, or the key policy does not grant Amazon CloudWatch the permissions it needs to use the key. Verify that the caller has kms:Decrypt permission on the key, and that the key policy grants the CloudWatch service principal the kms:DescribeKey, kms:GenerateDataKey, kms:Encrypt, kms:Decrypt, and kms:ReEncrypt* permissions described in AssociateDatasetKmsKey."
Why: the message is the only machine-readable statement of WHICH key failed and what to repair; a client matching AWS's wording does not match, and the emulator's text never names the offending key ARN.
Fix: the refusal-message construction for an unusable key in the dataset-KMS precondition path reached from the AssociateDatasetKmsKey handler (the KeyCondition::Unusable branch) — it must emit the "Unable to validate KMS key <arn>. Verify the key exists and the key policy grants kms:DescribeKey to the CloudWatch service principal." sentence, not the shape's documentation.
Traces: cloudwatch.associate-dataset-kms-key-missing-permission-denied, cloudwatch.associate-dataset-kms-key-reassociation-conflict-rejected, cloudwatch.duplicate-kms-association-rejected, cloudwatch.kms-key-access-errors.

Cause 4 of 5: the disabled-key refusal carries the Smithy documentation paragraph instead of AWS's disabled-key sentence.
AWS: Smithy: the operation documents "It must be enabled and not pending deletion", and KmsKeyDisabledException's documentation is "The specified … KMS key is disabled or pending deletion. Re-enable the key …" — again documentation, not the wire message. Recorded behavior: associating a key that is in state Disabled is refused with HTTP 400, Error.Code "AccessDenied" and the message "KMS key <kms-key-arn> is disabled." — fixed text "KMS key " and " is disabled."; the variable part is the KmsKeyArn from the request. The dataset's association is unchanged, and re-enabling the key makes the identical call succeed.
Emulator: Error.Code is correct ("AccessDenied") but the message is the KmsKeyDisabledException documentation paragraph: "The specified Amazon Web Services Key Management Service (Amazon Web Services KMS) key is disabled or pending deletion. Re-enable the key (or restore it, if it is pending deletion) and retry the operation."
Why: a caller cannot tell from the message which key is disabled, and anything matching AWS's wording for this condition fails to match.
Fix: the same unusable-key refusal path as Cause 3, but the disabled-key branch specifically — it must render "KMS key <arn> is disabled."
Traces: cloudwatch.associate-dataset-kms-key-disabled-key-rejected, cloudwatch.dataset-kms-key-disabled-key-rejected.

Cause 5 of 5: GetMetricWidgetImage's refusal of a widget spec with no "metrics" property is worded by the emulator.
AWS: Smithy: GetMetricWidgetImage takes MetricWidget (@required, a JSON document) and models no errors of its own, so the refusal is the service-common query error envelope. Recorded behavior: GetMetricWidgetImage(MetricWidget="{}") — structurally valid JSON that omits the required "metrics" property — is refused with Error.Code "ValidationError" and the message "Should have required property 'metrics'", the complete fixed text of that refusal (it is a JSON-schema diagnostic; there is no variable part). The sibling refusal for syntactically invalid widget JSON is "MetricWidget property 'metricWidget' has a bad JSON content", and a well-formed widget still renders non-empty image bytes (the check's positive control).
Emulator: the code is correct ("ValidationError") but the message is "The parameter MetricWidget.metrics is required."
Why: a client matching AWS's JSON-schema diagnostic for a malformed widget specification does not recognise the refusal, and the emulator's wording names a request parameter path AWS does not use for widget-body validation.
Fix: GetMetricWidgetImage's widget-body validation in the operation's handler — the missing-required-property diagnostic must be rendered as "Should have required property '<property>'".
Traces: cloudwatch.get-metric-widget-image-malformed-spec-rejected.

## Evidence
Representative expected-vs-actual from the replay:

1) cloudwatch.associate-dataset-kms-key-nonexistent-key-rejected — AssertionError: expected AccessDenied for a nonexistent KMS key, got KmsKeyNotFoundException ('The specified Amazon Web Services Key Management Service (Amazon Web Services KMS) key could not be found. Verify that the key Amazon Resource Name (ARN) is correct, that the key exists, and that it is in the same Amazon Web Services Region as the resource.'). Required: code 'AccessDenied', message "Unable to validate KMS key arn:aws:kms:<region>:<account>:key/00000000-0000-0000-0000-000000000000. Verify the key exists and the key policy grants kms:DescribeKey to the CloudWatch service principal."

2) cloudwatch.associate-dataset-kms-key-cross-account-key-denied — AssertionError: expected ValidationError, got 'KmsKeyNotFoundException' ('The specified Amazon Web Services Key Management Service (Amazon Web Services KMS) key could not be found. …'). Required: code 'ValidationError', message 'KMS key account <key-account> does not match caller account <caller-account>'. Handler evidence: services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/associate_dataset_kms_key.rs:223-226 compares only the ARN's Region segment, then falls through to key_not_found() (lines 311-329); no account comparison exists.

3) cloudwatch.associate-dataset-kms-key-missing-permission-denied, cloudwatch.associate-dataset-kms-key-reassociation-conflict-rejected, cloudwatch.duplicate-kms-association-rejected, cloudwatch.kms-key-access-errors — AssertionError: unexpected AccessDenied message 'The operation was denied because either the calling principal lacks the required Amazon Web Services Key Management Service (Amazon Web Services KMS) permission on the key, or the key policy does not grant Amazon CloudWatch the permissions it needs to use the key. Verify that the caller has kms:Decrypt permission on the key, and that the key policy grants the CloudWatch service principal the kms:DescribeKey, kms:GenerateDataKey, kms:Encrypt, kms:Decrypt, and kms:ReEncrypt* permissions described in AssociateDatasetKmsKey.' Required: 'Unable to validate KMS key <bad-key-arn>. Verify the key exists and the key policy grants kms:DescribeKey to the CloudWatch service principal.'

4) cloudwatch.associate-dataset-kms-key-disabled-key-rejected and cloudwatch.dataset-kms-key-disabled-key-rejected — AssertionError: unexpected AccessDenied message 'The specified Amazon Web Services Key Management Service (Amazon Web Services KMS) key is disabled or pending deletion. Re-enable the key (or restore it, if it is pending deletion) and retry the operation.' Required: 'KMS key <key-arn> is disabled.'

5) cloudwatch.get-metric-widget-image-malformed-spec-rejected — AssertionError: unexpected message for a widget spec with no metrics: 'The parameter MetricWidget.metrics is required.' Required: "Should have required property 'metrics'" (code ValidationError was correct; the malformed-JSON sibling message and the well-formed render both passed).

The handler's own doc comment (associate_dataset_kms_key.rs:236-243) states the intent that usability failures collapse to AccessDenied while an unresolvable identifier keeps the modelled not-found fault — the recordings show AWS collapses BOTH to AccessDenied with the "Unable to validate KMS key …" sentence, and reserves ValidationError for an account mismatch.

## Steps to Reproduce
Boot the emulator and replay the behavioral suite restricted to the trace_ids on this issue.

Cause 1: sts:GetCallerIdentity for the account id, then cloudwatch:AssociateDatasetKmsKey(DatasetIdentifier='default', KmsKeyArn='arn:aws:kms:<region>:<own-account>:key/00000000-0000-0000-0000-000000000000'). Compare Error.Code and Error.Message.

Cause 2: cloudwatch:AssociateDatasetKmsKey(DatasetIdentifier='default', KmsKeyArn='arn:aws:kms:<region>:111111111111:key/<uuid>') from a caller in a different account. Compare Error.Code (must be ValidationError) and Error.Message.

Cause 3: kms:CreateKey with its default (root-only) key policy, then AssociateDatasetKmsKey against that key ARN; repeat with a working association already in place. Compare Error.Message. Then kms:PutKeyPolicy granting cloudwatch.amazonaws.com kms:DescribeKey/GenerateDataKey/Encrypt/Decrypt/ReEncrypt* and confirm the identical call succeeds.

Cause 4: kms:CreateKey with a policy granting CloudWatch the documented permissions, kms:DisableKey, poll DescribeKey until KeyState=Disabled, then AssociateDatasetKmsKey. Compare Error.Message, confirm GetDataset's KmsKeyArn is unchanged, then kms:EnableKey and confirm the identical call succeeds.

Cause 5: cloudwatch:GetMetricWidgetImage(MetricWidget='{}') and compare Error.Message; GetMetricWidgetImage(MetricWidget='{"metrics":[["AWS/EC2","CPUUtilization"]],"width":400,"height":300}') is the positive control, and MetricWidget='{this is not valid json' is the sibling refusal.
