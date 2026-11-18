Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol
Issue-Step: task#494
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/alm61aw5oxpgfgh3bhmish87u"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/d74cde29acef7679566b39808"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/35hoe56okxxh2ujnsub92wkym"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/e11a33779b7bf3ef059ee9f31"]
Issue-Commit: ab88f803a4e379013b8a9f7f3c950c61c3772e52

# Security Hub S3 control records drop GovernedResources and rewrite Description

> _Note: this issue was identified against the code base at commit `ab88f803a4e379013b8a9f7f3c950c61c3772e52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
The two S3 entries of the Security Hub page of the catalog are served from emulator-authored records: the catalog's control ARN is absent, an emulator-minted ARN appears for the same Security Hub control identifier, the governed-resource list is reduced to a single type, and the Description is replaced by the emulator's own sentence. TWO divergence clusters are bundled here, numbered as causes below; BOTH must be closed and re-replayed before this issue is done.

Shared AWS basis (Smithy model services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json): ListControls is POST /list-controls, @readonly, success 200, paginated (NextToken/MaxResults @range 1..100, items Controls); ListControlsResponse.Controls is @required. ControlSummary.Arn is @required (@pattern ^arn:(aws(?:[-a-z]*)?):(controlcatalog|controltower):[a-zA-Z0-9-]*::control/[0-9a-zA-Z_\-]+$), Description and Name are @required, GovernedResources lists the resource types the control governs, Implementation is {Type, Identifier} and CreateTime is when the control was released as a governance capability. The catalog is an AWS-published, account-independent read-only dataset; the per-control values below are recorded behavior.

Shared Emulator basis: status matches (200); for both controls the catalog's ARN is reported missing and an emulator-minted ARN is reported extra under the same SH.S3.* alias.

Shared Why: the minted ARNs resolve nowhere in AWS, so any id a client harvests here is unusable against the real service; a GovernedResources list reduced to AWS::S3::Bucket hides the access-point, bucket-policy and multi-region-access-point types the control actually governs, so a client filtering or reporting by governed resource type under-reports coverage; and the rewritten Description loses the pass/fail rule the catalog states.

Shared Fix: the ControlCatalog control seed dataset read by crate::state and the ControlSummary projection in the ListControls handler (services/service-com-amazonaws-controlcatalog-controlcatalog/src/ops/list_controls.rs) — Arn, Description, GovernedResources and CreateTime must be read from the catalog record.

Cause 1 of 2 — alias SH.S3.1 / control S3.1 (step task#494). AWS: control/alm61aw5oxpgfgh3bhmish87u, GovernedResources ['AWS::S3::AccessPoint', 'AWS::S3::Bucket', 'AWS::S3::BucketPolicy', 'AWS::S3::MultiRegionAccessPoint', 'AWS::S3::MultiRegionAccessPointPolicy'], Description stating that the control fails if one or more of the account-level block-public-access settings is false, CreateTime 1669593600. Emulator: that entry is absent; a minted ARN appears with GovernedResources ['AWS::S3::Bucket'] only, a bucket-level paraphrase of the Description and CreateTime 1693353600.

Cause 2 of 2 — alias SH.S3.5 / control S3.5 (step task#510). AWS: control/35hoe56okxxh2ujnsub92wkym, Description "This control checks whether an Amazon S3 general purpose bucket has a policy that requires requests to use SSL. The control fails if the bucket policy doesn't require requests to use SSL.", CreateTime 1669593600. Emulator: absent; a minted ARN appears with the emulator's own '...and reports the finding in Security Hub' Description and CreateTime 1587772800.

Traces: controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol steps task#494 and task#510.

Related open issue which does NOT carry either cluster's replay key (owned_by empty and stub_path empty for both): 20261113T140928Z-controlcatalog-taxonomy-and-control-text-are-emulator-authored.md describes the same emulator-authored-dataset root cause. All ARNs, names and timestamps quoted above are the recording's values filling the ControlSummary template; the requirement is that the catalog's own values are served.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, 1408 steps, 13 passed, 1395 mismatched, 89 divergence clusters. Both clusters: service controlcatalog, operation ListControls, fixture controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol, verdict mismatch, expected_status 200, actual_status 200, stub_path empty, owned_by empty.

Cause 1 (#task#494): $.Controls[Arn="arn:aws:controlcatalog:::control/alm61aw5oxpgfgh3bhmish87u"] missing (expected Aliases ['SH.S3.1'], GovernedResources ['AWS::S3::AccessPoint', 'AWS::S3::Bucket', 'AWS::S3::BucketPolicy', 'AWS::S3::MultiRegionAccessPoint', 'AWS::S3::MultiRegionAccessPointPolicy'], CreateTime 1669593600.0, Implementation {'Identifier': 'S3.1', 'Type': 'AWS::SecurityHub::SecurityControl'}); $.Controls[Arn="arn:aws:controlcatalog:::control/d74cde29acef7679566b39808"] extra (GovernedResources ['AWS::S3::Bucket'], CreateTime 1693353600, rewritten Description).

Cause 2 (#task#510): $.Controls[Arn="arn:aws:controlcatalog:::control/35hoe56okxxh2ujnsub92wkym"] missing (expected Aliases ['SH.S3.5'], CreateTime 1669593600.0, Description "This control checks whether an Amazon S3 general purpose bucket has a policy that requires requests to use SSL. The control fails if the bucket policy doesn't require requests to use SSL."); $.Controls[Arn="arn:aws:controlcatalog:::control/e11a33779b7bf3ef059ee9f31"] extra (CreateTime 1587772800, Description 'Checks whether an Amazon S3 general purpose bucket has a policy that requires requests to use SSL, and reports the finding in Security Hub.').

No status or error-code divergence in either cause: both return the modeled 200.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.
2. Replay fixture controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol at step task#494 — POST /list-controls filtered to AWS::SecurityHub::SecurityControl. The page omits control/alm61aw5oxpgfgh3bhmish87u and carries a minted ARN for S3.1 whose GovernedResources lists only AWS::S3::Bucket.
3. Replay step task#510 of the same fixture — control/35hoe56okxxh2ujnsub92wkym is absent and a minted ARN for S3.5 appears with a rewritten Description and CreateTime 1587772800.
4. Confirm the fix by re-replaying BOTH representatives (task#494 and task#510): each must appear under the catalog's own ARN with the catalog's full GovernedResources list, Description and CreateTime.
