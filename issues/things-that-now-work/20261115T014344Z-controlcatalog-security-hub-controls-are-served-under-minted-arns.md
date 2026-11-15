Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol
Issue-Step: task#70
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/3b0a9d6550360b808fc7812ff"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/624yg0j9d8swiglwfc1m4kvnm"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/578ae099bc64665af00238723"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9r3or4f2x3aq1jrnma6ddfp2y"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/730c27ce58a5899cfb4db3698"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/dowsaumj3n874siszx3j94m9n"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/58a14d983e8acb84121f29d72"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/5cksj6jdycadjupdbviikn1w7"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/62smpoz33dsy0oa7u1iwa58lz"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/e150d5128aedec3b1127011ca"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/6b6dbeb85f8a005d07f3d8c11"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/a7ertnvgba7449o25l6s75nrj"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/951c1bf7a40814be706e81983"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/bxh5ltpzbcr59zexx3llhg3l9"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/aa44c0d8db872bf143e815e51"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/esv514s4zvnxdunuijbgerpn3"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/561595b80b643a6c71c4d24aa"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/bbns1svmi60sz11i658ot4amf"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/a19b7ded8d8bacf2f99d5f287"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/dsrtfqf4g9gvlguj5hqahozla"]
Issue-Commit: ab88f803a4e379013b8a9f7f3c950c61c3772e52

# Security Hub controls are served under minted ARNs with wrong CreateTime

