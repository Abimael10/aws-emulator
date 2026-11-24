Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Step: task#226
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/a0ufrfbomg90qbskltuop262u"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/a0ufrfbomg90qbskltuop262u"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4v7xtm83uvvyulk1wwpm4qm3s"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4v7xtm83uvvyulk1wwpm4qm3s"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8qmzz7l2841u6bvwpkiug38ta"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8qmzz7l2841u6bvwpkiug38ta"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/aeellyghb27pbehyzua1nyena"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/aeellyghb27pbehyzua1nyena"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/b2gzofz99eb7nsuj5g8wcimse"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/b2gzofz99eb7nsuj5g8wcimse"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1fvktjhpo9wdpt3pjb2wsz7nt"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1fvktjhpo9wdpt3pjb2wsz7nt"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/70u9gks728rcr33237rckvzbo"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/70u9gks728rcr33237rckvzbo"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4jc77cq1lcr7g64xywwypykv8"].CreateTime
Issue-Commit: 3cf14233d7b3c1398fbfecd8519fd2fa7d531f41

# controlcatalog ListControls rewrites published Config-rule control text and mints CreateTime values (8 causes)

> _Note: this issue was identified against the code base at commit `3cf14233d7b3c1398fbfecd8519fd2fa7d531f41`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
ListControls answers HTTP 200 for the AWS Config (`AWS::Config::ConfigRule`) implementation filter, but the ControlSummary rows it projects are the emulator's own prose and its own release dates rather than the catalog's published values. Two members diverge systematically: `Name`/`Description` are re-worded (AWS's "Checks whether ..." catalog sentences are rewritten into "Checks if ..." form, qualifying clauses are added or dropped, and the Config-rule field names the published text cites are replaced by paraphrases), and `CreateTime` carries a date unrelated to the control's release. EIGHT net-new causes, all on service `controlcatalog`, all in fixture `controlcatalog-catalog-list-controls-aws-config-configrule`, each a distinct control ARN and a distinct step.

These clusters are reported un-owned by the replay. Related OPEN issues that do NOT cover them (different control ARNs / different steps, and none carries these replay keys): 20261123T051108Z-controlcatalog-serves-a-synthesized-control-catalog-getcontrol-404s.md, 20261122T055800Z-controlcatalog-listcontrols-serves-a-divergent-aws-config-configrule.md, 20261118T190218Z-controlcatalog-listcontrols-rewrites-the-published-description-of-the.md.

Shared model citation for every cause below — Smithy: ListControls POST /list-controls, http code 200, output `Controls` @required, paginated (NextToken/MaxResults, items Controls); ControlSummary.Arn @required @pattern ^arn:(aws(?:[-a-z]*)?):(controlcatalog|controltower):[a-zA-Z0-9-]*::control/[0-9a-zA-Z_\-]+$; ControlSummary.Name @required, doc "The display name of the control"; ControlSummary.Description @required, doc "A description of the control, as it may appear in the console. Describes the functionality of the control."; ControlSummary.CreateTime target Timestamp, doc "A timestamp that notes the time when the control was released (start of its life) as a governance capability in Amazon Web Services." The member docs make Name/Description the catalog's published strings and CreateTime the control's actual release instant, so neither is a value the emulator may author; the published values themselves are recorded behavior only (the model does not enumerate catalog content).

Cause 1 of 8: control/a0ufrfbomg90qbskltuop262u (DynamoDB KMS encryption) Name and Description re-worded — fixture controlcatalog-catalog-list-controls-aws-config-configrule, step task#226.
AWS: recorded behavior only — Description is "Checks whether Amazon DynamoDB table is encrypted with AWS Key Management Service (KMS). The rule is NON_COMPLIANT if Amazon DynamoDB table is not encrypted with AWS KMS." and Name is that same sentence without the trailing period: "Checks whether Amazon DynamoDB table is encrypted with AWS Key Management Service (KMS)". Model: both members @required with the docs quoted above.
Emulator: answers 200 with Description "Checks if the Amazon DynamoDB table is encrypted with AWS Key Management Service (KMS). The rule is NON_COMPLIANT if the Amazon DynamoDB table is not encrypted with AWS KMS. The rule is also NON_COMPLIANT if the encrypted AWS KMS key is not present in kmsKeyArns input parameter." and Name "Checks if the Amazon DynamoDB table is encrypted with AWS Key Management Service (KMS)" — the leading clause is rewritten and a third NON_COMPLIANT condition the published text does not state is appended.
Why: a client that renders the catalog (console view, compliance export, control-mapping document) shows control text that is not AWS's, and the added clause asserts a compliance condition on `kmsKeyArns` that the published control does not evaluate — so generated evidence misstates what the control checks.
Fix: the AWS Config control rows in the control-catalog state that ListControls' response projection reads; Name/Description must be stored verbatim as published, not authored.
Traces: controlcatalog-catalog-list-controls-aws-config-configrule#task#226.

Cause 2 of 8: control/4v7xtm83uvvyulk1wwpm4qm3s (EC2 public IPv4) CreateTime and Description diverge — step task#250.
AWS: recorded behavior only — CreateTime 1638230400.0; Description "Detects whether an Amazon Elastic Compute Cloud (Amazon EC2) instance has an associated public IPv4 address. The rule is NON_COMPLIANT if the public IP field is present in the Amazon EC2 instance configuration item. This guardrail applies only to IPv4 addresses."
Emulator: CreateTime 1669939200 (a different release date entirely) and Description "Detects whether any Amazon Elastic Compute Cloud (Amazon EC2) instance has an associated public IPv4 address. The rule is NON_COMPLIANT if the publicIp field is present in the Amazon EC2 instance configuration item. This rule applies only to IPv4 addresses." — scope word changed, the configuration-item field renamed, and "guardrail" replaced by "rule".
Why: CreateTime is the control's release instant, so a client that orders or filters the catalog by release date, or reports when a governance capability became available, gets a wrong answer; the re-worded body again misstates the evaluated field.
Fix: the same AWS Config control rows in the control-catalog state, plus the CreateTime these rows are seeded with — the release timestamp must come from the catalog, not from an emulator default.
Traces: controlcatalog-catalog-list-controls-aws-config-configrule#task#250.

Cause 3 of 8: control/8qmzz7l2841u6bvwpkiug38ta (EFS encryption) Name and Description re-worded — step task#312.
AWS: recorded behavior only — Description "Checks whether Amazon EFS are configured to encrypt file data using AWS KMS. The rule is NON_COMPLIANT if the Encrypted key is set to False on DescribeFileSystems or, if specified, KmsKeyId key on DescribeFileSystems is not matching KmsKeyId parameter.", Name "Checks whether Amazon EFS are configured to encrypt file data using AWS KMS".
Emulator: Description "Checks if Amazon Elastic File System (Amazon EFS) file systems are encrypted. The rule is NON_COMPLIANT if the file system is not encrypted, or if it is encrypted with a key other than the one the rule's input parameter names.", Name "Checks if Amazon Elastic File System (Amazon EFS) file systems are encrypted" — the published text names the evaluated API (DescribeFileSystems) and the exact keys (Encrypted, KmsKeyId); the emulator's drops all of them.
Why: the published description is what tells an auditor which API response fields the control inspects; a paraphrase that removes them cannot be reconciled against the control's actual evaluation.
Fix: same component — the control-catalog rows behind ListControls' projection.
Traces: controlcatalog-catalog-list-controls-aws-config-configrule#task#312.

Cause 4 of 8: control/aeellyghb27pbehyzua1nyena (EKS endpoint public access) CreateTime wrong and Description inverted — step task#325.
AWS: recorded behavior only — CreateTime 1638316800.0; Description "Detects whether an Amazon Elastic Kubernetes Service (Amazon EKS) endpoint is publicly accessible. The rule is NON_COMPLIANT if the endpoint is publicly accessible."
Emulator: CreateTime 1579392000 and Description "Detects whether an Amazon Elastic Kubernetes Service (Amazon EKS) endpoint is blocked from public access. The rule is NON_COMPLIANT if the Amazon EKS endpoint is publicly accessible." — the first sentence states the opposite predicate ("blocked from public access") from the NON_COMPLIANT condition in its own second sentence, so the emulator's own text is self-contradictory where AWS's is not.
Why: a reader of the catalog is told the control detects the inverse condition from the one it fails on, and the release date is roughly two years off.
Fix: same component — control-catalog rows and their seeded CreateTime.
Traces: controlcatalog-catalog-list-controls-aws-config-configrule#task#325.

Cause 5 of 8: control/b2gzofz99eb7nsuj5g8wcimse (Lambda function policy public access) CreateTime wrong and Description re-worded — step task#469.
AWS: recorded behavior only — CreateTime 1638230400.0; Description "Detects whether the AWS Lambda function policy attached to the Lambda resource prohibits public access. The rule is NON_COMPLIANT if the Lambda function policy allows public access."
Emulator: CreateTime 1669420800; Description "Detects whether the AWS Lambda function policy attached to the Lambda resource blocks public access. The rule is NON_COMPLIANT if the AWS Lambda function policy allows public access." ("prohibits" rewritten to "blocks", subject expanded).
Why: catalog text and release date both differ from the published catalog, so any artifact generated from the emulator's catalog fails a byte comparison against AWS's.
Fix: same component — control-catalog rows and their seeded CreateTime.
Traces: controlcatalog-catalog-list-controls-aws-config-configrule#task#469.

Cause 6 of 8: control/1fvktjhpo9wdpt3pjb2wsz7nt (IAM user console MFA) CreateTime wrong and Description re-worded — step task#484.
AWS: recorded behavior only — CreateTime 1564444800.0; Description "Protect your account by requiring multi-factor authentication (MFA) for any AWS Identity and Access Management (IAM) user using the console. MFA adds an extra authentication code on top of a user name and password."
Emulator: CreateTime 1598313600; Description "...for any AWS Identity and Access Management (IAM) user with console access. MFA adds an extra authentication code on top of a user name and password." — the qualifying clause is restated ("using the console" -> "with console access"), which also widens the stated population from users who use the console to users who merely have access.
Why: the clause change alters which principals the control is documented to cover, and the release date is a year out.
Fix: same component — control-catalog rows and their seeded CreateTime.
Traces: controlcatalog-catalog-list-controls-aws-config-configrule#task#484.

Cause 7 of 8: control/70u9gks728rcr33237rckvzbo (OpenSearch encryption at rest) Name and Description re-worded — step task#528.
AWS: recorded behavior only — Description "Checks if Amazon OpenSearch Service domains have encryption at rest configuration enabled. The rule is NON_COMPLIANT if EncryptionAtRestOptions field is not enabled.", Name "Checks if Amazon OpenSearch Service domains have encryption at rest configuration enabled".
Emulator: Description "Checks if Amazon OpenSearch Service domains have encryption at rest enabled. The rule is NON_COMPLIANT if the domain does not have encryption at rest enabled.", Name "Checks if Amazon OpenSearch Service domains have encryption at rest enabled" — the evaluated member name `EncryptionAtRestOptions` is replaced by prose.
Why: as in cause 3, the field the control evaluates is erased from the published text, and Name no longer matches the catalog string a client may key display or dedup on.
Fix: same component — control-catalog rows behind ListControls' projection.
Traces: controlcatalog-catalog-list-controls-aws-config-configrule#task#528.

Cause 8 of 8: control/4jc77cq1lcr7g64xywwypykv8 CreateTime is an emulator-chosen date — step task#552.
AWS: recorded behavior only — CreateTime 1564444800.0 (the control's release instant, per the member doc).
Emulator: CreateTime 1615248000. This is the only diverging member in the row, which isolates the CreateTime seeding defect from the text defect: the row's text matches, so the wrong timestamp is not collateral of a re-worded row but its own fault in how catalog rows are dated.
Why: the catalog's release dates are not derivable from anything else, so a client sorting or windowing controls by CreateTime gets an order AWS does not report. (This is a stored catalog value, not a per-run clock reading — the emulator returns a fixed wrong date, not "now".)
Fix: the CreateTime seeded onto control-catalog rows in the control-catalog state.
Traces: controlcatalog-catalog-list-controls-aws-config-configrule#task#552.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, bootstrap_signal false, 10 fixtures replayed, 1408 steps (61 passed, 1347 mismatched, 0 errored, 0 blocked), score 4.3%, 46 divergence clusters. Eight of the net-new clusters are the ones below — all service=controlcatalog, operation=ListControls, verdict=mismatch, expected_status 200 / actual_status 200, stub_path empty, owned_by empty, affected_count 1 each.

1. controlcatalog-catalog-list-controls-aws-config-configrule#task#226 — $.Controls[Arn="arn:aws:controlcatalog:::control/a0ufrfbomg90qbskltuop262u"].Description: value_mismatch (expected 'Checks whether Amazon DynamoDB table is encrypted with AWS Key Management Service (KMS). The rule is NON_COMPLIANT if Amazon DynamoDB table is not encrypted with AWS KMS.', got 'Checks if the Amazon DynamoDB table is encrypted with AWS Key Management Service (KMS). The rule is NON_COMPLIANT if the Amazon DynamoDB table is not encrypted with AWS KMS. The rule is also NON_COMPLIANT if the encrypted AWS KMS key is not present in kmsKeyArns input parameter.'); .Name: value_mismatch (expected 'Checks whether Amazon DynamoDB table is encrypted with AWS Key Management Service (KMS)', got 'Checks if the Amazon DynamoDB table is encrypted with AWS Key Management Service (KMS)').

2. controlcatalog-catalog-list-controls-aws-config-configrule#task#250 — $.Controls[Arn=".../control/4v7xtm83uvvyulk1wwpm4qm3s"].CreateTime: value_mismatch (expected 1638230400.0, got 1669939200); .Description: value_mismatch (expected 'Detects whether an Amazon Elastic Compute Cloud (Amazon EC2) instance has an associated public IPv4 address. The rule is NON_COMPLIANT if the public IP field is present in the Amazon EC2 instance configuration item. This guardrail applies only to IPv4 addresses.', got 'Detects whether any Amazon Elastic Compute Cloud (Amazon EC2) instance has an associated public IPv4 address. The rule is NON_COMPLIANT if the publicIp field is present in the Amazon EC2 instance configuration item. This rule applies only to IPv4 addresses.').

3. controlcatalog-catalog-list-controls-aws-config-configrule#task#312 — $.Controls[Arn=".../control/8qmzz7l2841u6bvwpkiug38ta"].Description: value_mismatch (expected 'Checks whether Amazon EFS are configured to encrypt file data using AWS KMS. The rule is NON_COMPLIANT if the Encrypted key is set to False on DescribeFileSystems or, if specified, KmsKeyId key on DescribeFileSystems is not matching KmsKeyId parameter.', got "Checks if Amazon Elastic File System (Amazon EFS) file systems are encrypted. The rule is NON_COMPLIANT if the file system is not encrypted, or if it is encrypted with a key other than the one the rule's input parameter names."); .Name: value_mismatch (expected 'Checks whether Amazon EFS are configured to encrypt file data using AWS KMS', got 'Checks if Amazon Elastic File System (Amazon EFS) file systems are encrypted').

4. controlcatalog-catalog-list-controls-aws-config-configrule#task#325 — $.Controls[Arn=".../control/aeellyghb27pbehyzua1nyena"].CreateTime: value_mismatch (expected 1638316800.0, got 1579392000); .Description: value_mismatch (expected 'Detects whether an Amazon Elastic Kubernetes Service (Amazon EKS) endpoint is publicly accessible. The rule is NON_COMPLIANT if the endpoint is publicly accessible.', got 'Detects whether an Amazon Elastic Kubernetes Service (Amazon EKS) endpoint is blocked from public access. The rule is NON_COMPLIANT if the Amazon EKS endpoint is publicly accessible.').

5. controlcatalog-catalog-list-controls-aws-config-configrule#task#469 — $.Controls[Arn=".../control/b2gzofz99eb7nsuj5g8wcimse"].CreateTime: value_mismatch (expected 1638230400.0, got 1669420800); .Description: value_mismatch (expected 'Detects whether the AWS Lambda function policy attached to the Lambda resource prohibits public access. The rule is NON_COMPLIANT if the Lambda function policy allows public access.', got 'Detects whether the AWS Lambda function policy attached to the Lambda resource blocks public access. The rule is NON_COMPLIANT if the AWS Lambda function policy allows public access.').

6. controlcatalog-catalog-list-controls-aws-config-configrule#task#484 — $.Controls[Arn=".../control/1fvktjhpo9wdpt3pjb2wsz7nt"].CreateTime: value_mismatch (expected 1564444800.0, got 1598313600); .Description: value_mismatch (expected 'Protect your account by requiring multi-factor authentication (MFA) for any AWS Identity and Access Management (IAM) user using the console. MFA adds an extra authentication code on top of a user name and password.', got 'Protect your account by requiring multi-factor authentication (MFA) for any AWS Identity and Access Management (IAM) user with console access. MFA adds an extra authentication code on top of a user name and password.').

7. controlcatalog-catalog-list-controls-aws-config-configrule#task#528 — $.Controls[Arn=".../control/70u9gks728rcr33237rckvzbo"].Description: value_mismatch (expected 'Checks if Amazon OpenSearch Service domains have encryption at rest configuration enabled. The rule is NON_COMPLIANT if EncryptionAtRestOptions field is not enabled.', got 'Checks if Amazon OpenSearch Service domains have encryption at rest enabled. The rule is NON_COMPLIANT if the domain does not have encryption at rest enabled.'); .Name: value_mismatch (expected 'Checks if Amazon OpenSearch Service domains have encryption at rest configuration enabled', got 'Checks if Amazon OpenSearch Service domains have encryption at rest enabled').

8. controlcatalog-catalog-list-controls-aws-config-configrule#task#552 — $.Controls[Arn=".../control/4jc77cq1lcr7g64xywwypykv8"].CreateTime: value_mismatch (expected 1564444800.0, got 1615248000) — sole diverging member in the row.

Model citations used: Smithy ListControls (POST /list-controls, http 200, Controls @required, paginated on NextToken/MaxResults with items=Controls, errors AccessDeniedException 403 / ValidationException 400 / ThrottlingException 429 / InternalServerException 500); ControlSummary.Name @required ("The display name of the control"); ControlSummary.Description @required ("A description of the control, as it may appear in the console. Describes the functionality of the control."); ControlSummary.CreateTime -> Timestamp ("A timestamp that notes the time when the control was released (start of its life) as a governance capability in Amazon Web Services."); ControlSummary.Arn @required, length 34..2048, pattern ^arn:(aws(?:[-a-z]*)?):(controlcatalog|controltower):[a-zA-Z0-9-]*::control/[0-9a-zA-Z_\-]+$.

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it, then replay trace set simparity-wire-aws-v1 (replay_wire_corpus).
2. Cause 1: replay fixture controlcatalog-catalog-list-controls-aws-config-configrule step task#226 — POST /list-controls with the AWS::Config::ConfigRule implementation filter — and diff $.Controls[Arn="arn:aws:controlcatalog:::control/a0ufrfbomg90qbskltuop262u"].Name and .Description against the recorded page.
3. Cause 2: replay the same fixture step task#250 and diff .CreateTime and .Description of control/4v7xtm83uvvyulk1wwpm4qm3s.
4. Cause 3: replay step task#312 and diff .Name and .Description of control/8qmzz7l2841u6bvwpkiug38ta.
5. Cause 4: replay step task#325 and diff .CreateTime and .Description of control/aeellyghb27pbehyzua1nyena.
6. Cause 5: replay step task#469 and diff .CreateTime and .Description of control/b2gzofz99eb7nsuj5g8wcimse.
7. Cause 6: replay step task#484 and diff .CreateTime and .Description of control/1fvktjhpo9wdpt3pjb2wsz7nt.
8. Cause 7: replay step task#528 and diff .Name and .Description of control/70u9gks728rcr33237rckvzbo.
9. Cause 8: replay step task#552 and diff .CreateTime of control/4jc77cq1lcr7g64xywwypykv8.
All EIGHT causes must diff clean before this issue is closed — they are eight distinct catalog rows across eight distinct steps, not one.
