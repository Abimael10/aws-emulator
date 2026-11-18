Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol
Issue-Step: task#106
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9r3or4f2x3aq1jrnma6ddfp2y"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/dowsaumj3n874siszx3j94m9n"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/5cksj6jdycadjupdbviikn1w7"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/a7ertnvgba7449o25l6s75nrj"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/esv514s4zvnxdunuijbgerpn3"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/bbns1svmi60sz11i658ot4amf"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/dsrtfqf4g9gvlguj5hqahozla"].Description
Issue-Commit: 83b924bfca64d21d2064b8fa56823232662207f8

# controlcatalog ListControls rewrites published Security Hub control Description text (7 catalog records)

> _Note: this issue was identified against the code base at commit `83b924bfca64d21d2064b8fa56823232662207f8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
ListControls answers 200 and the control records it serves carry the right Arn/Name/identity, but ControlSummary$Description is an emulator-authored paraphrase of the catalog's published description rather than the published text itself. Seven distinct Security Hub (AWS::SecurityHub::SecurityControl) catalog records diverge this way in one replayed listing; all seven share one root cause (the seeded catalog's Description strings are re-written prose), so they are bundled here as 7 causes.

Shared statement of the fault:
AWS: Smithy (controlcatalog): ListControls -> ListControlsResponse$Controls : list<ControlSummary>; ControlSummary$Description is `required` and documented as "A description of the control, as it may appear in the console. Describes the functionality of the control." The description is catalog content owned by AWS — the exact published sentence for that control ARN, byte for byte — not a field a caller supplies or a server may re-phrase. The wire shape and status (200) are correct; only the served text diverges. Recorded behavior supplies the published sentence for each ARN below.
Emulator: for each of the seven control ARNs the emulator serves a differently worded sentence with the same meaning (re-ordered clauses, service names expanded or contracted, extra or missing trailing conditions, different casing of property names).
Why: clients treat the catalog description as published AWS content. Console/portal renderings, compliance-evidence exports, and documentation-generation or diffing tools that pin a control's published description, and tests that assert on it, all see text AWS never published; an auditor comparing emulator output to the AWS catalog sees an unexplained content drift on every one of these controls. Because Description is also the human-facing identity of a control in reports, a paraphrase that drops a qualifying clause (e.g. the "does not check KMS keys with imported key material" / "only checks customer managed policies" caveats) misstates what the control actually evaluates.
Fix: the controlcatalog crate's seeded catalog data behind the ListControls handler — the stored ControlSummary records for AWS::SecurityHub::SecurityControl implementations must hold the published description strings verbatim, so the handler and serializer pass them through unchanged. This is a state/data-fidelity defect, not a serialization defect.

Cause 1 of 7 — control arn:aws:controlcatalog:::control/9r3or4f2x3aq1jrnma6ddfp2y (AWS Config recording).
AWS (recorded): "This control checks whether AWS Config is enabled in your account in the current AWS Region, records all resources that correspond to controls that are enabled in the current Region, and uses the service-linked AWS Config role."
Emulator: "This AWS control checks whether AWS Config is enabled in your AWS account and region and records resource configurations with the AWS Config service-linked role."
Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol#task#106.

Cause 2 of 7 — control arn:aws:controlcatalog:::control/dowsaumj3n874siszx3j94m9n (VPC default security group).
AWS (recorded): "This AWS control checks that the default security group of a VPC does not allow inbound or outbound traffic."
Emulator: "This AWS control checks whether the default security group of a VPC allows inbound or outbound traffic." (the paraphrase also inverts the sense of the check from "does not allow" to "allows").
Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol#task#163.

Cause 3 of 7 — control arn:aws:controlcatalog:::control/5cksj6jdycadjupdbviikn1w7 (EFS encryption).
AWS (recorded): "This AWS control checks whether Amazon Elastic File System (Amazon EFS) is configured to encrypt the file data using AWS Key Management Service (AWS KMS). The check will fail if the encrypted key is set to false on DescribeFileSystems or if the KmsKeyId key on DescribeFileSystems does not match the KmsKeyId parameter."
Emulator: "This AWS control checks whether Amazon Elastic File System is configured to encrypt file data using AWS KMS. The check fails if the encrypted key is set to false on DescribeFileSystems or if the KmsKeyId key on DescribeFileSystems doesn't match the KmsKeyId parameter for efs-encrypted-check." (names the backing Config rule, which the published text does not).
Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol#task#226.

Cause 4 of 7 — control arn:aws:controlcatalog:::control/a7ertnvgba7449o25l6s75nrj (IAM customer managed policies with admin access).
AWS (recorded): "This AWS control checks whether the default version of AWS Identity and Access Management (IAM) policies (also known as customer managed policies) do not have administrator access with a statement that has \"Effect\": \"Allow\" with \"Action\": \"*\" over \"Resource\": \"*\". It only checks for the Customer Managed Policies that you created, but not inline and AWS Managed Policies."
Emulator: same opening sentence without the "(IAM)" expansion, and the final sentence replaced with "... but does not check for full access to individual services, such as \"S3:*\"." — a different caveat from the published one (which is about inline and AWS managed policies).
Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol#task#310.

Cause 5 of 7 — control arn:aws:controlcatalog:::control/esv514s4zvnxdunuijbgerpn3 (KMS key rotation).
AWS (recorded): "This AWS control checks that key rotation is enabled for each AWS KMS key. It does not check KMS keys that have imported key material."
Emulator: "This AWS control checks whether automatic key rotation is enabled for each customer managed AWS KMS key. The control fails if automatic key rotation of the KMS key isn't enabled. This control doesn't check KMS keys with imported key material."
Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol#task#365.

Cause 6 of 7 — control arn:aws:controlcatalog:::control/bbns1svmi60sz11i658ot4amf (RDS instances publicly accessible).
AWS (recorded): "This AWS control checks whether RDS instances are publicly accessible by evaluating the publiclyAccessible field in the instance configuration item."
Emulator: "This AWS control checks whether Amazon RDS instances are publicly accessible by evaluating the PubliclyAccessible field in the instance configuration item." (service name expanded and the configuration-item field name re-cased).
Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol#task#431.

Cause 7 of 7 — control arn:aws:controlcatalog:::control/dsrtfqf4g9gvlguj5hqahozla (RDS storage encryption).
AWS (recorded): "This AWS control checks whether storage encryption is enabled for your RDS DB instances."
Emulator: "This AWS control checks whether storage encryption is enabled for your Amazon RDS DB instances."
Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol#task#442.

Relation to open issues: the open issues 20261115T014344Z-controlcatalog-listcontrols-pages-omit-catalog-config-controls-and-rewrite.md and 20261115T014344Z-controlcatalog-security-hub-s3-control-records-drop-governedresources-and.md describe Description rewrites for the AWS Config rule controls and for the Security Hub S3 records respectively; the replay reports no owner for the seven records above, so they are net-new and are filed here. A fixer should expect the published-text correction to be applied to the whole seeded Security Hub catalogue, not just these seven rows.

## Evidence
Replay of fixture controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol (trace set simparity-wire-aws-v1): expected status 200, actual status 200, verdict mismatch on seven steps, each a single value_mismatch on $.Controls[Arn="..."].Description.

step task#106, control 9r3or4f2x3aq1jrnma6ddfp2y:
  expected: 'This control checks whether AWS Config is enabled in your account in the current AWS Region, records all resources that correspond to controls that are enabled in the current Region, and uses the service-linked AWS Config role.'
  actual:   'This AWS control checks whether AWS Config is enabled in your AWS account and region and records resource configurations with the AWS Config service-linked role.'

step task#163, control dowsaumj3n874siszx3j94m9n:
  expected: 'This AWS control checks that the default security group of a VPC does not allow inbound or outbound traffic.'
  actual:   'This AWS control checks whether the default security group of a VPC allows inbound or outbound traffic.'

step task#226, control 5cksj6jdycadjupdbviikn1w7:
  expected: 'This AWS control checks whether Amazon Elastic File System (Amazon EFS) is configured to encrypt the file data using AWS Key Management Service (AWS KMS). The check will fail if the encrypted key is set to false on DescribeFileSystems or if the KmsKeyId key on DescribeFileSystems does not match the KmsKeyId parameter.'
  actual:   "This AWS control checks whether Amazon Elastic File System is configured to encrypt file data using AWS KMS. The check fails if the encrypted key is set to false on DescribeFileSystems or if the KmsKeyId key on DescribeFileSystems doesn't match the KmsKeyId parameter for efs-encrypted-check."

step task#310, control a7ertnvgba7449o25l6s75nrj:
  expected: '...AWS Identity and Access Management (IAM) policies ... It only checks for the Customer Managed Policies that you created, but not inline and AWS Managed Policies.'
  actual:   '...AWS Identity and Access Management policies ... It only checks for the customer managed policies that you created, but does not check for full access to individual services, such as "S3:*".'

step task#365, control esv514s4zvnxdunuijbgerpn3:
  expected: 'This AWS control checks that key rotation is enabled for each AWS KMS key. It does not check KMS keys that have imported key material.'
  actual:   "This AWS control checks whether automatic key rotation is enabled for each customer managed AWS KMS key. The control fails if automatic key rotation of the KMS key isn't enabled. This control doesn't check KMS keys with imported key material."

step task#431, control bbns1svmi60sz11i658ot4amf:
  expected: 'This AWS control checks whether RDS instances are publicly accessible by evaluating the publiclyAccessible field in the instance configuration item.'
  actual:   'This AWS control checks whether Amazon RDS instances are publicly accessible by evaluating the PubliclyAccessible field in the instance configuration item.'

step task#442, control dsrtfqf4g9gvlguj5hqahozla:
  expected: 'This AWS control checks whether storage encryption is enabled for your RDS DB instances.'
  actual:   'This AWS control checks whether storage encryption is enabled for your Amazon RDS DB instances.'

Model citation: Smithy controlcatalog ControlSummary$Description — required member, target String, documented as the control's console description; ListControls POST /list-controls, 200, paginated over Controls.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay wire fixture controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol from trace set simparity-wire-aws-v1.
2. POST /list-controls with the fixture's Filter selecting AWS::SecurityHub::SecurityControl implementations and page to the step under test (task#106, then task#163, task#226, task#310, task#365, task#431, task#442).
3. In each 200 response locate the ControlSummary whose Arn is the one named for that step and compare its Description with the recorded response's Description for the same Arn.
4. Each comparison differs: the status, Arn, Name and remaining members match, only Description is re-worded by the emulator. Re-replay all seven steps to confirm a fix.