> _Note: this issue was identified against the code base at commit `ab88f803a4e379013b8a9f7f3c950c61c3772e52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
On the AWS::SecurityHub::SecurityControl page of the catalog, ListControls answers 200 but serves emulator-authored summaries in place of the catalog's: the control ARN is emulator-minted, CreateTime is synthesized, the Description is rewritten into the emulator's own 'and reports the finding in Security Hub' phrasing, and in some entries Severity, Aliases or GovernedResources differ from what the catalog publishes. The replay reports the catalog's summary missing and the emulator's extra under the same SecurityHub control identifier, which is the signature of a fabricated identity rather than a filter fault. TEN divergence clusters are bundled here, numbered as causes below; ALL TEN must be closed and re-replayed before this issue is done.

Shared AWS basis (cited once; Smithy model services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json): ListControls is POST /list-controls, @readonly, 200, paginated (NextToken/MaxResults @range 1..100, items Controls); Controls is @required. ControlSummary.Arn is @required (@pattern ^arn:(aws(?:[-a-z]*)?):(controlcatalog|controltower):[a-zA-Z0-9-]*::control/[0-9a-zA-Z_\-]+$), Name and Description are @required, Aliases carries the control's alternative identifiers (such as SH.S3.1), Severity and ParameterRequirementSummary are enums, Implementation is {Type, Identifier} and CreateTime is when the control was released as a governance capability. The catalog is an AWS-published, account-independent read-only dataset; the per-control values below are recorded behavior.

Shared Emulator basis: status matches (200); in every cause the catalog's control ARN is absent from the page and an emulator-minted ARN appears for the same Security Hub control identifier with a different CreateTime and the emulator's own Description.

Shared Why: a client harvesting control ARNs from the emulator gets ids that exist in no AWS account and that the real service rejects (and the emulator's own GetControl resolves only its minted ids). The Security Hub alias is how a client joins a catalog control to a Security Hub security control, so a changed alias form breaks that join. A rewritten Description, a wrong CreateTime, a downgraded Severity and an altered GovernedResources list each make the catalog's content unusable as a reference dataset.

Shared Fix: the ControlCatalog control seed dataset read by crate::state and the ControlSummary projection in the ListControls handler (services/service-com-amazonaws-controlcatalog-controlcatalog/src/ops/list_controls.rs) — Arn, Aliases, Name, Description, Severity, ParameterRequirementSummary, CreateTime and GovernedResources must all come from the catalog record.

Cause 1 of 10 — alias SH.CloudTrail.4 / control CloudTrail.4 (step task#70). AWS: control/624yg0j9d8swiglwfc1m4kvnm, Description 'This AWS control checks whether CloudTrail log file validation is enabled.', CreateTime 1669593600. Emulator: absent; minted ARN with the emulator's own Description and CreateTime 1555891200.

Cause 2 of 10 — Config.1 (step task#106). AWS: control/9r3or4f2x3aq1jrnma6ddfp2y, Aliases ['SH.CONFIG.DT.1'], Severity CRITICAL, ParameterRequirementSummary OPTIONAL, CreateTime 1779840000. Emulator: absent; minted ARN whose alias is 'SH.Config.1' (a different alias string), Severity MEDIUM, ParameterRequirementSummary NONE and CreateTime 1572998400 — three classification fields plus the alias all diverge.

Cause 3 of 10 — alias SH.EC2.2 / control EC2.2 (step task#163). AWS: control/dowsaumj3n874siszx3j94m9n, Severity HIGH, CreateTime 1669593600. Emulator: absent; minted ARN with Severity MEDIUM and CreateTime 1664064000.

Cause 4 of 10 — alias SH.EFS.1 / control EFS.1 (step task#226). AWS: control/5cksj6jdycadjupdbviikn1w7, Name 'Elastic File System should be configured to encrypt file data at-rest using AWS KMS', Description naming the DescribeFileSystems encrypted/KmsKeyId evaluation, CreateTime 1669593600. Emulator: absent; minted ARN with an altered Name ('at rest'), a one-sentence Description and CreateTime 1598400000.

Cause 5 of 10 — alias SH.GuardDuty.1 / control GuardDuty.1 (step task#297). AWS: control/62smpoz33dsy0oa7u1iwa58lz, Description 'This AWS control checks whether Amazon GuardDuty is enabled in your AWS account and region.', CreateTime 1669593600. Emulator: absent; minted ARN with a rewritten Description and CreateTime 1700092800.

Cause 6 of 10 — alias SH.IAM.1 / control IAM.1 (step task#310). AWS: control/a7ertnvgba7449o25l6s75nrj, Description spelling out the "Effect": "Allow" / "Action": "*" / "Resource": "*" test and the customer-managed-policy scope, CreateTime 1669593600. Emulator: absent; minted ARN with a one-sentence paraphrase and CreateTime 1688860800.

Cause 7 of 10 — alias SH.IAM.3 / control IAM.3 (step task#330). AWS: control/bxh5ltpzbcr59zexx3llhg3l9, GovernedResources ['AWS::IAM::User'], CreateTime 1669593600. Emulator: absent; minted ARN with GovernedResources invented as ['AWS::IAM::AccessKey'] and CreateTime 1535328000, so a client filtering by governed resource type gets a different answer than AWS gives.

Cause 8 of 10 — alias SH.KMS.4 / control KMS.4 (step task#365). AWS: control/esv514s4zvnxdunuijbgerpn3, Description noting that keys with imported key material are not checked, CreateTime 1701043200. Emulator: absent; minted ARN with that qualification dropped and CreateTime 1565481600.

Cause 9 of 10 — alias SH.RDS.2 / control RDS.2 (step task#431). AWS: control/bbns1svmi60sz11i658ot4amf, Name 'RDS DB Instances should prohibit public access, as determined by the PubliclyAccessible configuration', CreateTime 1669593600. Emulator: absent; minted ARN with an altered Name/Description and CreateTime 1589414400.

Cause 10 of 10 — alias SH.RDS.3 / control RDS.3 (step task#442). AWS: control/dsrtfqf4g9gvlguj5hqahozla, Name 'RDS DB instances should have encryption at-rest enabled', CreateTime 1669593600. Emulator: absent; minted ARN with 'at rest' in the Name, a rewritten Description and CreateTime 1548806400.

Traces: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol steps task#70, task#106, task#163, task#226, task#297, task#310, task#330, task#365, task#431, task#442.

Related open issues which do NOT carry the replay key of any cause above (owned_by empty and stub_path empty for all ten clusters): 20261106T180948Z-controlcatalog-listcontrols-replaces-aws-config-catalog-controls-with-emulator.md (the Config-rule half of the catalog) and 20261113T140928Z-controlcatalog-taxonomy-and-control-text-are-emulator-authored.md. All ARNs, names and timestamps quoted above are the recording's values filling the ControlSummary template; the requirement is that the catalog's own values are served.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, 1408 steps, 13 passed, 1395 mismatched, 89 divergence clusters. All ten clusters: service controlcatalog, operation ListControls, fixture controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol, verdict mismatch, expected_status 200, actual_status 200, stub_path empty, owned_by empty.

Cause 1 (#task#70): $.Controls[Arn="arn:aws:controlcatalog:::control/624yg0j9d8swiglwfc1m4kvnm"] missing (expected CreateTime 1669593600.0, Description 'This AWS control checks whether CloudTrail log file validation is enabled.'); $.Controls[Arn="arn:aws:controlcatalog:::control/3b0a9d6550360b808fc7812ff"] extra (CreateTime 1555891200, Description '...and reports the finding in Security Hub.').
Cause 2 (#task#106): expected control/9r3or4f2x3aq1jrnma6ddfp2y (Aliases ['SH.CONFIG.DT.1'], Severity 'CRITICAL', PRS 'OPTIONAL', CreateTime 1779840000.0) missing; extra control/578ae099bc64665af00238723 (Aliases ['SH.Config.1'], Severity 'MEDIUM', PRS 'NONE', CreateTime 1572998400).
Cause 3 (#task#163): expected control/dowsaumj3n874siszx3j94m9n (Severity 'HIGH') missing; extra control/730c27ce58a5899cfb4db3698 (Severity 'MEDIUM', CreateTime 1664064000).
Cause 4 (#task#226): expected control/5cksj6jdycadjupdbviikn1w7 missing; extra control/58a14d983e8acb84121f29d72 (CreateTime 1598400000, altered Name/Description).
Cause 5 (#task#297): expected control/62smpoz33dsy0oa7u1iwa58lz missing; extra control/e150d5128aedec3b1127011ca (CreateTime 1700092800).
Cause 6 (#task#310): expected control/a7ertnvgba7449o25l6s75nrj missing; extra control/6b6dbeb85f8a005d07f3d8c11 (CreateTime 1688860800, paraphrased Description).
Cause 7 (#task#330): expected control/bxh5ltpzbcr59zexx3llhg3l9 ('GovernedResources': ['AWS::IAM::User']) missing; extra control/951c1bf7a40814be706e81983 ('GovernedResources': ['AWS::IAM::AccessKey'], CreateTime 1535328000).
Cause 8 (#task#365): expected control/esv514s4zvnxdunuijbgerpn3 (CreateTime 1701043200.0) missing; extra control/aa44c0d8db872bf143e815e51 (CreateTime 1565481600).
Cause 9 (#task#431): expected control/bbns1svmi60sz11i658ot4amf missing; extra control/561595b80b643a6c71c4d24aa (CreateTime 1589414400).
Cause 10 (#task#442): expected control/dsrtfqf4g9gvlguj5hqahozla missing; extra control/a19b7ded8d8bacf2f99d5f287 (CreateTime 1548806400).

No status or error-code divergence in any cause: all return the modeled 200 and the defect is the content of the @required Controls list.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.
2. Replay fixture controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol at step task#70 — POST /list-controls filtered to AWS::SecurityHub::SecurityControl. The page omits control/624yg0j9d8swiglwfc1m4kvnm and carries an emulator-minted ARN for CloudTrail.4 with a rewritten Description and a different CreateTime.
3. Repeat for the other nine representatives: task#106, task#163, task#226, task#297, task#310, task#330, task#365, task#431, task#442 — each catalog control is absent and a minted ARN appears, with the alias, Severity, ParameterRequirementSummary, GovernedResources or Description divergences named per cause.
4. Confirm the fix by re-replaying ALL TEN representatives: each Security Hub control must appear under the catalog's own ARN with the catalog's alias, name, description, severity, parameter-requirement summary, create time and governed resources.
