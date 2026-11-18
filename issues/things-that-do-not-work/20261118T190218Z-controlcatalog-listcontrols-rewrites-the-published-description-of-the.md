Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol
Issue-Step: task#494
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/alm61aw5oxpgfgh3bhmish87u"].Description
Issue-Commit: b7ec6bdb210b3057151ab40d1bb783153679c3f3

# controlcatalog ListControls rewrites the published Description of the S3 account-level block-public-access control

> _Note: this issue was identified against the code base at commit `b7ec6bdb210b3057151ab40d1bb783153679c3f3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
ListControls answers 200 and the control record's identity members (Arn, Name, Implementation) match, but ControlSummary$Description for the Security Hub control arn:aws:controlcatalog:::control/alm61aw5oxpgfgh3bhmish87u (S3 account-level block public access) is an emulator-authored paraphrase instead of the catalog's published sentence. One cause.

Cause 1 of 1 - control arn:aws:controlcatalog:::control/alm61aw5oxpgfgh3bhmish87u, member $.Controls[Arn="arn:aws:controlcatalog:::control/alm61aw5oxpgfgh3bhmish87u"].Description.

AWS: Smithy (com.amazonaws.controlcatalog): ListControls -> ListControlsResponse$Controls : list<ControlSummary>; ControlSummary$Description is a @required String documented as "A description of the control, as it may appear in the console. Describes the functionality of the control." The description is AWS-published catalog content keyed by the control ARN - a caller never supplies it and a server must return the published sentence verbatim for that ARN, not a re-phrasing. The model states no wording rule beyond that, so the exact sentence is recorded behavior only: for this ARN the recorded 200 response carries "This control checks whether the preceding Amazon S3 block public access settings are configured at the account level for an S3 general purpose bucket. The control fails if one or more of the block public access settings are set to false." The whole string is fixed published text for this control ARN; nothing in it is a per-request or per-run value, so there is no variable part a run may legitimately differ on.
Emulator: serves 200 with the same Arn but the re-worded sentence "This control checks whether the following Amazon S3 block public access settings are configured at the account level: ignorePublicAcls, blockPublicPolicy, blockPublicAcls, and restrictPublicBuckets. The control fails if one or more of the account-level block public access settings is set to false." - "the preceding" becomes "the following" plus an enumerated setting list the published text does not contain, the "for an S3 general purpose bucket" scope clause is dropped, and the closing condition is re-worded ("account-level block public access settings is set to false").
Why: clients treat this field as published AWS content. Console and portal renderings, compliance-evidence exports, documentation diffing tools and tests that pin a control's published description all read text AWS never published, so an auditor comparing emulator output with the AWS control catalog sees unexplained content drift on this control. Because the paraphrase drops the "S3 general purpose bucket" scope clause and invents an explicit settings list, it also misstates what the control evaluates.
Fix: the controlcatalog crate's seeded catalog data behind the ListControls handler - the stored ControlSummary record for this AWS::SecurityHub::SecurityControl ARN must hold the published description string verbatim so the handler and serializer pass it through unchanged. This is a state/data-fidelity defect, not a serialization defect.
Traces: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol#task#494.

Relation to open issues: 20261116T001437Z-controlcatalog-listcontrols-rewrites-published-security-hub-control-description.md reports the same fault for seven OTHER Security Hub control ARNs (task#106/163/226/310/365/431/442), and 20261115T014344Z-controlcatalog-listcontrols-pages-omit-catalog-config-controls-and-rewrite.md covers the Config-rule controls; the replay reports no owner for this record, so it is net-new and filed here. A fixer should expect the published-text correction to be applied to the whole seeded Security Hub catalogue rather than this row alone.

## Evidence
Replay of fixture controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol (trace set simparity-wire-aws-v1), step task#494: expected status 200, actual status 200, verdict mismatch, divergence key controlcatalog|ListControls|status=200->200|keys=$.Controls[Arn="arn:aws:controlcatalog:::control/alm61aw5oxpgfgh3bhmish87u"].Description.

$.Controls[Arn="arn:aws:controlcatalog:::control/alm61aw5oxpgfgh3bhmish87u"].Description: value_mismatch
  expected: 'This control checks whether the preceding Amazon S3 block public access settings are configured at the account level for an S3 general purpose bucket. The control fails if one or more of the block public access settings are set to false.'
  actual:   'This control checks whether the following Amazon S3 block public access settings are configured at the account level: ignorePublicAcls, blockPublicPolicy, blockPublicAcls, and restrictPublicBuckets. The control fails if one or more of the account-level block public access settings is set to false.'

All other members of the record and the HTTP status agree; only Description diverges. Model citation: Smithy controlcatalog ControlSummary$Description - @required, target String, "A description of the control, as it may appear in the console."; ControlSummary$Arn @required with pattern ^arn:(aws(?:[-a-z]*)?):(controlcatalog|controltower):[a-zA-Z0-9-]*::control/[0-9a-zA-Z_\-]+$.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay wire fixture controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol from trace set simparity-wire-aws-v1.
2. Page the listing to step task#494 (POST /list-controls with the fixture's AWS::SecurityHub::SecurityControl implementation filter and that step's NextToken).
3. In the 200 response locate the ControlSummary whose Arn is arn:aws:controlcatalog:::control/alm61aw5oxpgfgh3bhmish87u and compare its Description with the recorded response's Description for the same Arn.
4. The strings differ: the emulator enumerates the four block-public-access settings and drops the "for an S3 general purpose bucket" clause. Re-replay task#494 to confirm a fix.
